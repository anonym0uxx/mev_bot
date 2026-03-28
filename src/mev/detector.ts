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
    triggerIsolation: number;
    uniqueBuyersBanded: number;
    buyerDiversity: number;
    curveFill: number;
  };
  preTriggerSignals?: {
    buyCount1s: number;
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

    // Gate 5b: large-trigger concentration guard
    if (event.solAmount > 1.5 && uniqueBuyers < 5) {
      log.debug(`[gate:large-trigger] ${mint.slice(0,8)} trigger=${event.solAmount.toFixed(2)} buyers=${uniqueBuyers} < 5 — skip`);
      return;
    }

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

      // Gate: at least one buy in the final 1s before trigger (momentum still live)
      const minBuys1s = this.cfg.pre_trigger_min_buys_1s ?? 1;
      if (preTriggerSignals.buyCount1s < minBuys1s) {
        log.debug(`[gate:stale-momentum] ${mint.slice(0,8)} buys1s=${preTriggerSignals.buyCount1s} < ${minBuys1s} — skip`);
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

    log.debug(`Opportunity: ${mint.slice(0,8)} score=${score.toFixed(3)} vSol=${vSol.toFixed(1)} buys1s=${preTriggerSignals.buyCount1s} buys2s=${preTriggerSignals.buyCount2s} gap=${preTriggerSignals.timeSinceLastBuyMs}ms`);
    this.emit('opportunity', opp);
  }

  private lastComponents = { triggerIsolation: 0, uniqueBuyersBanded: 0, buyerDiversity: 0, curveFill: 0 };

  private computePreTriggerSignals(
    preTrades: TradeRecord[],
    now: number,
    currentVSol: number,
  ): { buyCount1s: number; buyCount2s: number; buyCount5s: number; netFlow2s: number; netFlow5s: number; timeSinceLastBuyMs: number; accel: number; vSolDelta3s: number; volume5sBuys: number; preTrades: TradeRecord[] } {
    const buys1s = preTrades.filter(t => t.txType === 'buy' && now - t.ts < 1_000);
    const buyCount1s = buys1s.length;
    const buys2s = preTrades.filter(t => t.txType === 'buy' && now - t.ts < 2_000);
    const buys5s = preTrades.filter(t => t.txType === 'buy' && now - t.ts < 5_000);
    const netFlow2s = buys2s.reduce((s, t) => s + t.solAmount, 0);
    const netFlow5s = buys5s.reduce((s, t) => s + t.solAmount, 0);
    const volume5sBuys = buys5s.reduce((s, t) => s + t.solAmount, 0);

    const lastBuy = [...preTrades].reverse().find(t => t.txType === 'buy');
    const timeSinceLastBuyMs = lastBuy ? now - lastBuy.ts : Infinity;

    const olderCount = buys5s.length - buys2s.length;
    const accel = buys2s.length / Math.max(1, olderCount);

    const trades3s = preTrades.filter(t => now - t.ts < 3_000 && t.vSol > 0);
    const oldestVSol = trades3s.length > 0 ? trades3s[0].vSol : currentVSol;
    const vSolDelta3s = Math.max(0, currentVSol - oldestVSol);

    return { buyCount1s, buyCount2s: buys2s.length, buyCount5s: buys5s.length, netFlow2s, netFlow5s, timeSinceLastBuyMs, accel, vSolDelta3s, volume5sBuys, preTrades };
  }

  private computeScore(
    event: TokenTradeEvent,
    uniqueBuyers: number,
    vSol: number,
    pts: { netFlow2s: number; netFlow5s: number; accel: number; buyCount2s: number; volume5sBuys: number; preTrades: TradeRecord[] },
  ): number {
    // 1. Trigger Isolation (40%) — how much of recent volume is THIS trigger vs background noise
    const triggerSol = event.solAmount;
    const isolationRatio = triggerSol / (pts.volume5sBuys + triggerSol);
    const triggerIsolation = Math.pow(Math.max(0, Math.min(1, isolationRatio)), 1.5);

    // 2. Unique Buyers Banded (25%) — sweet spot at 5-10 buyers, penalise <3 and >15
    let buyerScore: number;
    if (uniqueBuyers < 3) buyerScore = 0.1;
    else if (uniqueBuyers <= 5) buyerScore = 0.5 + (uniqueBuyers - 3) * 0.15;
    else if (uniqueBuyers <= 10) buyerScore = 0.8 + (uniqueBuyers - 5) * 0.04;
    else if (uniqueBuyers <= 15) buyerScore = 1.0 - (uniqueBuyers - 10) * 0.06;
    else buyerScore = 0.7;

    // 3. Buyer Diversity (20%) — unique wallets / total buys in last 30s
    const now30sCutoff = Date.now() - 30_000;
    const recentBuys = [...pts.preTrades, { ts: Date.now(), solAmount: event.solAmount, txType: 'buy' as const, trader: event.traderPublicKey, vSol }]
      .filter(t => t.txType === 'buy' && t.ts >= now30sCutoff);
    const uniqueTraders30s = new Set(recentBuys.map(t => t.trader)).size;
    const buyerDiversity = Math.min(1, (uniqueTraders30s / Math.max(1, recentBuys.length)) * 1.5);

    // 4. Curve Fill (15%) — prefer early curve (lower vSol = higher score)
    const range = this.cfg.max_vsol_in_curve - this.cfg.min_vsol_in_curve;
    const fill = (vSol - this.cfg.min_vsol_in_curve) / range;
    const curveFillScore = Math.max(0, 1 - fill);

    // Adversarial concentration check: single wallet > 60% of 30s buy volume = likely pump setup
    const totalBuyVol30s = recentBuys.reduce((s, t) => s + t.solAmount, 0);
    const walletVols = new Map<string, number>();
    for (const t of recentBuys) walletVols.set(t.trader, (walletVols.get(t.trader) ?? 0) + t.solAmount);
    const maxWalletVol = Math.max(...walletVols.values(), 0);
    const concentrationRatio = totalBuyVol30s > 0 ? maxWalletVol / totalBuyVol30s : 0;
    const adversarialPenalty = concentrationRatio > 0.6 ? 0.5 : 1.0;

    // Final weighted score with adversarial penalty
    const rawScore = triggerIsolation * 0.40 + buyerScore * 0.25 + buyerDiversity * 0.20 + curveFillScore * 0.15;
    const score = rawScore * adversarialPenalty;
    this.lastComponents = { triggerIsolation, uniqueBuyersBanded: buyerScore, buyerDiversity, curveFill: curveFillScore };
    log.debug(`[score-v2] ${event.mint.slice(0,8)} isolation=${triggerIsolation.toFixed(3)} buyers=${buyerScore.toFixed(3)} diversity=${buyerDiversity.toFixed(3)} curve=${curveFillScore.toFixed(3)} adversarial=${adversarialPenalty} → ${score.toFixed(3)}`);
    return score;
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
