/**
 * @module mev/detector
 * BackrunDetector: scores each confirmed buy for momentum backrun opportunity.
 *
 * Score components:
 *   size component    35%  — normalised buy size vs trigger threshold
 *   momentum          25%  — 30-second net SOL flow
 *   unique buyers     20%  — unique buyer count in last 60 s
 *   curve fill        20%  — how far through the target vSol band
 *
 * Entry gates: txType=buy, solAmount >= trigger_min_buy_sol, vSol in range,
 * token age <= max_token_age_s, uniqueBuyers >= min_unique_buyers, score >= trigger_min_score.
 *
 * Per-mint 60 s sliding history window; stale entries evicted after 300 s.
 */

import { EventEmitter } from 'events';
import { MevConfig } from '../types/config';
import { TokenTradeEvent } from '../types/events';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';

const log = createLogger('mev:detector');

export interface BackrunOpportunity {
  mint: string;
  triggerEvent: TokenTradeEvent;
  score: number;
  /** Component breakdown for diagnostics */
  components: {
    size: number;
    momentum: number;
    uniqueBuyers: number;
    curveFill: number;
  };
  entryVSol: number;
  tokenFirstSeenMs: number;
  uniqueBuyerCount: number;
  detectedAt: number;
}

interface TradeRecord {
  ts: number;
  solAmount: number;
  txType: 'buy' | 'sell';
  trader: string;
  vSol: number;
}

interface MintHistory {
  trades: TradeRecord[];
  firstSeenMs: number;
  lastUpdatedMs: number;
}

/** How long to keep per-mint history (ms) */
const HISTORY_WINDOW_MS = 60_000;
/** How long before a mint with no trades is evicted (ms) */
const STALE_EVICT_MS = 300_000;

export declare interface BackrunDetector {
  on(event: 'opportunity', listener: (opp: BackrunOpportunity) => void): this;
  emit(event: 'opportunity', opp: BackrunOpportunity): boolean;
}

export class BackrunDetector extends EventEmitter {
  private cfg: MevConfig;
  private history: Map<string, MintHistory> = new Map();
  private evictTimer: NodeJS.Timeout;

  constructor(cfg: MevConfig) {
    super();
    this.cfg = cfg;
    // Evict stale mint histories every 60 s
    this.evictTimer = setInterval(() => this.evictStale(), 60_000);
    this.evictTimer.unref();
  }

  /**
   * Feed a TokenTradeEvent into the detector.
   * Will emit 'opportunity' if the event passes all gates.
   */
  onTrade(event: TokenTradeEvent): void {
    const now = nowMs();
    const mint = event.mint;

    // Upsert mint history
    let mh = this.history.get(mint);
    if (!mh) {
      mh = { trades: [], firstSeenMs: now, lastUpdatedMs: now };
      this.history.set(mint, mh);
    }
    mh.lastUpdatedMs = now;

    // Prune records outside sliding window
    const cutoff = now - HISTORY_WINDOW_MS;
    mh.trades = mh.trades.filter(t => t.ts >= cutoff);

    // Append this trade
    mh.trades.push({
      ts: now,
      solAmount: event.solAmount,
      txType: event.txType,
      trader: event.traderPublicKey,
      vSol: event.vSolInBondingCurve,
    });

    // Gate 1: must be a buy
    if (event.txType !== 'buy') return;

    // Gate 2: minimum buy size
    if (event.solAmount < this.cfg.trigger_min_buy_sol) return;

    // Gate 3: vSol within target range
    const vSol = event.vSolInBondingCurve;
    if (vSol < this.cfg.min_vsol_in_curve || vSol > this.cfg.max_vsol_in_curve) return;

    // Gate 4: token age
    const ageS = (now - mh.firstSeenMs) / 1000;
    if (ageS > this.cfg.max_token_age_s) return;

    // Gate 5: unique buyers in window
    const uniqueBuyers = new Set(mh.trades.map(t => t.trader)).size;
    if (uniqueBuyers < this.cfg.min_unique_buyers) return;

    // Compute score
    const score = this.computeScore(event, mh, uniqueBuyers, vSol);

    // Gate 6: score threshold
    if (score < this.cfg.trigger_min_score) return;

    const opp: BackrunOpportunity = {
      mint,
      triggerEvent: event,
      score,
      components: this.lastComponents,
      entryVSol: vSol,
      tokenFirstSeenMs: mh.firstSeenMs,
      uniqueBuyerCount: uniqueBuyers,
      detectedAt: now,
    };

    log.debug(`Opportunity detected: ${mint.slice(0, 8)} score=${score.toFixed(3)} vSol=${vSol.toFixed(1)}`);
    this.emit('opportunity', opp);
  }

  /** Populated by computeScore() for the last call — avoids repeated calculation. */
  private lastComponents = { size: 0, momentum: 0, uniqueBuyers: 0, curveFill: 0 };

  private computeScore(
    event: TokenTradeEvent,
    mh: MintHistory,
    uniqueBuyers: number,
    vSol: number,
  ): number {
    // --- Component 1: size (35%) ---
    // Normalised: 1.0 at 10× threshold, clamped [0,1]
    const sizeRatio = event.solAmount / this.cfg.trigger_min_buy_sol;
    const sizeScore = Math.min(1, sizeRatio / 10);

    // --- Component 2: 30 s momentum (25%) ---
    const momentumCutoff = nowMs() - 30_000;
    const recentBuys = mh.trades.filter(t => t.ts >= momentumCutoff && t.txType === 'buy');
    const recentSells = mh.trades.filter(t => t.ts >= momentumCutoff && t.txType === 'sell');
    const buyFlow = recentBuys.reduce((s, t) => s + t.solAmount, 0);
    const sellFlow = recentSells.reduce((s, t) => s + t.solAmount, 0);
    // Net flow capped at 2× trigger threshold per second for normalisation
    const netFlow = buyFlow - sellFlow;
    const momentumScore = Math.max(0, Math.min(1, netFlow / (this.cfg.trigger_min_buy_sol * 20)));

    // --- Component 3: unique buyers (20%) ---
    // 1.0 at 10 buyers, scaled linearly
    const uniqueBuyerScore = Math.min(1, uniqueBuyers / 10);

    // --- Component 4: curve fill (20%) ---
    // Position within [min_vsol, max_vsol] range: sweet spot in the middle
    const range = this.cfg.max_vsol_in_curve - this.cfg.min_vsol_in_curve;
    const fill = (vSol - this.cfg.min_vsol_in_curve) / range; // 0..1
    // Peak at mid-range (fill=0.5)
    const curveFillScore = 1 - Math.abs(fill - 0.5) * 2;

    this.lastComponents = {
      size: sizeScore,
      momentum: momentumScore,
      uniqueBuyers: uniqueBuyerScore,
      curveFill: curveFillScore,
    };

    return (
      sizeScore * 0.35 +
      momentumScore * 0.25 +
      uniqueBuyerScore * 0.20 +
      curveFillScore * 0.20
    );
  }

  private evictStale(): void {
    const now = nowMs();
    let evicted = 0;
    for (const [mint, mh] of this.history) {
      if (now - mh.lastUpdatedMs > STALE_EVICT_MS) {
        this.history.delete(mint);
        evicted++;
      }
    }
    if (evicted > 0) {
      log.debug(`Evicted ${evicted} stale mint histories (${this.history.size} remaining)`);
    }
  }

  destroy(): void {
    clearInterval(this.evictTimer);
    this.history.clear();
  }
}
