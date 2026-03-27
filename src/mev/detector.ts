/**
 * @module mev/detector
 * BackrunDetector: scores each confirmed buy for momentum backrun opportunity.
 *
 * Score components:
 *   size component    35%  — normalised buy size vs trigger threshold
 *   momentum          40%  — 2s+5s weighted net flow (replaces stale 30s signal)
 *   unique buyers     15%  — unique buyer count in last 60 s
 *   curve fill        10%  — how far through the target vSol band
 *
 * Pre-trigger momentum gate (when enabled):
 *   - Requires recent buy activity BEFORE the trigger event
 *   - Eliminates isolated buys with no crowd behind them (max_hold prevention)
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
  components: {
    size: number;
    momentum: number;
    uniqueBuyers: number;
    curveFill: number;
  };
  preTriggerSignals?: {
    buyCount2s: number;
    buyCount5s: number;
    netFlow2s: number;
    netFlow5s: number;
    timeSinceLastBuyMs: number;
    accel: number;
    vSolDelta3s: number;
  };
  entryVSol: number;
  tokenFirstSeenMs: number;
  uniqueBuyerCount: number;
  detectedAt: number;
  recommendedSizeSol?: number;
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

const HISTORY_WINDOW_MS = 60_000;
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
    this.evictTimer = setInterval(() => this.evictStale(), 60_000);
    this.evictTimer.unref();
  }

  onTrade(event: TokenTradeEvent): void {
    const now = nowMs();
    const mint = event.mint;

    let mh = this.history.get(mint);
    if (!mh) {
      mh = { trades: [], firstSeenMs: now, lastUpdatedMs: now };
      this.history.set(mint, mh);
    }
    mh.lastUpdatedMs = now;

    const cutoff = now - HISTORY_WINDOW_MS;
    mh.trades = mh.trades.filter(t => t.ts >= cutoff);

    // Append BEFORE gate checks so future events have full history
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

    // Gate 2b: maximum buy size
    if (this.cfg.trigger_max_buy_sol !== undefined && event.solAmount > this.cfg.trigger_max_buy_sol) return;

    // Gate 3: vSol within target range
    const vSol = event.vSolInBondingCurve;
    if (vSol < this.cfg.min_vsol_in_curve || vSol > this.cfg.max_vsol_in_curve) return;

    // Gate 4: token age
    const ageS = (now - mh.firstSeenMs) / 1000;
    if (ageS > this.cfg.max_token_age_s) return;

    // Gate 5: unique buyers in window
    const uniqueBuyers = new Set(mh.trades.map(t => t.trader)).size;
    if (uniqueBuyers < this.cfg.min_unique_buyers) return;

    // Gate 6: pre-trigger momentum gate
    // Use all trades EXCEPT the current trigger (slice off last element)
    const preTrades = mh.trades.slice(0, -1);
    const preTriggerSignals = this.computePreTriggerSignals(preTrades, now, event.vSolInBondingCurve);

    if (this.cfg.pre_trigger_gate_enabled !== false) {
      const maxGapMs = this.cfg.pre_trigger_max_gap_ms ?? 3000;
      const minBuys2s = this.cfg.pre_trigger_min_buys_2s ?? 1;
      const minBuys5s = this.cfg.pre_trigger_min_buys_5s ?? 2;
      const minVSolAccel = this.cfg.pre_trigger_min_vsol_accel ?? 0.3;

      if (preTriggerSignals.timeSinceLastBuyMs > maxGapMs) {
        log.debug(`[gate:isolated] ${mint.slice(0,8)} gap=${preTriggerSignals.timeSinceLastBuyMs}ms — skip`);
        return;
      }

      if (event.solAmount < 0.5) {
        if (preTriggerSignals.buyCount2s < minBuys2s) {
          log.debug(`[gate:no-crowd-2s] ${mint.slice(0,8)} buys2s=${preTriggerSignals.buyCount2s} — skip`);
          return;
        }
        if (preTriggerSignals.buyCount5s < minBuys5s) {
          log.debug(`[gate:no-crowd-5s] ${mint.slice(0,8)} buys5s=${preTriggerSignals.buyCount5s} — skip`);
          return;
        }
      }

      if (preTriggerSignals.vSolDelta3s < minVSolAccel) {
        log.debug(`[gate:no-accel] ${mint.slice(0,8)} vSolDelta3s=${preTriggerSignals.vSolDelta3s.toFixed(3)} — skip`);
        return;
      }
    }

    const score = this.computeScore(event, uniqueBuyers, vSol, preTriggerSignals);

    // Gate 7: score threshold
    if (score < this.cfg.trigger_min_score) return;

    const opp: BackrunOpportunity = {
      mint,
      triggerEvent: event,
      score,
      components: this.lastComponents,
      preTriggerSignals,
      entryVSol: vSol,
      tokenFirstSeenMs: mh.firstSeenMs,
      uniqueBuyerCount: uniqueBuyers,
      detectedAt: now,
    };

    log.debug(`Opportunity: ${mint.slice(0,8)} score=${score.toFixed(3)} vSol=${vSol.toFixed(1)} buys2s=${preTriggerSignals.buyCount2s} gap=${preTriggerSignals.timeSinceLastBuyMs}ms`);
    this.emit('opportunity', opp);
  }

  private lastComponents = { size: 0, momentum: 0, uniqueBuyers: 0, curveFill: 0 };

  private computePreTriggerSignals(
    preTrades: TradeRecord[],
    now: number,
    currentVSol: number,
  ): { buyCount2s: number; buyCount5s: number; netFlow2s: number; netFlow5s: number; timeSinceLastBuyMs: number; accel: number; vSolDelta3s: number } {
    const buys2s = preTrades.filter(t => t.txType === 'buy' && now - t.ts < 2_000);
    const buys5s = preTrades.filter(t => t.txType === 'buy' && now - t.ts < 5_000);
    const netFlow2s = buys2s.reduce((s, t) => s + t.solAmount, 0);
    const netFlow5s = buys5s.reduce((s, t) => s + t.solAmount, 0);

    const lastBuy = [...preTrades].reverse().find(t => t.txType === 'buy');
    const timeSinceLastBuyMs = lastBuy ? now - lastBuy.ts : Infinity;

    const olderCount = buys5s.length - buys2s.length;
    const accel = buys2s.length / Math.max(1, olderCount);

    const trades3s = preTrades.filter(t => now - t.ts < 3_000 && t.vSol > 0);
    const oldestVSol = trades3s.length > 0 ? trades3s[0].vSol : currentVSol;
    const vSolDelta3s = Math.max(0, currentVSol - oldestVSol);

    return { buyCount2s: buys2s.length, buyCount5s: buys5s.length, netFlow2s, netFlow5s, timeSinceLastBuyMs, accel, vSolDelta3s };
  }

  private computeScore(
    event: TokenTradeEvent,
    uniqueBuyers: number,
    vSol: number,
    pts: { netFlow2s: number; netFlow5s: number; accel: number; buyCount2s: number },
  ): number {
    // Size (35%)
    const sizeRatio = event.solAmount / this.cfg.trigger_min_buy_sol;
    const sizeScore = Math.min(1, sizeRatio / 10);

    // Momentum (40%) — 2s/5s weighted, kills 30s stale signal
    const triggerSol = event.solAmount;
    const flow2sNorm  = Math.min(1, pts.netFlow2s  / Math.max(0.01, triggerSol * 3));
    const flow5sNorm  = Math.min(1, pts.netFlow5s  / Math.max(0.01, triggerSol * 8));
    const accelScore  = Math.min(1, pts.accel / 1.5);
    const velocityScore = Math.min(1, pts.buyCount2s / 4);
    const momentumScore = 0.45 * flow2sNorm + 0.25 * flow5sNorm + 0.20 * accelScore + 0.10 * velocityScore;

    // Unique buyers (15%)
    const uniqueBuyerScore = Math.min(1, uniqueBuyers / 10);

    // Curve fill (10%)
    const range = this.cfg.max_vsol_in_curve - this.cfg.min_vsol_in_curve;
    const fill = (vSol - this.cfg.min_vsol_in_curve) / range;
    const curveFillScore = Math.max(0, 1 - fill);

    this.lastComponents = { size: sizeScore, momentum: momentumScore, uniqueBuyers: uniqueBuyerScore, curveFill: curveFillScore };

    return sizeScore * 0.35 + momentumScore * 0.40 + uniqueBuyerScore * 0.15 + curveFillScore * 0.10;
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
    if (evicted > 0) log.debug(`Evicted ${evicted} stale histories`);
  }

  destroy(): void {
    clearInterval(this.evictTimer);
    this.history.clear();
  }
}
