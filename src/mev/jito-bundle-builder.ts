/**
 * @module mev/jito-bundle-builder
 * JitoBundleBuilder: constructs and submits Jito MEV bundles for entry transactions.
 *
 * In paper mode: logs what WOULD be submitted without touching the chain.
 * In live mode: stub that logs a Phase 3 warning (full pump.fun IDL integration pending).
 *
 * Tip calculation: max(cfg.jito_tip_lamports, floor(expectedProfit * 0.5))
 * Tip account: randomly selected from JITO_TIP_ACCOUNTS on each bundle.
 *
 * See src/execution/jito.ts for the lower-level gRPC bundle submission.
 * This class sits above that layer and adds MEV-aware tip sizing and logging.
 */

import { PublicKey } from '@solana/web3.js';
import { createLogger } from '../utils/logger';
import { MevConfig } from '../types/config';

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
}

export interface BundleResult {
  bundleId: string;
  paperMode: boolean;
  tipLamports?: number;
  tipAccount?: string;
}

export class JitoBundleBuilder {
  private cfg: MevConfig;

  constructor(cfg: MevConfig) {
    this.cfg = cfg;
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
   * Live mode: stub — returns warning that full integration is in Phase 3.
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

    // Live mode: actual bundle construction requires pump.fun IDL (Phase 3)
    log.warn(
      `[live] Jito bundle construction for ${params.mint.slice(0, 8)}… — ` +
      `full tx building requires pump.fun IDL integration (Phase 3)`
    );
    return {
      bundleId: `stub-${Date.now()}`,
      paperMode: false,
      tipLamports,
      tipAccount: tipAccount.toBase58(),
    };
  }
}
