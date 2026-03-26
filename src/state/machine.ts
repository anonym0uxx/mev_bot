/**
 * @module state/machine
 * Token state machine: OBSERVE → WATCH → ENTER_READY → LONG → REDUCE → EXIT → BAN
 * All transitions per spec section 14. Persists transitions to DB.
 */

import { EventEmitter } from 'events';
import { v4 as uuidv4 } from 'uuid';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import {
  TokenState, Regime, CandidatePacket,
  ProbabilityOutputs, EntryEVCalculation, ExitEVCalculation, PositionSizing,
} from '../types/state';
import { FeatureSnapshot } from '../types/features';
import { PumpQuantConfig } from '../types/config';
import { PumpQuantDB } from '../persistence/database';
import { getConfigVersion } from '../config/loader';

const log = createLogger('state-machine');

export interface StateMachineEvents {
  stateChange: (mint: string, fromState: TokenState, toState: TokenState, reason: string) => void;
  enterReady: (packet: CandidatePacket) => void;
  exitSignal: (mint: string, reason: string) => void;
  ban: (mint: string, reason: string) => void;
}

export class TokenStateMachine extends EventEmitter {
  /** In-memory packet store: mint → CandidatePacket */
  private packets: Map<string, CandidatePacket> = new Map();

  constructor(
    private db: PumpQuantDB,
    private config: PumpQuantConfig
  ) {
    super();
  }

  /** Update config (on version change) */
  updateConfig(config: PumpQuantConfig): void {
    this.config = config;
  }

  /** Get or create a candidate packet for a token */
  getPacket(mint: string): CandidatePacket | undefined {
    return this.packets.get(mint);
  }

  /** Get all packets in a given state */
  getPacketsByState(state: TokenState): CandidatePacket[] {
    return Array.from(this.packets.values()).filter(p => p.state === state);
  }

  /** Get all active packets (not EXIT or BAN) */
  getActivePackets(): CandidatePacket[] {
    return Array.from(this.packets.values())
      .filter(p => p.state !== TokenState.EXIT && p.state !== TokenState.BAN);
  }

  /** Get top candidates by entry edge, in ENTER_READY or WATCH */
  getTopCandidates(limit: number = 10): CandidatePacket[] {
    return Array.from(this.packets.values())
      .filter(p => p.state === TokenState.ENTER_READY || p.state === TokenState.WATCH)
      .filter(p => p.entry_ev !== null)
      .sort((a, b) => (b.entry_ev?.EntryEdge ?? 0) - (a.entry_ev?.EntryEdge ?? 0))
      .slice(0, limit);
  }

  /** Initialize a new token in OBSERVE state */
  initToken(
    mint: string,
    symbol: string,
    name: string,
    creator: string,
    bondingCurveKey: string,
    uri: string,
    regime: Regime,
    vTokensInCurve: number,
    vSolInCurve: number,
    marketCapSol: number,
    bondingCurveProgress: number
  ): CandidatePacket {
    const now = nowMs();
    const packet: CandidatePacket = {
      id: uuidv4(),
      mint,
      symbol,
      name,
      creator,
      created_at: now,
      bonding_curve_key: bondingCurveKey,
      state: TokenState.OBSERVE,
      regime,
      v_tokens_in_curve: vTokensInCurve,
      v_sol_in_curve: vSolInCurve,
      market_cap_sol: marketCapSol,
      bonding_curve_progress: bondingCurveProgress,
      features: this.emptyFeatures(mint, now),
      probabilities: this.emptyProbabilities(),
      entry_ev: null,
      exit_ev: null,
      sizing: null,
      config_version: getConfigVersion(),
      first_seen: now,
      last_updated: now,
      state_entered_at: now,
      ban_reason: null,
      uri,
      metadata_fetched: false,
    };

    this.packets.set(mint, packet);
    this.db.upsertTokenState(packet);
    return packet;
  }

  /** Update token market data (from trade events) */
  updateMarketData(
    mint: string,
    vTokensInCurve: number,
    vSolInCurve: number,
    marketCapSol: number,
    bondingCurveProgress: number
  ): void {
    const packet = this.packets.get(mint);
    if (!packet) return;

    packet.v_tokens_in_curve = vTokensInCurve;
    packet.v_sol_in_curve = vSolInCurve;
    packet.market_cap_sol = marketCapSol;
    packet.bonding_curve_progress = bondingCurveProgress;
    packet.last_updated = nowMs();
  }

  /** Update features and probabilities for a token */
  updateAnalysis(
    mint: string,
    features: FeatureSnapshot,
    probabilities: ProbabilityOutputs,
    regime: Regime,
    entryEv: EntryEVCalculation | null,
    exitEv: ExitEVCalculation | null,
    sizing: PositionSizing | null
  ): void {
    const packet = this.packets.get(mint);
    if (!packet) return;

    packet.features = features;
    packet.probabilities = probabilities;
    packet.regime = regime;
    packet.entry_ev = entryEv;
    packet.exit_ev = exitEv;
    packet.sizing = sizing;
    packet.config_version = getConfigVersion();
    packet.last_updated = nowMs();

    // Persist snapshot
    this.db.upsertTokenState(packet);
  }

  // ====== STATE TRANSITIONS ======

  /**
   * OBSERVE → WATCH: when enough data exists and token not excluded.
   */
  transitionToWatch(mint: string, reason: string): boolean {
    const packet = this.packets.get(mint);
    if (!packet || packet.state !== TokenState.OBSERVE) return false;
    if (packet.regime === Regime.EXCLUDED) return false;

    return this.transition(packet, TokenState.WATCH, reason);
  }

  /**
   * WATCH → ENTER_READY: when hard filters pass and EntryEdge > threshold.
   */
  transitionToEnterReady(mint: string, reason: string): boolean {
    const packet = this.packets.get(mint);
    if (!packet || packet.state !== TokenState.WATCH) return false;

    const ok = this.transition(packet, TokenState.ENTER_READY, reason);
    if (ok) {
      this.emit('enterReady', packet);
    }
    return ok;
  }

  /**
   * ENTER_READY → LONG: only when buy_token is explicitly called.
   */
  transitionToLong(mint: string, reason: string): boolean {
    const packet = this.packets.get(mint);
    if (!packet || packet.state !== TokenState.ENTER_READY) return false;

    return this.transition(packet, TokenState.LONG, reason);
  }

  /**
   * LONG → REDUCE: when HoldEdge weakens materially but not catastrophic.
   */
  transitionToReduce(mint: string, reason: string): boolean {
    const packet = this.packets.get(mint);
    if (!packet || packet.state !== TokenState.LONG) return false;

    const ok = this.transition(packet, TokenState.REDUCE, reason);
    if (ok) {
      this.emit('exitSignal', mint, reason);
    }
    return ok;
  }

  /**
   * LONG|REDUCE → EXIT: when EV_exit >= EV_hold or override trips.
   */
  transitionToExit(mint: string, reason: string): boolean {
    const packet = this.packets.get(mint);
    if (!packet || (packet.state !== TokenState.LONG && packet.state !== TokenState.REDUCE)) return false;

    const ok = this.transition(packet, TokenState.EXIT, reason);
    if (ok) {
      this.emit('exitSignal', mint, reason);
    }
    return ok;
  }

  /**
   * ANY → BAN: manipulation/system fault/policy exclusion.
   */
  transitionToBan(mint: string, reason: string): boolean {
    const packet = this.packets.get(mint);
    if (!packet || packet.state === TokenState.BAN) return false;

    packet.ban_reason = reason;
    const ok = this.transition(packet, TokenState.BAN, reason);
    if (ok) {
      this.emit('ban', mint, reason);
      // If in LONG or REDUCE, also emit exit signal for forced exit
      if (packet.state === TokenState.LONG || packet.state === TokenState.REDUCE) {
        this.emit('exitSignal', mint, `BAN: ${reason}`);
      }
    }
    return ok;
  }

  /** Generic transition helper */
  private transition(packet: CandidatePacket, newState: TokenState, reason: string): boolean {
    const oldState = packet.state;
    const configVersion = getConfigVersion();

    log.info(`State transition: ${packet.mint} ${oldState} → ${newState}: ${reason}`);

    packet.state = newState;
    packet.state_entered_at = nowMs();
    packet.last_updated = nowMs();
    packet.config_version = configVersion;

    // Persist
    this.db.upsertTokenState(packet);
    this.db.insertStateTransition(packet.mint, oldState, newState, reason, configVersion);

    this.emit('stateChange', packet.mint, oldState, newState, reason);
    return true;
  }

  /** Remove token from in-memory tracking (after EXIT/BAN) */
  cleanup(mint: string): void {
    this.packets.delete(mint);
  }

  /** Get total tracked token count */
  get trackedCount(): number {
    return this.packets.size;
  }

  /** Create empty feature snapshot */
  private emptyFeatures(mint: string, timestamp: number): FeatureSnapshot {
    return {
      timestamp,
      mint,
      flow_momentum: {
        buy_notional_velocity_1s: 0, buy_notional_velocity_5s: 0,
        buy_notional_velocity_15s: 0, buy_notional_velocity_30s: 0,
        trade_count_velocity_1s: 0, trade_count_velocity_5s: 0,
        trade_count_velocity_15s: 0, trade_count_velocity_30s: 0,
        buy_velocity_acceleration_5s: 0, buy_velocity_acceleration_15s: 0,
        curve_progress_acceleration_5s: 0, curve_progress_acceleration_15s: 0,
        buy_sell_imbalance_5s: 0, buy_sell_imbalance_15s: 0, buy_sell_imbalance_30s: 0,
        avg_trade_size_5s: 0, avg_trade_size_15s: 0,
        size_dispersion_5s: 0, size_dispersion_15s: 0,
      },
      breadth_topology: {
        unique_buyers_growth_5s: 0, unique_buyers_growth_15s: 0,
        unique_buyers_total: 0, repeat_wallet_ratio: 0, fresh_wallet_ratio: 0,
        non_dev_participation: 0, first_100_persistence: 0,
        top_10_concentration: 0, top_20_concentration: 0, breadth_score: 0,
      },
      creator_wallet_prior: {
        creator_history_score: 0, creator_sell_flag: false,
        creator_holdings_trend: 0, qualified_wallet_score: 0,
        top_trader_score: 0, first_100_persistence_contribution: 0,
        dispersion_quality_score: 0, distribution_penalty: 0, composite_prior: 0,
      },
      friction_execution: {
        expected_entry_slippage: 0, expected_exit_slippage: 0,
        route_mode: 'local', priority_fee_burden: 0,
        landing_risk_estimate: 0, retry_failure_rate: 0,
        execution_freshness_s: 0, route_score: 0,
        route_ev_adjustment: 0, route_health_prior: 0,
        latency_budget_utilization: 0,
      },
      manipulation_distribution: {
        creator_sell: false, same_size_print_count: 0,
        price_breadth_divergence: 0, concentration_worsening: 0,
        cluster_correlation: 0, suspicious_burst: 0,
        slippage_shock: 0, distribution_signatures: 0,
        manipulation_penalty: 0, hard_shock: false,
      },
      multimodal_junk: {
        ticker_clarity: 0.5, name_clarity: 0.5,
        logo_presence: 0.5, logo_quality: 0.5,
        metadata_spam: 0.5, comment_entropy: 0.5,
        social_pickup: 0, junk_score: 0.5, is_stale: true,
      },
      bonding_curve_dynamics: {
        capital_efficiency_raw: 0, capital_efficiency_normalized: 0,
        window_capital_efficiency: 0, efficiency_trend: 0.5,
        curve_fill_rate_sol_per_min: 0, curve_fill_rate_normalized: 0,
        large_trade_fraction: 0, median_trade_size_sol: 0,
        median_trade_size_normalized: 0,
        initial_capital_efficiency: 0.5,   // neutral (insufficient data)
        accumulation_shape: 0.5,           // neutral
        initial_burst_impact: 0,
        high_impact_fraction: 0,
        max_impact_ratio_normalized: 0,
        organic_diversity_score: 0.5,      // neutral
        bcd_score: 0,
      },
      creator_net_sol_position: 0,
      total_swap_count: 0,
    };
  }

  /** Create empty probability outputs */
  private emptyProbabilities(): ProbabilityOutputs {
    return {
      P_continuation_5s: 0.5,
      P_continuation_15s: 0.5,
      P_reversal_5s: 0.5,
      P_reversal_15s: 0.5,
      P_manipulation_event: 0,
    };
  }
}
