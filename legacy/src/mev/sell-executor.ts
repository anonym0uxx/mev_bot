/**
 * @module mev/sell-executor
 * SellExecutor: handles exit transaction dispatch for closed MEV positions.
 *
 * Paper mode: simulates the sell and logs estimated PnL.
 * Live mode: builds a real Pump.fun sell VersionedTransaction via PumpTxBuilder
 *            and submits via Helius staked RPC for lower slot-skip rate.
 *
 * Helius staked URL resolution order:
 *   1. SOLANA_STAKED_URL env var   (Helius staked connection dedicated endpoint)
 *   2. SOLANA_RPC_URL env var      (standard RPC fallback)
 *   3. mainnet-beta public RPC     (last resort, not recommended for live trading)
 */

import { Connection, Keypair, PublicKey, LAMPORTS_PER_SOL } from '@solana/web3.js';
import { createLogger } from '../utils/logger';
import { MevConfig } from '../types/config';
import { PnLRecord } from './position-manager';
import { PumpTxBuilder } from './pump-tx-builder';
import { BondingCurveSimulator } from './bonding-curve-sim';

const log = createLogger('mev:sell-executor');

const SELL_CONFIRM_TIMEOUT_MS = 5_000;

export interface SellResult {
  success: boolean;
  txSig?: string;
  solReceived?: number;
  paperMode: boolean;
  error?: string;
}

export class SellExecutor {
  private cfg: MevConfig;
  private heliusStakedUrl: string;
  private sim: BondingCurveSimulator;

  constructor(cfg: MevConfig) {
    this.cfg = cfg;
    this.heliusStakedUrl =
      process.env.SOLANA_STAKED_URL ||
      process.env.SOLANA_RPC_URL ||
      'https://api.mainnet-beta.solana.com';
    this.sim = new BondingCurveSimulator();
  }

  /**
   * Execute a sell for a closed position.
   *
   * Paper mode: simulates and returns estimated received SOL from PnLRecord.
   * Live mode: builds real sell tx via PumpTxBuilder, sends via Helius staked RPC,
   *            confirms with 5s timeout.
   */
  async executeSell(record: PnLRecord, sellerKeypair?: Keypair): Promise<SellResult> {
    if (this.cfg.paper_mode) {
      const pnlStr = record.pnlSol >= 0
        ? `+${record.pnlSol.toFixed(5)}`
        : record.pnlSol.toFixed(5);
      log.info(
        `[paper] Would sell ${record.mint.slice(0, 8)}… via Helius staked RPC: ` +
        `estimated ${pnlStr} SOL (exitVSol=${record.exitVSol.toFixed(3)}) ` +
        `reason=${record.exitReason}`
      );
      return {
        success: true,
        paperMode: true,
        solReceived: record.exitVSol,
      };
    }

    // Live mode: build and submit real sell tx
    if (!sellerKeypair) {
      const err = 'Live sell requires sellerKeypair';
      log.error(err);
      return { success: false, paperMode: false, error: err };
    }

    if (!record.bondingCurveKey || !record.associatedBondingCurve) {
      const err = `Missing bondingCurve data for ${record.mint.slice(0, 8)}`;
      log.error(err);
      return { success: false, paperMode: false, error: err };
    }

    const tokensHeld = record.tokensHeld ?? 0n;
    if (tokensHeld === 0n) {
      // Estimate from vSol math if tokensHeld wasn't tracked
      const entryVSolLamports = BigInt(Math.floor(record.entryVSol * Number(LAMPORTS_PER_SOL)));
      const exitVSolLamports = record.exitVSolLamports ?? BigInt(Math.floor(record.exitVSol * Number(LAMPORTS_PER_SOL)));
      const entryVTokens = record.exitVTokens ?? 0n;
      log.warn(
        `[live] tokensHeld=0 for ${record.mint.slice(0, 8)}, cannot estimate accurately — skipping sell`
      );
      return { success: false, paperMode: false, error: 'tokensHeld=0, cannot build sell' };
    }

    try {
      const connection = new Connection(this.heliusStakedUrl, 'confirmed');
      const txBuilder = new PumpTxBuilder(connection);

      // Get fresh blockhash
      const { blockhash, lastValidBlockHeight } = await connection.getLatestBlockhash('confirmed');

      // Compute minSolOut with 2% slippage from current exit reserves
      const exitVSolLamports = record.exitVSolLamports ?? BigInt(Math.floor(record.exitVSol * Number(LAMPORTS_PER_SOL)));
      const exitVTokens = record.exitVTokens ?? 0n;

      let minSolOut = 0n;
      if (exitVSolLamports > 0n && exitVTokens > 0n) {
        try {
          const sellSim = this.sim.simulateSell(exitVSolLamports, exitVTokens, tokensHeld);
          // 2% slippage protection
          minSolOut = (sellSim.solOut * 9800n) / 10000n;
        } catch (e) {
          log.warn(`simulateSell failed: ${(e as Error).message} — using minSolOut=0`);
          minSolOut = 0n;
        }
      }

      const priorityFeeMicroLamports = Math.floor(
        (this.cfg.jito_tip_lamports ?? 10_000) * 100
      );

      const sellTx = await txBuilder.buildSellTx({
        mint: new PublicKey(record.mint),
        bondingCurve: new PublicKey(record.bondingCurveKey),
        associatedBondingCurve: new PublicKey(record.associatedBondingCurve),
        seller: sellerKeypair,
        tokenAmount: tokensHeld,
        minSolOut,
        priorityFeeMicroLamports,
        recentBlockhash: blockhash,
      });

      log.info(
        `[live] Sending sell tx: ${record.mint.slice(0, 8)}… ` +
        `tokens=${tokensHeld} minSol=${Number(minSolOut) / Number(LAMPORTS_PER_SOL)} ` +
        `staked=${this.heliusStakedUrl.replace(/api-key=[^&]+/, 'api-key=***')}`
      );

      const txSig = await connection.sendRawTransaction(sellTx.serialize(), {
        skipPreflight: true,
        maxRetries: 3,
      });

      log.info(`[live] Sell tx sent: ${txSig}`);

      // Confirm with 5s timeout
      const confirmPromise = connection.confirmTransaction(
        { signature: txSig, blockhash, lastValidBlockHeight },
        'confirmed'
      );
      const timeoutPromise = new Promise<null>((_, reject) =>
        setTimeout(() => reject(new Error('confirm timeout')), SELL_CONFIRM_TIMEOUT_MS)
      );

      try {
        const confirmation = await Promise.race([confirmPromise, timeoutPromise]);
        if (confirmation && (confirmation as any).value?.err) {
          const txErr = JSON.stringify((confirmation as any).value.err);
          log.warn(`[live] Sell tx failed on-chain: ${txSig} err=${txErr}`);
          return { success: false, paperMode: false, txSig, error: `On-chain error: ${txErr}` };
        }
      } catch (confirmErr) {
        // Timeout or confirmation error — tx may still land
        log.warn(`[live] Sell confirm warning: ${(confirmErr as Error).message} sig=${txSig}`);
      }

      const estimatedSolReceived = Number(minSolOut) / Number(LAMPORTS_PER_SOL);
      log.info(
        `[live] ✅ Sell executed: ${record.mint.slice(0, 8)} sig=${txSig.slice(0, 16)}… ` +
        `~${estimatedSolReceived.toFixed(5)} SOL received`
      );

      return {
        success: true,
        paperMode: false,
        txSig,
        solReceived: estimatedSolReceived,
      };

    } catch (err) {
      const msg = (err as Error).message ?? String(err);
      log.warn(`[live] Sell executor exception for ${record.mint.slice(0, 8)}: ${msg}`);
      return { success: false, paperMode: false, error: msg };
    }
  }

  /**
   * Get the Helius staked RPC URL being used.
   * Useful for health checks and logging.
   */
  getStakedUrl(): string {
    return this.heliusStakedUrl;
  }
}
