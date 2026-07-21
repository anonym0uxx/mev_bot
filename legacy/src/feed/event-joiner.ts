/**
 * @module feed/event-joiner
 * Joins pool creation events (transactions stream) with first trades (dex_trades stream).
 * Pool creation arrives before trades; we hold pending pools for up to 60s.
 */

import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { NewTokenEvent } from '../types/events';

const log = createLogger('event-joiner');
const PENDING_TTL_MS = 60_000; // Expire pending pool events after 60s

export interface PendingPoolEvent {
  mint: string;
  creator: string;
  name: string;
  symbol: string;
  uri: string;
  createdAt: number;
  initialVirtualTokenReserves: number;
  initialVirtualSolReserves: number;
  expiresAt: number;
}

export class EventJoiner {
  private pendingPools: Map<string, PendingPoolEvent> = new Map();
  private cleanupTimer: NodeJS.Timeout | null = null;

  constructor() {
    // Periodically clean up expired pending pools
    this.cleanupTimer = setInterval(() => this.cleanup(), 10_000);
  }

  /** Register a pool creation event, waiting for first trade */
  registerPool(event: PendingPoolEvent): void {
    this.pendingPools.set(event.mint, event);
    log.debug(`Pool registered: ${event.mint.slice(0, 8)} creator=${event.creator.slice(0, 8)}`);
  }

  /** Get and consume a pending pool event for a mint (called on first trade) */
  consumePool(mint: string): PendingPoolEvent | null {
    const pool = this.pendingPools.get(mint);
    if (!pool) return null;
    if (nowMs() > pool.expiresAt) {
      this.pendingPools.delete(mint);
      return null;
    }
    this.pendingPools.delete(mint);
    return pool;
  }

  /** Build a NewTokenEvent from a pending pool (when creator already known) */
  buildNewTokenEvent(pool: PendingPoolEvent): NewTokenEvent {
    return {
      signature: '',
      mint: pool.mint,
      traderPublicKey: pool.creator, // creator = first signer
      txType: 'create',
      name: pool.name,
      symbol: pool.symbol,
      uri: pool.uri,
      bondingCurveKey: '',
      vTokensInBondingCurve: pool.initialVirtualTokenReserves,
      vSolInBondingCurve: pool.initialVirtualSolReserves,
      marketCapSol: 0,
      timestamp: pool.createdAt,
    };
  }

  /** Check if we have a pending pool for this mint */
  hasPending(mint: string): boolean {
    const pool = this.pendingPools.get(mint);
    if (!pool) return false;
    if (nowMs() > pool.expiresAt) {
      this.pendingPools.delete(mint);
      return false;
    }
    return true;
  }

  get pendingCount(): number {
    return this.pendingPools.size;
  }

  destroy(): void {
    if (this.cleanupTimer) {
      clearInterval(this.cleanupTimer);
      this.cleanupTimer = null;
    }
  }

  private cleanup(): void {
    const now = nowMs();
    let expired = 0;
    for (const [mint, pool] of this.pendingPools) {
      if (now > pool.expiresAt) {
        this.pendingPools.delete(mint);
        expired++;
      }
    }
    if (expired > 0) {
      log.debug(`EventJoiner: expired ${expired} pending pools`);
    }
  }
}
