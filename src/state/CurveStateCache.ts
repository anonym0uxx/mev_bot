/**
 * @module state/CurveStateCache
 * Local cache of Pump.fun bonding curve vSol/vTokens state per mint.
 * Updated from PumpPortal confirmed trade events.
 * Used by SandwichDetector to avoid slow RPC calls during sandwich evaluation.
 *
 * Staleness policy: entries older than MAX_STALENESS_SLOTS slots are treated
 * as stale and sandwich is skipped. Entries older than 60s are evicted.
 */

import { BondingCurveSimulator } from '../mev/bonding-curve-sim';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';

const log = createLogger('CurveStateCache');

const EVICTION_AGE_MS = 60_000;

export interface CurveState {
  vSolLamports: bigint;
  vTokens: bigint;
  lastSlot: number;
  lastUpdateMs: number;
}

export class CurveStateCache {
  private cache: Map<string, CurveState> = new Map();

  /**
   * Upsert curve state for a given mint.
   * Triggers stale entry eviction on every call.
   */
  update(mint: string, vSolLamports: bigint, vTokens: bigint, slot: number): void {
    this.evictStale();

    const existing = this.cache.get(mint);

    // Only update if incoming slot is >= existing slot (don't go backwards)
    if (existing && existing.lastSlot > slot) {
      log.debug(`Skipping stale update for ${mint}: cached slot ${existing.lastSlot} > incoming ${slot}`);
      return;
    }

    this.cache.set(mint, {
      vSolLamports,
      vTokens,
      lastSlot: slot,
      lastUpdateMs: nowMs(),
    });
  }

  /**
   * Get cached curve state for a mint. Returns null if not cached.
   */
  get(mint: string): CurveState | null {
    const entry = this.cache.get(mint);
    return entry ?? null;
  }

  /**
   * Check if cached state is stale.
   * Returns true if mint is not cached OR if the slot difference exceeds maxStalenessSlots.
   */
  isStale(mint: string, currentSlot: number, maxStalenessSlots: number): boolean {
    const entry = this.cache.get(mint);
    if (!entry) {
      return true;
    }
    return (currentSlot - entry.lastSlot) > maxStalenessSlots;
  }

  /**
   * Simulate a buy on the cached state and return the resulting CurveState
   * WITHOUT committing to cache. Returns null if mint is not cached.
   *
   * Used for speculative what-if analysis (e.g., "what would the curve look
   * like after our front-run buy?").
   */
  applySpeculativeBuy(
    mint: string,
    solInLamports: bigint,
    sim: BondingCurveSimulator,
  ): CurveState | null {
    const entry = this.cache.get(mint);
    if (!entry) {
      return null;
    }

    const result = sim.simulateBuy(
      entry.vSolLamports,
      entry.vTokens,
      solInLamports,
    );

    return {
      vSolLamports: result.newVSol,
      vTokens: result.newVTokens,
      lastSlot: entry.lastSlot,
      lastUpdateMs: entry.lastUpdateMs,
    };
  }

  /**
   * Evict entries older than 60 seconds.
   * Called internally on every update().
   */
  evictStale(): void {
    const cutoff = nowMs() - EVICTION_AGE_MS;
    const toDelete: string[] = [];

    for (const [mint, state] of this.cache) {
      if (state.lastUpdateMs < cutoff) {
        toDelete.push(mint);
      }
    }

    for (const mint of toDelete) {
      this.cache.delete(mint);
    }

    if (toDelete.length > 0) {
      log.debug(`Evicted ${toDelete.length} stale curve entries`);
    }
  }

  /**
   * Number of cached mints.
   */
  size(): number {
    return this.cache.size;
  }
}

/** Singleton instance for application-wide use */
export const curveStateCache = new CurveStateCache();
