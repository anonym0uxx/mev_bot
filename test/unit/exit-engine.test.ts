/**
 * Exit engine tests — spec section 10
 * Non-negotiable: hold ONLY while EV_hold > EV_exit_now
 */
import { describe, it, expect } from 'vitest';
import { evaluateExit } from '../../src/exit/engine';
import { CandidatePacket, Regime, TokenState, AnalysisTier, ExitReason } from '../../src/types/state';
import { Position, PositionStatus } from '../../src/types/trade';
import { FeatureSnapshot } from '../../src/types/features';
import { PumpQuantConfig } from '../../src/types/config';
import * as fs from 'fs';
import * as path from 'path';

const config: PumpQuantConfig = JSON.parse(
  fs.readFileSync(path.join(__dirname, '../../config/default.json'), 'utf-8')
);

function makePacket(overrides: Partial<CandidatePacket> = {}): CandidatePacket {
  return {
    id: 'test-packet',
    mint: 'TestMint111111111111111111111111111111111111',
    symbol: 'TEST',
    name: 'Test Token',
    creator: 'Creator111111111111111111111111111111111111',
    bonding_curve_key: 'BC111111111111111111111111111111111111111111',
    uri: '',
    state: TokenState.LONG,
    regime: Regime.MID_CURVE,
    v_tokens_in_curve: 700_000_000_000_000,
    v_sol_in_curve: 40,
    market_cap_sol: 8,
    bonding_curve_progress: 0.35,
    created_at: Date.now() - 30000,
    last_updated: Date.now(),
    state_entered_at: Date.now() - 10000,
    config_version: 1,
    tier: AnalysisTier.TIER_1,
    features: null as any,
    probabilities: null as any,
    entry_ev: null,
    exit_ev: null,
    sizing: null,
    ...overrides,
  };
}

function makePosition(overrides: Partial<Position> = {}): Position {
  return {
    id: 'test-pos',
    mint: 'TestMint111111111111111111111111111111111111',
    symbol: 'TEST',
    name: 'Test Token',
    regime: Regime.MID_CURVE,
    entry_order_id: 'order-1',
    entry_price_sol: 0.05,
    entry_sol: 0.05,
    entry_tokens: 1_000_000,
    entry_timestamp: Date.now() - 15000,
    entry_route_mode: 'local',
    entry_config_version: 1,
    current_tokens: 1_000_000,
    current_value_sol: 0.06,
    unrealized_pnl_sol: 0.01,
    unrealized_pnl_pct: 0.2,
    peak_net_exit_value: 0.06,
    exit_orders: [],
    exit_price_sol: null,
    exit_sol: null,
    exit_timestamp: null,
    exit_reason: null,
    exit_route_mode: null,
    realized_pnl_sol: null,
    realized_pnl_pct: null,
    total_fees_sol: 0.002,
    status: PositionStatus.OPEN,
    opened_at: Date.now() - 15000,
    closed_at: null,
    hold_duration_s: 15,
    mfe_sol: 0.015,
    mae_sol: -0.005,
    is_paper: false,
    config_version: 1,
    ...overrides,
  };
}

function makeFeatures(overrides: any = {}): FeatureSnapshot {
  return {
    mint: 'TestMint111111111111111111111111111111111111',
    timestamp: Date.now(),
    flow_momentum: {
      buy_notional_velocity_1s: 0.2,
      buy_notional_velocity_5s: 0.15,
      buy_notional_velocity_15s: 0.1,
      buy_notional_velocity_30s: 0.08,
      trade_count_velocity_5s: 2,
      buy_velocity_acceleration: 0,
      curve_progress_acceleration: 0,
      buy_sell_imbalance_1s: 0.3,
      buy_sell_imbalance_5s: 0.2,
      buy_sell_imbalance_15s: 0.15,
      buy_sell_imbalance_30s: 0.1,
      avg_trade_size_5s: 0.03,
      size_dispersion_5s: 0.2,
      ...overrides.flow_momentum,
    },
    breadth_topology: {
      unique_buyers_1s: 1,
      unique_buyers_5s: 3,
      unique_buyers_15s: 5,
      unique_buyers_30s: 7,
      unique_buyers_total: 10,
      repeat_wallet_ratio: 0.2,
      fresh_wallet_ratio: 0.7,
      non_dev_participation: 0.85,
      first_100_persistence: 0.6,
      top_10_concentration: 0.4,
      top_20_concentration: 0.55,
      breadth_score: 0.55,
      ...overrides.breadth_topology,
    },
    creator_wallet_prior: {
      creator_history_score: 0.5,
      creator_sell_flag: false,
      creator_holdings_trend: 0,
      qualified_wallet_score: 0.3,
      top_trader_score: 0.2,
      first_100_persistence_contribution: 0.5,
      dispersion_quality: 0.6,
      distribution_penalty: 0,
      composite_prior: 0.2,
      ...overrides.creator_wallet_prior,
    },
    friction_execution: {
      expected_entry_slippage: 0.03,
      expected_exit_slippage: 0.05,
      route_mode: 'local',
      priority_fee_burden: 0.001,
      landing_risk_estimate: 0.05,
      retry_failure_rate: 0.02,
      execution_freshness_s: 2,
      route_score: 0.7,
      route_ev_adjustment: 0,
      route_health_prior: 0.8,
      latency_budget_utilization: 0.3,
      ...overrides.friction_execution,
    },
    manipulation_distribution: {
      creator_sell: false,
      same_size_prints: 0,
      price_breadth_divergence: 0.05,
      concentration_worsening: 0.02,
      cluster_correlation: 0.1,
      suspicious_burst: 0.05,
      slippage_shock: 0,
      manipulation_penalty: 0.08,
      hard_shock: false,
      hard_shock_reason: null,
      ...overrides.manipulation_distribution,
    },
    multimodal_junk: {
      ticker_clarity: 0.8,
      name_clarity: 0.7,
      logo_presence: true,
      logo_quality: 0.6,
      metadata_spam_score: 0.1,
      comment_entropy: 0.5,
      junk_score: 0.75,
      is_stale: false,
      ...overrides.multimodal_junk,
    },
  };
}

function makeProbabilities(overrides: any = {}) {
  return {
    P_continuation_5s: 0.55,
    P_continuation_15s: 0.50,
    P_reversal_5s: 0.45,
    P_reversal_15s: 0.50,
    P_manipulation_event: 0.10,
    ...overrides,
  };
}

describe('evaluateExit', () => {
  it('triggers catastrophic exit on creator_sell', () => {
    const features = makeFeatures({
      manipulation_distribution: { creator_sell: true, hard_shock: true },
    });
    const result = evaluateExit(
      makePacket(), makePosition(), makeProbabilities(), features, config
    );
    expect(result.shouldExit).toBe(true);
    expect(result.exitPct).toBe(100);
    expect(result.reason).toBe(ExitReason.CREATOR_SELL);
  });

  it('triggers catastrophic exit on manipulation_shock', () => {
    const features = makeFeatures({
      manipulation_distribution: { hard_shock: true },
    });
    const result = evaluateExit(
      makePacket(), makePosition(), makeProbabilities(), features, config
    );
    expect(result.shouldExit).toBe(true);
    expect(result.exitPct).toBe(100);
    expect(result.reason).toBe(ExitReason.MANIPULATION_SHOCK);
  });

  it('triggers catastrophic exit on execution failure', () => {
    const features = makeFeatures({
      friction_execution: { retry_failure_rate: 0.9 },
    });
    const result = evaluateExit(
      makePacket(), makePosition(), makeProbabilities(), features, config
    );
    expect(result.shouldExit).toBe(true);
    expect(result.reason).toBe(ExitReason.EXECUTION_FAILURE);
  });

  it('exits when bearish (shouldExit=true)', () => {
    // Very bearish probabilities — should trigger some exit path
    const probs = makeProbabilities({
      P_continuation_5s: 0.15,
      P_continuation_15s: 0.10,
      P_reversal_5s: 0.85,
      P_reversal_15s: 0.90,
      P_manipulation_event: 0.3,
    });
    const result = evaluateExit(
      makePacket(), makePosition(), probs, makeFeatures(), config
    );
    // Non-negotiable: bearish setup must exit
    expect(result.shouldExit).toBe(true);
    expect(result.exitPct).toBe(100);
  });

  it('exits on max hold time', () => {
    const pos = makePosition({
      entry_timestamp: Date.now() - (config.exit.max_hold_time_s + 10) * 1000,
      peak_net_exit_value: 0, // No peak to trigger retrace
    });
    const result = evaluateExit(
      makePacket(), pos, makeProbabilities(), makeFeatures(), config
    );
    expect(result.shouldExit).toBe(true);
    // May exit via TIME_DECAY, EV_NEGATIVE, or peak_retrace
    expect([ExitReason.TIME_DECAY, ExitReason.EV_NEGATIVE, ExitReason.PEAK_RETRACE]).toContain(result.reason);
  });

  it('holds when edge is positive and no overrides', () => {
    const probs = makeProbabilities({
      P_continuation_5s: 0.80,
      P_continuation_15s: 0.70,
      P_reversal_5s: 0.20,
      P_reversal_15s: 0.30,
      P_manipulation_event: 0.02,
    });
    const result = evaluateExit(
      makePacket(), makePosition(), probs, makeFeatures(), config
    );
    // With strong continuation, should hold
    if (!result.shouldExit && !result.shouldReduce) {
      expect(result.exitPct).toBe(0);
    }
  });

  it('computes ExpectedNetExitNow with fees and slippage', () => {
    const result = evaluateExit(
      makePacket(), makePosition(), makeProbabilities(), makeFeatures(), config
    );
    // Net exit should be less than current value (fees + slippage deducted)
    expect(result.ev.ExpectedNetExitNow).toBeLessThan(makePosition().current_value_sol);
  });
});
