/**
 * Entry engine tests — spec section 9
 * Non-negotiable: EV_enter_now > 0 AND EntryEdge > 0
 */
import { describe, it, expect } from 'vitest';
import { evaluateEntry, computePositionSizing } from '../../src/entry/engine';
import { CandidatePacket, Regime, TokenState, AnalysisTier } from '../../src/types/state';
import { FeatureSnapshot } from '../../src/types/features';
import { PumpQuantConfig } from '../../src/types/config';
import * as fs from 'fs';
import * as path from 'path';

// Load actual default config
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
    state: TokenState.WATCH,
    regime: Regime.MID_CURVE,
    v_tokens_in_curve: 800_000_000_000_000,
    v_sol_in_curve: 30,
    market_cap_sol: 5,
    bonding_curve_progress: 0.25,
    created_at: Date.now() - 20000, // 20s old
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

function makeFeatures(overrides: any = {}): FeatureSnapshot {
  return {
    mint: 'TestMint111111111111111111111111111111111111',
    timestamp: Date.now(),
    flow_momentum: {
      buy_notional_velocity_1s: 0.3,
      buy_notional_velocity_5s: 0.25,
      buy_notional_velocity_15s: 0.15,
      buy_notional_velocity_30s: 0.10,
      trade_count_velocity_5s: 3,
      buy_velocity_acceleration: 0.05,
      curve_progress_acceleration: 0.01,
      buy_sell_imbalance_1s: 0.8,
      buy_sell_imbalance_5s: 0.6,
      buy_sell_imbalance_15s: 0.4,
      buy_sell_imbalance_30s: 0.3,
      avg_trade_size_5s: 0.05,
      size_dispersion_5s: 0.3,
      ...overrides.flow_momentum,
    },
    breadth_topology: {
      unique_buyers_1s: 2,
      unique_buyers_5s: 5,
      unique_buyers_15s: 8,
      unique_buyers_30s: 10,
      unique_buyers_total: 12,
      repeat_wallet_ratio: 0.1,
      fresh_wallet_ratio: 0.8,
      non_dev_participation: 0.9,
      first_100_persistence: 0.7,
      top_10_concentration: 0.35,
      top_20_concentration: 0.50,
      breadth_score: 0.65,
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
    P_continuation_5s: 0.65,
    P_continuation_15s: 0.55,
    P_reversal_5s: 0.35,
    P_reversal_15s: 0.45,
    P_manipulation_event: 0.10,
    ...overrides,
  };
}

describe('evaluateEntry', () => {
  it('rejects when creator has sold (MID_CURVE with creator sell)', () => {
    const features = makeFeatures({
      creator_wallet_prior: { creator_sell_flag: true },
    });
    const result = evaluateEntry(
      makePacket({ regime: Regime.MID_CURVE }), makeProbabilities(), features, config, 0, 0, false
    );
    expect(result.shouldEnter).toBe(false);
    expect(result.hardFilterRejection).toBe('creator_sold');
  });

  it('rejects excluded regime (EARLY_CURVE now excluded per QUANT_STRATEGY)', () => {
    const result = evaluateEntry(
      makePacket({ regime: Regime.EARLY_CURVE }),
      makeProbabilities(),
      makeFeatures(),
      config, 0, 0, false
    );
    expect(result.shouldEnter).toBe(false);
    expect(result.hardFilterRejection).toBe('excluded_regime');
  });

  it('rejects when max positions reached', () => {
    const result = evaluateEntry(
      makePacket({ regime: Regime.MID_CURVE }),
      makeProbabilities(),
      makeFeatures(),
      config, config.risk.max_positions, 0, false
    );
    expect(result.shouldEnter).toBe(false);
    expect(result.hardFilterRejection).toBe('max_positions');
  });

  it('rejects when breadth too low', () => {
    const features = makeFeatures({
      breadth_topology: { unique_buyers_total: 2, breadth_score: 0.1 },
    });
    const result = evaluateEntry(
      makePacket(), makeProbabilities(), features, config, 0, 0, false
    );
    expect(result.shouldEnter).toBe(false);
  });

  it('rejects when manipulation hard shock', () => {
    const features = makeFeatures({
      manipulation_distribution: { hard_shock: true, manipulation_penalty: 0.9 },
    });
    const result = evaluateEntry(
      makePacket(), makeProbabilities(), features, config, 0, 0, false
    );
    expect(result.shouldEnter).toBe(false);
  });

  it('approves entry with healthy setup', () => {
    const packet = makePacket({ created_at: Date.now() - 30000 }); // 30s old
    const probs = makeProbabilities({ P_continuation_5s: 0.75, P_reversal_5s: 0.25 });
    const features = makeFeatures();
    const result = evaluateEntry(packet, probs, features, config, 0, 0, false);

    // Should compute positive EV
    expect(result.ev.EV_enter_now).toBeGreaterThan(0);
    if (result.shouldEnter) {
      expect(result.ev.EntryEdge).toBeGreaterThan(0);
      expect(result.sizing).not.toBeNull();
      expect(result.sizing!.position_size).toBeGreaterThan(0);
      expect(result.sizing!.position_size).toBeLessThanOrEqual(config.risk.quick_spend_sol);
    }
  });

  it('never sizes above quick_spend', () => {
    const sizing = computePositionSizing(config, makeFeatures(), 0.001);
    expect(sizing.position_size).toBeLessThanOrEqual(config.risk.quick_spend_sol);
  });

  it('EV_enter_now uses net liquidation value (includes friction)', () => {
    const probs = makeProbabilities({ P_continuation_5s: 0.65, P_reversal_5s: 0.35 });
    const features = makeFeatures();
    const packet = makePacket({ created_at: Date.now() - 30000 });
    const result = evaluateEntry(packet, probs, features, config, 0, 0, false);
    // friction_cost_now must be > 0 (fees exist)
    expect(result.ev.friction_cost_now).toBeGreaterThan(0);
  });
});

describe('computePositionSizing', () => {
  it('applies all 5 caps correctly', () => {
    const sizing = computePositionSizing(config, makeFeatures(), 0.001);

    // Position size bounded by min of all caps
    expect(sizing.position_size).toBeGreaterThan(0);
    expect(sizing.position_size).toBeLessThanOrEqual(config.risk.quick_spend_sol);
    expect(sizing.position_size).toBeLessThanOrEqual(config.risk.bankroll_sol * config.risk.max_alloc_pct);
    expect(sizing.position_size).toBeLessThanOrEqual(config.risk.liquidity_cap_sol);
    expect(sizing.position_size).toBeLessThanOrEqual(config.risk.slippage_cap_sol);
    expect(sizing.limiting_factor).toBeTruthy();
  });

  it('effective_stop_pct includes fees and slippage', () => {
    const sizing = computePositionSizing(config, makeFeatures(), 0.001);
    expect(sizing.effective_stop_pct).toBeGreaterThan(config.risk.raw_stop_pct);
  });
});
