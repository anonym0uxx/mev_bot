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
  initToken(mint: string, creator: string): void {
    if (this.tokenStates.has(mint)) return;

    this.tokenStates.set(mint, {
      mint,
      creator,
      trades: [],
      uniqueBuyers: new Set(),
      walletBalances: new Map(),
      prevVelocities: new Map(),
      creatorContext: null,
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
    const currentBalance = state.walletBalances.get(trade.traderPublicKey) || 0;
    state.walletBalances.set(trade.traderPublicKey, trade.newTokenBalance);

    // Prune old trades (keep max 30s window + buffer)
    const maxWindow = Math.max(...this.windows);
    const cutoff = nowMs() - (maxWindow + 5) * 1000;
    state.trades = state.trades.filter(t => t.timestamp >= cutoff);
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
      },
      this.config.manipulation
    );

    const multimodalJunk = getMultimodalJunkScore(
      state.multimodalContext,
      this.config.features.multimodal_junk_filter
    );

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
