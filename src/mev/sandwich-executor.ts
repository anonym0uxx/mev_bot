/**
 * @module mev/sandwich-executor
 * SandwichExecutor: builds and submits Jito bundles for sandwich attacks.
 *
 * Bundle structure: [our_buy_tx, victim_tx (AS-IS raw bytes), our_sell_tx, tip_tx]
 * Bundle atomicity: if victim's tx reverts, entire bundle fails — zero capital risk.
 *
 * Paper mode: logs simulation details, returns fake bundleId. No real txs built.
 * Live mode: builds real txs, includes raw victim bytes, submits via jito-ts gRPC.
 */

import { Connection, Keypair, PublicKey, VersionedTransaction } from '@solana/web3.js';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import type { MevConfig } from '../types/config';
import type { PumpTxBuilder } from './pump-tx-builder';
// JitoGuard is added in Phase 1 (ShredStream build). Interface mirrored here for pre-compilation.
interface JitoGuard {
  canSubmit(mint: string, sizeSol: number, tipLamports: number): { allowed: boolean; reason?: string };
  recordOutcome(bundleId: string, success: boolean): void;
  applyTipNoise(baseTip: number): number;
  onGRPCError(): void;
  onGRPCSuccess(): void;
}
import type { WalletRotator } from './wallet-rotator';
import type { SandwichSignal } from '../strategies/SandwichDetector';

const log = createLogger('mev:sandwich-executor');

const JITO_TIP_ACCOUNTS = [
  '96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5',
  'HFqU5x63VTqvB6pQMT9zDQGGPgNDZ5Jznh5GbMggbUfU',
  'Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY',
  'ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1uw1nbn2uK6',
  'DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh',
  'ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt',
  'DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL',
  '3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT',
] as const;

const JITO_BLOCK_ENGINE_URL = 'mainnet.block-engine.jito.wtf:443';

export interface SandwichResult {
  success: boolean;
  bundleId?: string;
  error?: string;
  tipLamports: number;
  netProfitEstSol: number;
  latencyMs: number;
}

export class SandwichExecutor {
  private readonly cfg: MevConfig;
  private readonly txBuilder: PumpTxBuilder;
  private readonly guard: JitoGuard;
  private readonly rotator: WalletRotator;

  public totalAttempts: number = 0;
  public totalSuccess: number = 0;
  public totalFailed: number = 0;

  constructor(
    cfg: MevConfig,
    txBuilder: PumpTxBuilder,
    guard: JitoGuard,
    rotator: WalletRotator,
  ) {
    this.cfg = cfg;
    this.txBuilder = txBuilder;
    this.guard = guard;
    this.rotator = rotator;
  }

  getStats(): { totalAttempts: number; totalSuccess: number; totalFailed: number } {
    return {
      totalAttempts: this.totalAttempts,
      totalSuccess: this.totalSuccess,
      totalFailed: this.totalFailed,
    };
  }

  async execute(signal: SandwichSignal, connection: Connection): Promise<SandwichResult> {
    this.totalAttempts++;

    // ── Paper mode ──────────────────────────────────────────────────────
    if (this.cfg.paper_mode === true) {
      log.info(
        '[PAPER] sandwich bundle | mint=%s victim=%.4f SOL pos=%.4f SOL estNet=%.6f SOL tip=%d lamports',
        signal.mint.slice(0, 8),
        signal.victimSolAmount,
        signal.ourPositionSol,
        signal.estimatedNetProfitSol,
        signal.tipLamports,
      );
      this.totalSuccess++;
      return {
        success: true,
        bundleId: 'sandwich-paper-' + Date.now(),
        tipLamports: signal.tipLamports,
        netProfitEstSol: signal.estimatedNetProfitSol,
        latencyMs: 0,
      };
    }

    // ── Live mode ───────────────────────────────────────────────────────
    const t0 = nowMs();
    try {
      // Guard check
      const guardCheck = this.guard.canSubmit(
        signal.mint,
        signal.ourPositionSol,
        signal.tipLamports,
      );
      if (!guardCheck.allowed) {
        this.totalFailed++;
        log.warn('guard rejected sandwich: %s', guardCheck.reason);
        return {
          success: false,
          error: `guard rejected: ${guardCheck.reason}`,
          tipLamports: signal.tipLamports,
          netProfitEstSol: signal.estimatedNetProfitSol,
          latencyMs: nowMs() - t0,
        };
      }

      // Wallet
      const wallet: Keypair | null = this.rotator.next();
      if (wallet === null) {
        this.totalFailed++;
        log.error('no wallet available for sandwich');
        return {
          success: false,
          error: 'no wallet available',
          tipLamports: signal.tipLamports,
          netProfitEstSol: signal.estimatedNetProfitSol,
          latencyMs: nowMs() - t0,
        };
      }

      // Blockhash
      const { blockhash } = await connection.getLatestBlockhash('confirmed');

      // Apply tip noise for anti-detection
      const noisyTip = this.guard.applyTipNoise(signal.tipLamports);

      // Build our buy tx (front-run)
      const mintPk = new PublicKey(signal.mint);
      const bondingCurvePk = new PublicKey(signal.bondingCurveKey);

      const buyTx = await this.txBuilder.buildBuyTx({
        mint: mintPk,
        bondingCurve: bondingCurvePk,
        associatedBondingCurve: bondingCurvePk,
        buyer: wallet,
        solAmountLamports: signal.ourPositionLamports,
        minTokensOut: (signal.tokensWeReceive * 99n) / 100n,
        priorityFeeMicroLamports: 50_000,
        recentBlockhash: blockhash,
      });

      // Deserialize victim tx AS-IS — do NOT re-sign
      const victimTx = VersionedTransaction.deserialize(signal.victimTxBytes);

      // Build our sell tx (back-run)
      const sellTx = await this.txBuilder.buildSellTx({
        mint: mintPk,
        bondingCurve: bondingCurvePk,
        associatedBondingCurve: bondingCurvePk,
        seller: wallet,
        tokenAmount: signal.tokensWeReceive,
        minSolOut: (signal.ourSellProceedsLamports * 98n) / 100n,
        priorityFeeMicroLamports: 50_000,
        recentBlockhash: blockhash,
      });

      // Build Jito bundle
      const { searcherClient } = require('jito-ts/dist/sdk/block-engine/searcher');
      const { Bundle } = require('jito-ts/dist/sdk/block-engine/types');
      const { isError } = require('jito-ts/dist/sdk/block-engine/utils');

      const client = searcherClient(JITO_BLOCK_ENGINE_URL, undefined, {
        'grpc.keepalive_time_ms': 10_000,
      });

      const tipAccount = new PublicKey(
        JITO_TIP_ACCOUNTS[Math.floor(Math.random() * JITO_TIP_ACCOUNTS.length)],
      );

      const bundle = new Bundle([], 5);

      const withTrades = bundle.addTransactions(buyTx, victimTx, sellTx);
      if (isError(withTrades)) {
        this.totalFailed++;
        log.error('bundle addTransactions failed: %s', withTrades.message);
        return {
          success: false,
          error: withTrades.message,
          tipLamports: noisyTip,
          netProfitEstSol: signal.estimatedNetProfitSol,
          latencyMs: nowMs() - t0,
        };
      }

      const withTip = withTrades.addTipTx(wallet, noisyTip, tipAccount, blockhash);
      if (isError(withTip)) {
        this.totalFailed++;
        log.error('bundle addTipTx failed: %s', withTip.message);
        return {
          success: false,
          error: withTip.message,
          tipLamports: noisyTip,
          netProfitEstSol: signal.estimatedNetProfitSol,
          latencyMs: nowMs() - t0,
        };
      }

      // Submit bundle
      const result = await client.sendBundle(withTip);

      if (!result.ok) {
        this.totalFailed++;
        this.guard.onGRPCError();
        const errMsg = result.error?.message ?? 'unknown jito error';
        log.error('jito sendBundle failed: %s', errMsg);
        this.guard.recordOutcome('', false);
        return {
          success: false,
          error: errMsg,
          tipLamports: noisyTip,
          netProfitEstSol: signal.estimatedNetProfitSol,
          latencyMs: nowMs() - t0,
        };
      }

      // Success
      const bundleId: string = result.value;
      this.totalSuccess++;
      this.guard.onGRPCSuccess();
      this.guard.recordOutcome(bundleId, true);

      const latency = nowMs() - t0;
      log.info(
        'sandwich bundle submitted | id=%s mint=%s pos=%.4f SOL tip=%d net=%.6f SOL latency=%dms',
        bundleId,
        signal.mint.slice(0, 8),
        signal.ourPositionSol,
        noisyTip,
        signal.estimatedNetProfitSol,
        latency,
      );

      return {
        success: true,
        bundleId,
        tipLamports: noisyTip,
        netProfitEstSol: signal.estimatedNetProfitSol,
        latencyMs: latency,
      };
    } catch (err: unknown) {
      this.totalFailed++;
      const errMsg = err instanceof Error ? err.message : String(err);
      log.error('sandwich execute caught exception: %s', errMsg);
      this.guard.onGRPCError();
      return {
        success: false,
        error: errMsg,
        tipLamports: signal.tipLamports,
        netProfitEstSol: signal.estimatedNetProfitSol,
        latencyMs: nowMs() - t0,
      };
    }
  }
}
