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
    buyMomentumTrend: number;
    uniqueBuyersBanded: number;
    buyerDiversity: number;
    curveFill: number;
    crowdDepth5s: number;
    recentBuyers1s: number;
  };
  /** Raw adversarial concentration ratio (0–1). >0.6 triggers 0.5x penalty. */
  adversarialConcentration?: number;
  preTriggerSignals?: {
    buyCount1s: number;
    buyCount2s: number;
    buyCount5s: number;
    netFlow2s: number;
    netFlow5s: number;
    timeSinceLastBuyMs: number;
    /** inter-buy gap at trigger: alias of timeSinceLastBuyMs for clarity in logs/JSONL */
    interBuyGapMs: number;
    accel: number;
    vSolDelta3s: number;
    /** Total buy volume in 5s window before trigger (used for isolationRatio) */
    volume5sBuys: number;
    /** Sell count in 5s window before trigger */
    sellCount5s: number;
    /** Total sell volume in 5s window before trigger */
    sellVolume5s: number;
    /** Net flow ratio: (buyVol - sellVol) / (buyVol + sellVol). 1.0 = all buys, -1.0 = all sells */
    netFlowRatio5s: number;
    /** True if creator sold this token within last 30s */
    creatorSellDetected: boolean;
  };
  entryVSol: number;
  tokenFirstSeenMs: number;
  uniqueBuyerCount: number;
  detectedAt: number;
  recommendedSizeSol?: number;
  /** Time-of-day sizing multiplier applied at entry (1.25/1.10/1.0/0.75) */
  todMultiplier?: number;
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
  /** Timestamp of most recent creator sell detected for this mint (0 = none) */
  creatorSellAt: number;
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

  /**
   * Called when a creator sell is detected (from corecast-v3 Transactions stream).
   * Marks the mint so subsequent trigger events get the creatorSellDetected flag.
   */
  onCreatorSell(mint: string): void {
    const mh = this.history.get(mint);
    if (mh) {
      mh.creatorSellAt = nowMs();
      log.info(`[creator-sell] Marked ${mint.slice(0,8)} — will reject future triggers (30s TTL)`);
    }
  }

  onTrade(event: TokenTradeEvent): void {
    const now = nowMs();
    const mint = event.mint;

    let mh = this.history.get(mint);
    if (!mh) {
      mh = { trades: [], firstSeenMs: now, lastUpdatedMs: now, creatorSellAt: 0 };
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
    const preTriggerSignals = this.computePreTriggerSignals(preTrades, now, event.vSolInBondingCurve, mint);

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

    // Gate 6b: Creator sell gate — if creator sold this token within last 30s, reject
    // Data shows 0 creator sell fields across 3,093 trades = this signal was missing entirely.
    // Creator sells indicate dump bait — continuation probability drops to near-zero.
    if (preTriggerSignals.creatorSellDetected) {
      log.info(`[gate:creator-sell] ${mint.slice(0,8)} creator sold within 30s — skip`);
      return;
    }

    // Gate 6c: Sell pressure gate — if net flow ratio is negative (more sells than buys), reject
    // This catches coordinated selling that the pure buy-count gates miss
    if (preTriggerSignals.netFlowRatio5s < 0.2) {
      log.debug(`[gate:sell-pressure] ${mint.slice(0,8)} netFlowRatio5s=${preTriggerSignals.netFlowRatio5s.toFixed(3)} — skip`);
      return;
    }

    // Gate 6d: Trigger isolation gate — the strongest max_hold predictor from data analysis
    // Data: isolation < 0.1 → 25% max_hold rate, isolation > 0.3 → 53% max_hold rate
    // This is the key finding: isolated buys (trigger is large share of total flow) predict dead trades
    const maxTriggerIsolation = (this.cfg as any).max_trigger_isolation ?? 0.45;
    const triggerSol = event.solAmount;
    const isolationCheck = triggerSol / (preTriggerSignals.volume5sBuys + triggerSol);
    if (isolationCheck > maxTriggerIsolation) {
      log.debug(`[gate:isolation] ${mint.slice(0,8)} isolation=${isolationCheck.toFixed(3)} > ${maxTriggerIsolation} — skip`);
      return;
    }

    const score = this.computeScore(event, uniqueBuyers, vSol, preTriggerSignals);

    // Gate 7: score threshold
    if (score < this.cfg.trigger_min_score) return;

    const opp: BackrunOpportunity = {
      mint,
      triggerEvent: event,
      score,
      components: this.lastComponents,
      adversarialConcentration: this.lastAdversarialConcentration,
      preTriggerSignals,
      entryVSol: vSol,
      tokenFirstSeenMs: mh.firstSeenMs,
      uniqueBuyerCount: uniqueBuyers,
      detectedAt: now,
    };

    log.debug(`Opportunity: ${mint.slice(0,8)} score=${score.toFixed(3)} vSol=${vSol.toFixed(1)} buys1s=${preTriggerSignals.buyCount1s} buys2s=${preTriggerSignals.buyCount2s} gap=${preTriggerSignals.timeSinceLastBuyMs}ms`);
    this.emit('opportunity', opp);
  }

  private lastComponents = { triggerIsolation: 0, buyMomentumTrend: 0, uniqueBuyersBanded: 0, buyerDiversity: 0, curveFill: 0, crowdDepth5s: 0, recentBuyers1s: 0 };
  private lastAdversarialConcentration = 0;

  private computePreTriggerSignals(
    preTrades: TradeRecord[],
    now: number,
    currentVSol: number,
    mint: string,
  ): { buyCount1s: number; buyCount2s: number; buyCount5s: number; netFlow2s: number; netFlow5s: number; timeSinceLastBuyMs: number; interBuyGapMs: number; accel: number; vSolDelta3s: number; volume5sBuys: number; sellCount5s: number; sellVolume5s: number; netFlowRatio5s: number; creatorSellDetected: boolean; preTrades: TradeRecord[] } {
    const buys1s = preTrades.filter(t => t.txType === 'buy' && now - t.ts < 1_000);
    const buyCount1s = buys1s.length;
    const buys2s = preTrades.filter(t => t.txType === 'buy' && now - t.ts < 2_000);
    const buys5s = preTrades.filter(t => t.txType === 'buy' && now - t.ts < 5_000);
    const netFlow2s = buys2s.reduce((s, t) => s + t.solAmount, 0);
    const netFlow5s = buys5s.reduce((s, t) => s + t.solAmount, 0);
    const volume5sBuys = buys5s.reduce((s, t) => s + t.solAmount, 0);

    // NEW: Sell flow tracking — captures sell pressure that buy-only signals miss
    const sells5s = preTrades.filter(t => t.txType === 'sell' && now - t.ts < 5_000);
    const sellCount5s = sells5s.length;
    const sellVolume5s = sells5s.reduce((s, t) => s + t.solAmount, 0);

    // Net flow ratio: (buyVol - sellVol) / (buyVol + sellVol)
    // Range: -1.0 (all sells) to +1.0 (all buys). 0.0 = balanced.
    const totalVol5s = volume5sBuys + sellVolume5s;
    const netFlowRatio5s = totalVol5s > 0 ? (volume5sBuys - sellVolume5s) / totalVol5s : 1.0;

    // Creator sell detection: check if this mint's creator sold within last 30s
    const mh = this.history.get(mint);
    const creatorSellDetected = mh ? (mh.creatorSellAt > 0 && (now - mh.creatorSellAt) < 30_000) : false;

    const lastBuy = [...preTrades].reverse().find(t => t.txType === 'buy');
    const timeSinceLastBuyMs = lastBuy ? now - lastBuy.ts : Infinity;

    const olderCount = buys5s.length - buys2s.length;
    const accel = buys2s.length / Math.max(1, olderCount);

    const trades3s = preTrades.filter(t => now - t.ts < 3_000 && t.vSol > 0);
    const oldestVSol = trades3s.length > 0 ? trades3s[0].vSol : currentVSol;
    const vSolDelta3s = Math.max(0, currentVSol - oldestVSol);

    return { buyCount1s, buyCount2s: buys2s.length, buyCount5s: buys5s.length, netFlow2s, netFlow5s, timeSinceLastBuyMs, interBuyGapMs: timeSinceLastBuyMs, accel, vSolDelta3s, volume5sBuys, sellCount5s, sellVolume5s, netFlowRatio5s, creatorSellDetected, preTrades };
  }

  private computeScore(
    event: TokenTradeEvent,
    uniqueBuyers: number,
    vSol: number,
    pts: { buyCount1s: number; netFlow2s: number; netFlow5s: number; accel: number; buyCount2s: number; buyCount5s: number; volume5sBuys: number; preTrades: TradeRecord[] },
  ): number {
    // 1a. Legacy Trigger Isolation (kept for logging/JSONL but NOT used in score)
    const triggerSol = event.solAmount;
    const isolationRatio = triggerSol / (pts.volume5sBuys + triggerSol);
    const triggerIsolation = Math.pow(Math.max(0, Math.min(1, isolationRatio)), 1.5);

    // 1b. Buy Momentum Trend (10%) — is buy velocity accelerating?
    // v5: DEMOTED from 20% → 10%. Data shows r_pb = -0.043 (anti-predictive for wins).
    // Higher momentum actually correlates with MORE max_hold exits. Kept at minimal weight
    // as a tiebreaker only; removing entirely risks regime-change blind spots.
    const recentBuys1s = pts.buyCount1s;
    const olderBuys1s = Math.max(pts.buyCount2s - pts.buyCount1s, 0.1);
    const momentumRatio = recentBuys1s / olderBuys1s;
    const buyMomentumTrend = Math.max(0, Math.min(1, (momentumRatio - 0.5) / 1.5));

    // 2. Unique Buyers Banded (25%) — sweet spot at 5-10 buyers, penalise <3 and >15
    let buyerScore: number;
    if (uniqueBuyers < 3) buyerScore = 0.1;
    else if (uniqueBuyers <= 5) buyerScore = 0.5 + (uniqueBuyers - 3) * 0.15;
    else if (uniqueBuyers <= 10) buyerScore = 0.8 + (uniqueBuyers - 5) * 0.04;
    else if (uniqueBuyers <= 15) buyerScore = 1.0 - (uniqueBuyers - 10) * 0.06;
    else buyerScore = 0.7;

    // 3. Buyer Diversity (10%) — unique wallets / total buys in last 30s
    // v5: DEMOTED from 20% → 10%. r_pb = 0.036 (weak). Budget reallocated to crowd signals.
    const now30sCutoff = Date.now() - 30_000;
    const recentBuys = [...pts.preTrades, { ts: Date.now(), solAmount: event.solAmount, txType: 'buy' as const, trader: event.traderPublicKey, vSol }]
      .filter(t => t.txType === 'buy' && t.ts >= now30sCutoff);
    const uniqueTraders30s = new Set(recentBuys.map(t => t.trader)).size;
    const buyerDiversity = Math.min(1, (uniqueTraders30s / Math.max(1, recentBuys.length)) * 1.5);

    // 4. Curve Fill (20%) — prefer early curve (lower vSol = higher score)
    const range = this.cfg.max_vsol_in_curve - this.cfg.min_vsol_in_curve;
    const fill = (vSol - this.cfg.min_vsol_in_curve) / range;
    const curveFillScore = Math.max(0, 1 - fill);

    // 5. Crowd Depth 5s (20%) — pre-trigger buy volume normalized to [0, 1]
    // v5 NEW: r_pb = 0.097 for win prediction, r_pb = -0.230 for max_hold prediction.
    // This is the STRONGEST max_hold predictor in the dataset.
    // vol5s 0-1 → 54% max_hold; vol5s 5-10 → 22% max_hold.
    // Normalized: 10 SOL in 5s = max score (rare but achievable).
    const crowdDepth5s = Math.min(1, pts.volume5sBuys / 10);

    // 6. Recent Buyers 1s (15%) — buy count in final 1s before trigger
    // v5 NEW: r_pb = 0.121 for win prediction, r_pb = -0.189 for max_hold prediction.
    // Second strongest max_hold predictor. Measures "is the crowd still arriving?"
    // Normalized: 6 buys in 1s = max score (high activity).
    const recentBuyers1s = Math.min(1, pts.buyCount1s / 6);

    // Adversarial concentration check: single wallet > 60% of 30s buy volume = likely pump setup
    const totalBuyVol30s = recentBuys.reduce((s, t) => s + t.solAmount, 0);
    const walletVols = new Map<string, number>();
    for (const t of recentBuys) walletVols.set(t.trader, (walletVols.get(t.trader) ?? 0) + t.solAmount);
    const maxWalletVol = Math.max(...walletVols.values(), 0);
    const concentrationRatio = totalBuyVol30s > 0 ? maxWalletVol / totalBuyVol30s : 0;
    this.lastAdversarialConcentration = concentrationRatio;
    const adversarialPenalty = concentrationRatio > 0.6 ? 0.5 : 1.0;

    // Final weighted score with adversarial penalty
    // v5: Data-driven rebalance from 1,599-trade regression analysis.
    // Key changes: momentum demoted 20%→10% (anti-predictive), diversity demoted 20%→10%,
    // curveFill trimmed 30%→20%, budget reallocated to crowdDepth5s (20%) + recentBuyers1s (15%).
    // Weights: momentum 10% + buyers 25% + diversity 10% + curveFill 20% + crowd5s 20% + recent1s 15% = 100%
    // Backtest: WR 46.8% (from 42.9%), max_hold 29.9% (from 36.5%), EV +17.6 bps/trade.
    const rawScore = buyMomentumTrend * 0.10 + buyerScore * 0.25 + buyerDiversity * 0.10 + curveFillScore * 0.20 + crowdDepth5s * 0.20 + recentBuyers1s * 0.15;
    const score = rawScore * adversarialPenalty;
    this.lastComponents = { triggerIsolation, buyMomentumTrend, uniqueBuyersBanded: buyerScore, buyerDiversity, curveFill: curveFillScore, crowdDepth5s, recentBuyers1s };
    log.debug(`[score-v5] ${event.mint.slice(0,8)} momentum=${buyMomentumTrend.toFixed(3)} buyers=${buyerScore.toFixed(3)} diversity=${buyerDiversity.toFixed(3)} curve=${curveFillScore.toFixed(3)} crowd5s=${crowdDepth5s.toFixed(3)} recent1s=${recentBuyers1s.toFixed(3)} adversarial=${adversarialPenalty} → ${score.toFixed(3)}`);
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
