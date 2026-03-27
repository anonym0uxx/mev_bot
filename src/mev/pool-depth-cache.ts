/**
 * @module mev/pool-depth-cache
 * In-memory cache for Raydium/AMM pool depth data sourced from Bitquery DexPools stream.
 *
 * Used to:
 *   1. Gate backrun entries: skip tokens that have migrated to Raydium but lack pool depth.
 *   2. Force-exit positions when LP is being removed (rug detection).
 */

import { createLogger } from '../utils/logger';

const log = createLogger('mev:pool-depth-cache');

export interface PoolUpdate {
  mint: string;
  poolAddress: string;
  depthSol: number;    // current total SOL depth in pool
  changeSol: number;   // delta (positive = add, negative = remove)
  isRemoval: boolean;  // true if LP is being removed
  timestamp: number;
}

export class PoolDepthCache {
  private depths = new Map<string, number>(); // mint → SOL depth

  update(mint: string, depthSol: number): void {
    this.depths.set(mint, depthSol);
  }

  getDepth(mint: string): number {
    return this.depths.get(mint) ?? 0;
  }

  isDeep(mint: string, minDepthSol = 5): boolean {
    const d = this.depths.get(mint);
    return d !== undefined && d >= minDepthSol;
  }

  /** True if this mint has migrated to Raydium (any depth recorded) */
  hasMigrated(mint: string): boolean {
    return this.depths.has(mint);
  }

  clear(mint: string): void {
    this.depths.delete(mint);
    log.debug(`Pool depth cleared for ${mint.slice(0, 8)}`);
  }

  get size(): number {
    return this.depths.size;
  }
}
