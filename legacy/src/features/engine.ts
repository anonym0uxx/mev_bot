/**
 * @module features/engine
 * Rolling feature engine: maintains windowed trade buffers per token
 * and computes full feature snapshots across all 6 families.
 */

import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import {
  FeatureSnapshot, TradeDataPoint, FlowMomentumFeatures,
  BreadthTopologyFeatures, ManipulationDistributionFeatures,
} from '../types/features';
import { FrictionExecutionFeatures, CreatorWalletPriorFeatures, MultimodalJunkFeatures } from '../types/features';
import { PumpQuantConfig } from '../types/config';
import { computeFlowMomentum } from './flow-momentum';
import { computeBreadthTopology } from './breadth-topology';
import { computeCreatorWalletPriors, CreatorWalletContext } from './creator-wallet-priors';
import { computeFrictionExecution, FrictionContext } from './friction-execution';
import { computeManipulationDistribution, ManipulationContext } from './manipulation-distribution';
import { getMultimodalJunkScore, MultimodalContext } from './multimodal-junk-filter';
import { computeBondingCurveDynamics, BondingCurveDynamicsContext } from './bonding-curve-dynamics';

const log = createLogger('features');

/** Per-token trade buffer and state */
export interface TokenTradeState {
  mint: string;
  creator: string;
  trades: TradeDataPoint[];
  /** All unique buyer addresses seen */
  uniqueBuyers: Set<string>;
  /** Wallet → total balance */
  walletBalances: Map<string, number>;
  /** Previous window velocities for acceleration */
  prevVelocities: Map<string, number>;
  /** Creator wallet context for priors */
  creatorContext: CreatorWalletContext | null;
  /** Friction context */
  frictionContext: FrictionContext;
  /** Multimodal context */
  multimodalContext: MultimodalContext | null;
  /** Last feature computation timestamp */
  lastFeatureCompute: number;
  /** Unix ms when this token was created on-chain — used for maturity-aware manipulation gating */
  tokenCreatedAt: number;

  // ── Capital efficiency tracking (arXiv:2602.14860 primary predictor) ──
  /** Cumulative swap count since token creation */
  totalSwapCount: number;
  /** vSolInBondingCurve at first observed trade */
  vSolAtFirstTrade: number;
  /** vSolInBondingCurve when token entered monitoring window */
  vSolAtObservationStart: number;
  /** totalSwapCount when monitoring window started */
  swapCountAtObservationStart: number;
  /** current vSol - vSolAtFirstTrade */
  lifetimeVSolAccumulated: number;
  /** current vSol - vSolAtObservationStart */
  windowVSolAccumulated: number;
  /** totalSwapCount - swapCountAtObservationStart */
  windowSwapCount: number;
  /** Count of trades where solAmount >= 0.10 SOL */
  largeTradeCount: number;
  /** Capital efficiency sampled every 10s, max 12 entries (for trend analysis) */
  capitalEfficiencyHistory: Array<{ timestamp: number; value: number }>;
  /** Sum of creator sells received - creator buys spent (SOL). Positive = dumping. */
  creatorNetSolPosition: number;
  /** vSolInBondingCurve after each of first 30 trades (capped at 30, never evicted) */
  firstTradesVSolSnapshot: number[];
  /** How many trades are in firstTradesVSolSnapshot (≤ 30) */
  firstTradesCount: number;
  /** Impact ratios for first 20 trades (never evicted): solAmount / preTrade_vSol */
  firstNTradeImpacts: number[];
  /** Count of all trades where impact_ratio > 0.05 (5% of curve depth) */
  highImpactTradeCount: number;
  /** Max single trade impact ratio across all trades */
  maxImpactRatio: number;
}

export class FeatureEngine {
  private tokenStates: Map<string, TokenTradeState> = new Map();
  private windows: number[];

  constructor(private config: PumpQuantConfig) {
    this.windows = config.features.windows_s;
  }

  /** Update config (on version change) */
  updateConfig(config: PumpQuantConfig): void {
    this.config = config;
    this.windows = config.features.windows_s;
  }

  /** Initialize tracking for a new token */
  initToken(mint: string, creator: string, tokenCreatedAt?: number): void {
    if (this.tokenStates.has(mint)) return;

    this.tokenStates.set(mint, {
      mint,
      creator,
      trades: [],
      uniqueBuyers: new Set(),
      walletBalances: new Map(),
      prevVelocities: new Map(),
      creatorContext: null,
      tokenCreatedAt: tokenCreatedAt ?? Date.now(),
      frictionContext: {
        expectedEntrySlippage: this.config.friction.default_entry_slippage_pct,
        expectedExitSlippage: this.config.friction.default_exit_slippage_pct,
        routeMode: this.config.execution.default_route_mode,
        priorityFeeSol: this.config.execution.default_priority_fee_sol,
        landingRisk: 0.05,
        retryFailureRate: 0,
        executionFreshnessS: 0,
        latencyBudgetMs: this.config.execution.confirmation_timeout_ms,
        actualLatencyMs: 0,
        routeHealthLandingMs: 0,
        routeHealthRetryRate: 0,
        routeHealthCongestion: 0,
      },
      multimodalContext: null,
      lastFeatureCompute: 0,
      // Capital efficiency tracking (arXiv:2602.14860)
      totalSwapCount: 0,
      vSolAtFirstTrade: 0,
      vSolAtObservationStart: 0,
      swapCountAtObservationStart: 0,
      lifetimeVSolAccumulated: 0,
      windowVSolAccumulated: 0,
      windowSwapCount: 0,
      largeTradeCount: 0,
      capitalEfficiencyHistory: [],
      creatorNetSolPosition: 0,
      firstTradesVSolSnapshot: [],
      firstTradesCount: 0,
      firstNTradeImpacts: [],
      highImpactTradeCount: 0,
      maxImpactRatio: 0,
    });
  }

  /** Add a trade data point and update state */
  addTrade(mint: string, trade: TradeDataPoint): void {
    const state = this.tokenStates.get(mint);
    if (!state) return;

    state.trades.push(trade);

    // Track unique buyers
    if (trade.txType === 'buy') {
      state.uniqueBuyers.add(trade.traderPublicKey);
    }

    // Update wallet balances
    // Only update wallet balance if we have real data (gRPC events have newTokenBalance=0, which would corrupt concentration metrics)
    if (trade.newTokenBalance > 0 || trade.txType === 'sell') {
      state.walletBalances.set(trade.traderPublicKey, trade.newTokenBalance);
    }

    // ── Capital efficiency tracking (arXiv:2602.14860) ──
    state.totalSwapCount++;

    // Snapshot vSol at first trade
    if (state.vSolAtFirstTrade === 0 && trade.vSolInBondingCurve > 0) {
      state.vSolAtFirstTrade = trade.vSolInBondingCurve;
    }

    // Large trade detection
    if (trade.solAmount >= 0.10) {
      state.largeTradeCount++;
    }

    // Creator net SOL position tracking
    if (trade.traderPublicKey === state.creator) {
      if (trade.txType === 'buy') {
        state.creatorNetSolPosition -= trade.solAmount;
      } else {
        state.creatorNetSolPosition += trade.solAmount;
      }
    }

    // --- First-N-trades snapshot (never evicted, capped at 30) ---
    if (state.firstTradesCount < 30) {
      state.firstTradesVSolSnapshot.push(trade.vSolInBondingCurve);
      state.firstTradesCount++;
    }

    // --- Trade impact ratio ---
    if (trade.solAmount > 0) {
      // Approximate pre-trade vSol: post-trade vSol minus solAmount for buys
      const preTradeVSol = trade.txType === 'buy'
        ? Math.max(trade.vSolInBondingCurve - trade.solAmount, 0.001)
        : trade.vSolInBondingCurve + trade.solAmount;
      const tradeImpact = Math.min(trade.solAmount / preTradeVSol, 1.0);

      if (state.firstNTradeImpacts.length < 20) {
        state.firstNTradeImpacts.push(tradeImpact);
      }
      if (tradeImpact > 0.05) state.highImpactTradeCount++;
      if (tradeImpact > state.maxImpactRatio) state.maxImpactRatio = tradeImpact;
    }

    // Update derived accumulation fields
    if (state.vSolAtFirstTrade > 0) {
      state.lifetimeVSolAccumulated = trade.vSolInBondingCurve - state.vSolAtFirstTrade;
    }
    if (state.vSolAtObservationStart > 0) {
      state.windowVSolAccumulated = trade.vSolInBondingCurve - state.vSolAtObservationStart;
    }
    state.windowSwapCount = state.totalSwapCount - state.swapCountAtObservationStart;

    // Prune old trades — 120s retention for full pre-entry capital efficiency history
    const cutoff = nowMs() - 120 * 1000;
    state.trades = state.trades.filter(t => t.timestamp >= cutoff);
  }

  /**
   * Snapshot the observation start baseline for window-scoped efficiency metrics.
   * Idempotent: only snapshots once (when vSolAtObservationStart === 0).
   * Called when a token's observation window has elapsed and entry evaluation begins.
   */
  markObservationStart(mint: string): void {
    const state = this.tokenStates.get(mint);
    if (!state) return;
    // Only snapshot once — subsequent calls are no-ops
    if (state.vSolAtObservationStart > 0) return;
    const latestTrade = state.trades[state.trades.length - 1];
    if (latestTrade) {
      state.vSolAtObservationStart = latestTrade.vSolInBondingCurve;
    }
    state.swapCountAtObservationStart = state.totalSwapCount;
    // Initialize window-scoped accumulators from this point forward
    state.windowVSolAccumulated = 0;
    state.windowSwapCount = 0;
  }

  /** Check whether the observation start baseline has been snapshotted */
  isObservationStartMarked(mint: string): boolean {
    const state = this.tokenStates.get(mint);
    return (state?.vSolAtObservationStart ?? 0) > 0;
  }

  /** Set creator wallet context for deep-lane enrichment */
  setCreatorContext(mint: string, ctx: CreatorWalletContext): void {
    const state = this.tokenStates.get(mint);
    if (state) {
      state.creatorContext = ctx;
    }
  }

  /** Set multimodal context for junk filter */
  setMultimodalContext(mint: string, ctx: MultimodalContext): void {
    const state = this.tokenStates.get(mint);
    if (state) {
      state.multimodalContext = ctx;
    }
  }

  /** Update friction context (after execution) */
  updateFrictionContext(mint: string, ctx: Partial<FrictionContext>): void {
    const state = this.tokenStates.get(mint);
    if (state) {
      Object.assign(state.frictionContext, ctx);
    }
  }

  /** Compute full feature snapshot for a token */
  computeFeatures(mint: string): FeatureSnapshot | null {
    const state = this.tokenStates.get(mint);
    if (!state) return null;

    const now = nowMs();

    // Compute each feature family
    const flowMomentum = computeFlowMomentum(
      state.trades, this.windows, now, state.prevVelocities
    );

    const breadthTopology = computeBreadthTopology(
      state.trades, state.uniqueBuyers, state.walletBalances,
      state.creator, this.windows, now
    );

    const creatorWalletPrior = computeCreatorWalletPriors(
      state.creatorContext,
      state.walletBalances,
      state.uniqueBuyers,
      state.creator,
      this.config.features.qualified_wallet_prior
    );

    const frictionExecution = computeFrictionExecution(
      state.frictionContext,
      this.config.execution.route_health
    );

    const manipulationDistribution = computeManipulationDistribution(
      {
        trades: state.trades,
        creator: state.creator,
        walletBalances: state.walletBalances,
        uniqueBuyers: state.uniqueBuyers,
        windows: this.windows,
        now,
        tokenCreatedAt: state.tokenCreatedAt,
      },
      this.config.manipulation
    );

    const multimodalJunk = getMultimodalJunkScore(
      state.multimodalContext,
      this.config.features.multimodal_junk_filter
    );

    // ── Bonding curve dynamics (capital efficiency) ──
    // Sample capital efficiency history every 10s (max 12 samples = 2 min of data)
    const latestHistory = state.capitalEfficiencyHistory;
    const lastSample = latestHistory.length > 0 ? latestHistory[latestHistory.length - 1] : null;
    if (!lastSample || (now - lastSample.timestamp) >= 10000) {
      const ceRaw = state.totalSwapCount > 0
        ? (state.trades[state.trades.length - 1]?.vSolInBondingCurve ?? 0) / state.totalSwapCount
        : 0;
      latestHistory.push({ timestamp: now, value: ceRaw });
      if (latestHistory.length > 12) latestHistory.shift();
    }

    // Window duration: time from earliest retained trade to now
    const windowDurationMs = state.vSolAtObservationStart > 0 && state.trades.length > 0
      ? now - (state.trades[0]?.timestamp ?? now)
      : 0;

    const bcdCtx: BondingCurveDynamicsContext = {
      vSolInBondingCurve: state.trades[state.trades.length - 1]?.vSolInBondingCurve ?? 0,
      totalSwapCount: state.totalSwapCount,
      vSolAtFirstTrade: state.vSolAtFirstTrade,
      windowVSolAccumulated: state.windowVSolAccumulated,
      windowSwapCount: state.windowSwapCount,
      windowDurationMs,
      largeTradeCount: state.largeTradeCount,
      capitalEfficiencyHistory: state.capitalEfficiencyHistory,
      trades: state.trades,
      firstTradesVSolSnapshot: state.firstTradesVSolSnapshot,
      firstTradesCount: state.firstTradesCount,
      firstNTradeImpacts: state.firstNTradeImpacts,
      highImpactTradeCount: state.highImpactTradeCount,
      maxImpactRatio: state.maxImpactRatio,
    };
    const bondingCurveDynamics = computeBondingCurveDynamics(bcdCtx);

    // Store previous velocities for acceleration
    state.prevVelocities.set('buy_velocity_5s', flowMomentum.buy_notional_velocity_5s);
    state.prevVelocities.set('buy_velocity_15s', flowMomentum.buy_notional_velocity_15s);
    state.lastFeatureCompute = now;

    return {
      timestamp: now,
      mint,
      flow_momentum: flowMomentum,
      breadth_topology: breadthTopology,
      creator_wallet_prior: creatorWalletPrior,
      friction_execution: frictionExecution,
      manipulation_distribution: manipulationDistribution,
      multimodal_junk: multimodalJunk,
      bonding_curve_dynamics: bondingCurveDynamics,
      creator_net_sol_position: state.creatorNetSolPosition,
      total_swap_count: state.totalSwapCount,
    };
  }

  /** Remove a token from tracking */
  removeToken(mint: string): void {
    this.tokenStates.delete(mint);
  }

  /** Get tracked token count */
  get trackedTokenCount(): number {
    return this.tokenStates.size;
  }

  /** Check if token is tracked */
  isTracked(mint: string): boolean {
    return this.tokenStates.has(mint);
  }

  /** Get trade count for a token */
  getTradeCount(mint: string): number {
    return this.tokenStates.get(mint)?.trades.length ?? 0;
  }
}
