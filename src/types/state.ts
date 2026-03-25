/**
 * @module types/state
 * Token state machine and candidate packet types
 */

import { FeatureSnapshot } from './features';
import { RouteMode } from './config';

/** Token lifecycle states (section 14) */
export enum TokenState {
  OBSERVE = 'OBSERVE',
  WATCH = 'WATCH',
  ENTER_READY = 'ENTER_READY',
  LONG = 'LONG',
  REDUCE = 'REDUCE',
  EXIT = 'EXIT',
  BAN = 'BAN',
}

/** Regime classification (section 5) */
export enum Regime {
  EXCLUDED = 'EXCLUDED',
  EARLY_CURVE = 'EARLY_CURVE',
  MID_CURVE = 'MID_CURVE',
  LATE_CURVE = 'LATE_CURVE',
  GRADUATION_BOUNDARY = 'GRADUATION_BOUNDARY',
  POST_MIGRATION = 'POST_MIGRATION',
}

/** Probability outputs (section 8) */
export interface ProbabilityOutputs {
  P_continuation_5s: number;
  P_continuation_15s: number;
  P_reversal_5s: number;
  P_reversal_15s: number;
  P_manipulation_event: number;
}

/** EV calculations for entry (section 9.3) */
export interface EntryEVCalculation {
  EV_enter_now: number;
  EV_wait: number;
  EntryEdge: number;
  upside_net: number;
  downside_net: number;
  manipulation_cost: number;
  friction_cost_now: number;
  route_ev_adjustment: number;
}

/** EV calculations for exit (section 10.4) */
export interface ExitEVCalculation {
  ExpectedNetExitNow: number;
  EV_hold_h: number;
  HoldEdge: number;
  PeakNetExitValue: number;
  NetRetrace: number;
  dynamic_retrace_threshold: number;
  time_decay_pressure: number;
  upside_if_hold: number;
  downside_if_hold: number;
  shock_cost: number;
  extra_friction_if_hold: number;
}

/** Position sizing calculation (section 9.5) */
export interface PositionSizing {
  risk_budget: number;
  effective_stop_pct: number;
  raw_position_size: number;
  position_size: number;
  limiting_factor: 'risk_budget' | 'quick_spend' | 'max_alloc' | 'liquidity_cap' | 'slippage_cap';
}

/** Candidate packet: full decision context for a token */
export interface CandidatePacket {
  id: string;
  mint: string;
  symbol: string;
  name: string;
  creator: string;
  created_at: number;
  bonding_curve_key: string;

  /** Current state */
  state: TokenState;
  regime: Regime;

  /** Bonding curve data */
  v_tokens_in_curve: number;
  v_sol_in_curve: number;
  market_cap_sol: number;
  bonding_curve_progress: number;

  /** Latest feature snapshot */
  features: FeatureSnapshot;

  /** Probability outputs */
  probabilities: ProbabilityOutputs;

  /** Entry EV (only relevant in WATCH / ENTER_READY) */
  entry_ev: EntryEVCalculation | null;

  /** Exit EV (only relevant in LONG / REDUCE) */
  exit_ev: ExitEVCalculation | null;

  /** Position sizing (only relevant in ENTER_READY) */
  sizing: PositionSizing | null;

  /** Config version used for this snapshot */
  config_version: number;

  /** Timestamps */
  first_seen: number;
  last_updated: number;
  state_entered_at: number;

  /** Ban reason if applicable */
  ban_reason: string | null;

  /** Metadata for multimodal junk filter */
  uri: string;
  metadata_fetched: boolean;
}

/** Token state transition record */
export interface StateTransition {
  mint: string;
  from_state: TokenState;
  to_state: TokenState;
  reason: string;
  timestamp: number;
  config_version: number;
  features_snapshot_id: string | null;
}

/** Tier classification for compute budget */
export enum AnalysisTier {
  /** Discovery and instant exclusions */
  TIER_0 = 0,
  /** Live incremental scoring on shortlisted tokens */
  TIER_1 = 1,
  /** Sparse deep enrichment for top candidates and active positions */
  TIER_2 = 2,
}

/** Exit reason categories */
export enum ExitReason {
  /** EV_exit >= EV_hold */
  EV_NEGATIVE = 'ev_negative',
  /** Creator sold */
  CREATOR_SELL = 'creator_sell',
  /** Slippage shock */
  SLIPPAGE_SHOCK = 'slippage_shock',
  /** Execution path failure */
  EXECUTION_FAILURE = 'execution_failure',
  /** Manipulation shock */
  MANIPULATION_SHOCK = 'manipulation_shock',
  /** Concentration shock */
  CONCENTRATION_SHOCK = 'concentration_shock',
  /** Peak net retrace exceeded threshold */
  PEAK_RETRACE = 'peak_retrace',
  /** Max hold time exceeded */
  TIME_DECAY = 'time_decay',
  /** Operator requested */
  OPERATOR = 'operator',
  /** System health degraded */
  SYSTEM_HEALTH = 'system_health',
}
