/**
 * @module mev/sell-executor
 * SellExecutor: handles exit transaction dispatch for closed MEV positions.
 *
 * Paper mode: simulates the sell and logs estimated PnL.
 * Live mode: submits via Helius staked RPC for lower slot-skip rate.
 *            Full transaction construction requires pump.fun IDL (Phase 3).
 *
 * Helius staked URL resolution order:
 *   1. SOLANA_STAKED_URL env var   (Helius staked connection dedicated endpoint)
 *   2. SOLANA_RPC_URL env var      (standard RPC fallback)
 *   3. mainnet-beta public RPC     (last resort, not recommended for live trading)
 */

import { createLogger } from '../utils/logger';
import { MevConfig } from '../types/config';
import { PnLRecord } from './position-manager';

const log = createLogger('mev:sell-executor');

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

  constructor(cfg: MevConfig) {
    this.cfg = cfg;
    this.heliusStakedUrl =
      process.env.SOLANA_STAKED_URL ||
      process.env.SOLANA_RPC_URL ||
      'https://api.mainnet-beta.solana.com';
  }

  /**
   * Execute a sell for a closed position.
   *
   * Paper mode: simulates and returns estimated received SOL from PnLRecord.
   * Live mode: stub — logs Phase 3 warning, returns failure result.
   */
  async executeSell(record: PnLRecord): Promise<SellResult> {
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

    // Live mode: full tx construction requires pump.fun IDL (Phase 3)
    log.warn(
      `[live] Sell executor for ${record.mint.slice(0, 8)}… — ` +
      `full tx requires pump.fun IDL integration (Phase 3). ` +
      `Would use staked RPC: ${this.heliusStakedUrl.replace(/api-key=[^&]+/, 'api-key=***')}`
    );
    return {
      success: false,
      paperMode: false,
      error: 'Live sell not yet implemented — requires pump.fun IDL (Phase 3)',
    };
  }

  /**
   * Get the Helius staked RPC URL being used.
   * Useful for health checks and logging.
   */
  getStakedUrl(): string {
    return this.heliusStakedUrl;
  }
}
