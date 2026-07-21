/**
 * @module mev/jito-bundle-builder
 * JitoBundleBuilder: constructs and submits Jito MEV bundles for entry transactions.
 *
 * In paper mode: logs what WOULD be submitted without touching the chain.
 * In live mode: builds real Pump.fun VersionedTransactions via PumpTxBuilder,
 *               appends a Jito tip transfer, and submits via jito.ts submitBundle().
 *
 * Tip calculation: max(cfg.jito_tip_lamports, floor(expectedProfit * 0.5))
 * Tip account: randomly selected from JITO_TIP_ACCOUNTS on each bundle.
 *
 * See src/execution/jito.ts for the lower-level gRPC bundle submission.
 * This class sits above that layer and adds MEV-aware tip sizing and logging.
 */

import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  TransactionMessage,
  VersionedTransaction,
  LAMPORTS_PER_SOL,
} from '@solana/web3.js';
import { createLogger } from '../utils/logger';
import { MevConfig, PumpQuantConfig } from '../types/config';
import { PumpTxBuilder } from './pump-tx-builder';
import { BondingCurveSimulator } from './bonding-curve-sim';
import { selectTipAccount, submitBundle } from '../execution/jito';

const log = createLogger('mev:jito-bundle-builder');

// Jito tip accounts (rotate through these to distribute tips)
const JITO_TIP_ACCOUNTS: readonly string[] = [
  '96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5',
  'HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe',
  'Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY',
  'ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1GyZRMiPBrN',
  '3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT',
] as const;

export interface BundleParams {
  mint: string;
  sizeSol: number;
  tipLamports: number;
  paperMode: boolean;
  // Live mode required fields
  bondingCurve: string;
  associatedBondingCurve: string;
  buyerKeypair?: Keypair; // required for live mode
  vSolLamports: bigint;
  vTokens: bigint;
}

export interface BundleResult {
  bundleId: string;
  paperMode: boolean;
  tipLamports?: number;
  tipAccount?: string;
  error?: string;
}

export class JitoBundleBuilder {
  private cfg: MevConfig;
  private sim: BondingCurveSimulator;

  constructor(cfg: MevConfig) {
    this.cfg = cfg;
    this.sim = new BondingCurveSimulator();
  }

  /**
   * Select a random Jito tip account.
   * Uniform distribution — avoids routing all tips through one account.
   */
  selectTipAccount(): PublicKey {
    const idx = Math.floor(Math.random() * JITO_TIP_ACCOUNTS.length);
    return new PublicKey(JITO_TIP_ACCOUNTS[idx]);
  }

  /**
   * Compute tip amount in lamports.
   * Tip = max(cfg.jito_tip_lamports, floor(expectedProfitLamports * 0.5))
   * Ensures we always pay at least the configured minimum tip.
   */
  computeTip(expectedProfitLamports: number): number {
    const halfProfit = Math.floor(expectedProfitLamports * 0.5);
    return Math.max(this.cfg.jito_tip_lamports ?? 10_000, halfProfit);
  }

  /**
   * Build and submit a Jito bundle for a MEV entry.
   *
   * Paper mode: logs simulation details, returns a fake bundleId.
   * Live mode: builds real buy tx via PumpTxBuilder, builds tip tx via SystemProgram.transfer,
   *            submits bundle via submitBundle() from execution/jito.ts.
   */
  async buildBundle(params: BundleParams): Promise<BundleResult> {
    const tipAccount = this.selectTipAccount();
    const tipLamports = this.computeTip(params.sizeSol * 1_000); // rough lamport estimate

    if (params.paperMode) {
      log.info(
        `[paper] Would submit Jito bundle: ` +
        `buy ${params.sizeSol.toFixed(4)} SOL of ${params.mint.slice(0, 8)}… ` +
        `tip=${tipLamports} lamports → tipAccount=${tipAccount.toBase58().slice(0, 8)}…`
      );
      return {
        bundleId: `paper-${Date.now()}`,
        paperMode: true,
        tipLamports,
        tipAccount: tipAccount.toBase58(),
      };
    }

    // Live mode: build and submit real transactions
    if (!params.buyerKeypair) {
      const err = 'Live bundle requires buyerKeypair';
      log.error(err);
      return { bundleId: '', paperMode: false, error: err };
    }

    try {
      const rpcUrl = process.env.SOLANA_RPC_URL || 'https://api.mainnet-beta.solana.com';
      const connection = new Connection(rpcUrl, 'confirmed');

      // Fetch fresh blockhash
      const { blockhash } = await connection.getLatestBlockhash('confirmed');

      const txBuilder = new PumpTxBuilder(connection);
      const solAmountLamports = BigInt(Math.floor(params.sizeSol * Number(LAMPORTS_PER_SOL)));

      // Simulate buy to estimate minTokensOut with 1% slippage
      const simResult = this.sim.simulateBuy(
        params.vSolLamports,
        params.vTokens,
        solAmountLamports,
        100n // 1% slippage
      );

      const priorityFeeMicroLamports = Math.floor(
        (this.cfg.jito_tip_lamports ?? 10_000) * 100
      );

      // Build buy tx via PumpTxBuilder
      const buyTx = await txBuilder.buildBuyTx({
        mint: new PublicKey(params.mint),
        bondingCurve: new PublicKey(params.bondingCurve),
        associatedBondingCurve: new PublicKey(params.associatedBondingCurve),
        buyer: params.buyerKeypair,
        solAmountLamports,
        minTokensOut: simResult.minTokensOut,
        priorityFeeMicroLamports,
        recentBlockhash: blockhash,
      });

      // Build tip tx: SystemProgram.transfer to random tip account
      const finalTipAccount = selectTipAccount();
      const tipMsg = new TransactionMessage({
        payerKey: params.buyerKeypair.publicKey,
        recentBlockhash: blockhash,
        instructions: [
          SystemProgram.transfer({
            fromPubkey: params.buyerKeypair.publicKey,
            toPubkey: finalTipAccount,
            lamports: tipLamports,
          }),
        ],
      }).compileToV0Message();
      const tipTx = new VersionedTransaction(tipMsg);
      tipTx.sign([params.buyerKeypair]);

      // Build a minimal PumpQuantConfig wrapper for submitBundle
      // submitBundle expects a PumpQuantConfig with execution.jito_tip_lamports
      const configWrapper = {
        execution: {
          jito_tip_lamports: tipLamports,
          jito_enabled: true,
          bundle_route: { enabled: true },
        },
      } as unknown as PumpQuantConfig;

      log.info(
        `[live] Submitting Jito bundle: ` +
        `buy ${params.sizeSol.toFixed(4)} SOL of ${params.mint.slice(0, 8)}… ` +
        `tip=${tipLamports} lamports → ${finalTipAccount.toBase58().slice(0, 8)}…`
      );

      // submitBundle() in jito.ts accepts legacy Transactions, but we have VersionedTransactions.
      // We need to use the Bundle API directly. Since submitBundle expects legacy Txs,
      // we replicate the bundle submission here with pre-built VersionedTransactions.
      const { Bundle } = require('jito-ts/dist/sdk/block-engine/types');
      const { isError } = require('jito-ts/dist/sdk/block-engine/utils');
      const { searcherClient } = require('jito-ts/dist/sdk/block-engine/searcher');

      const JITO_BLOCK_ENGINE_URL = 'mainnet.block-engine.jito.wtf:443';
      const client = searcherClient(JITO_BLOCK_ENGINE_URL, undefined, {
        'grpc.keepalive_time_ms': 10_000,
        'grpc.keepalive_timeout_ms': 5_000,
        'grpc.keepalive_permit_without_calls': 1,
      });

      const bundle = new Bundle([], 5);
      const withTrades = bundle.addTransactions(buyTx);
      if (isError(withTrades)) {
        const errMsg = (withTrades as Error).message;
        log.warn(`Bundle.addTransactions failed: ${errMsg}`);
        return { bundleId: '', paperMode: false, error: errMsg };
      }

      // Use SDK's addTipTx for the tip (it builds + signs the tip tx internally)
      const withTip = withTrades.addTipTx(
        params.buyerKeypair,
        tipLamports,
        finalTipAccount,
        blockhash
      );
      if (isError(withTip)) {
        const errMsg = (withTip as Error).message;
        log.warn(`Bundle.addTipTx failed: ${errMsg}`);
        return { bundleId: '', paperMode: false, error: errMsg };
      }

      const result = await client.sendBundle(withTip);
      if (!result.ok) {
        const err = result.error;
        log.warn(`Jito sendBundle failed: code=${err.code} msg=${err.message}`);
        return {
          bundleId: '',
          paperMode: false,
          tipLamports,
          tipAccount: finalTipAccount.toBase58(),
          error: `gRPC ${err.code}: ${err.message}`,
        };
      }

      const bundleId = result.value;
      log.info(`[live] Jito bundle submitted ✓ id=${bundleId} tip=${tipLamports}L`);
      return {
        bundleId,
        paperMode: false,
        tipLamports,
        tipAccount: finalTipAccount.toBase58(),
      };

    } catch (err) {
      const msg = (err as Error).message ?? String(err);
      log.warn(`[live] Jito bundle exception: ${msg}`);
      return { bundleId: '', paperMode: false, error: msg };
    }
  }
}
