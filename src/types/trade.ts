/**
 * @module types/trade
 * Trade execution, position, and order types
 */

import { RouteMode } from './config';
import { ExitReason, Regime } from './state';

/** Trade intent (pre-execution) */
export interface TradeIntent {
  id: string;
  mint: string;
  side: 'buy' | 'sell';
  size_sol: number;
  amount_pct?: number;
  slippage_bps: number;
  priority_fee_sol: number;
  route_mode: RouteMode;
  reason: string;
  config_version: number;
  created_at: number;
  /** Entry or exit EV at time of intent */
  ev_at_intent: number;
}

/** Order (submitted to execution layer) */
export interface Order {
  id: string;
  trade_intent_id: string;
  mint: string;
  side: 'buy' | 'sell';
  size_sol: number;
  amount_pct?: number;
  slippage_bps: number;
  priority_fee_sol: number;
  route_mode: RouteMode;
  status: OrderStatus;
  /** Solana transaction signature */
  tx_signature: string | null;
  /** Timestamps */
  created_at: number;
  sent_at: number | null;
  confirmed_at: number | null;
  /** Realized fill data */
  realized_sol: number | null;
  realized_tokens: number | null;
  realized_price: number | null;
  realized_slippage_pct: number | null;
  /** Fees paid */
  fee_sol: number | null;
  priority_fee_paid_sol: number | null;
  /** Error info */
  error: string | null;
  retry_count: number;
  config_version: number;
  /** Whether this is a paper/synthetic fill */
  is_paper: boolean;
}

export enum OrderStatus {
  PENDING = 'pending',
  SENT = 'sent',
  CONFIRMED = 'confirmed',
  FAILED = 'failed',
  EXPIRED = 'expired',
}

/** Active position */
export interface Position {
  id: string;
  mint: string;
  symbol: string;
  name: string;
  regime: Regime;

  /** Entry data */
  entry_order_id: string;
  entry_price_sol: number;
  entry_sol: number;
  entry_tokens: number;
  entry_timestamp: number;
  entry_route_mode: RouteMode;
  entry_config_version: number;

  /** Current state */
  current_tokens: number;
  current_value_sol: number;
  unrealized_pnl_sol: number;
  unrealized_pnl_pct: number;

  /** Peak tracking for retrace protection */
  peak_net_exit_value: number;

  /** Exit data (filled on close) */
  exit_orders: string[];
  exit_price_sol: number | null;
  exit_sol: number | null;
  exit_timestamp: number | null;
  exit_reason: ExitReason | null;
  exit_route_mode: RouteMode | null;

  /** PnL (filled on close) */
  realized_pnl_sol: number | null;
  realized_pnl_pct: number | null;
  total_fees_sol: number;

  /** State */
  status: PositionStatus;
  opened_at: number;
  closed_at: number | null;
  hold_duration_s: number;

  /** Max favorable/adverse excursion */
  mfe_sol: number;
  mae_sol: number;

  /** Paper mode */
  is_paper: boolean;

  config_version: number;
}

export enum PositionStatus {
  OPEN = 'open',
  REDUCING = 'reducing',
  CLOSED = 'closed',
}

/** Learning ledger record (section 22A) */
export interface LearningLedgerRecord {
  id: string;
  timestamp: number;
  event_type: 'entry' | 'exit' | 'reduce' | 'reject' | 'ban' | 'forced_exit';
  mint: string;
  regime: Regime;
  config_version: number;
  route_mode: RouteMode;

  /** Feature snapshot at decision time */
  feature_snapshot: string; // JSON serialized FeatureSnapshot

  /** Candidate packet at decision time */
  candidate_packet_id: string;

  /** Realized data */
  realized_fill_quality: number | null;
  realized_pnl_sol: number | null;
  mfe_sol: number | null;
  mae_sol: number | null;

  /** Lane agreement */
  fast_lane_decision: string;
  deep_lane_decision: string | null;
  lane_agreement: boolean;

  /** Exit timing quality [-1 = too early, 0 = optimal, 1 = too late] */
  exit_timing_quality: number | null;

  /** Reject regret: would the rejected trade have been profitable? */
  reject_regret: number | null;

  /** Feature family attribution decomposition */
  attribution_flow_momentum: number;
  attribution_breadth_topology: number;
  attribution_creator_wallet_prior: number;
  attribution_multimodal_junk: number;
  attribution_manipulation_penalty: number;
  attribution_friction_route: number;
  attribution_regime_boundary: number;

  /** Route attribution */
  route_attribution: number;

  /** Wallet prior attribution */
  wallet_prior_attribution: number;

  /** Multimodal filter attribution */
  multimodal_filter_attribution: number;
}

/** Replay run metadata */
export interface ReplayRun {
  id: string;
  started_at: number;
  finished_at: number | null;
  config_version: number;
  event_count: number;
  trade_count: number;
  net_pnl_sol: number | null;
  metrics: ReplayMetrics | null;
  status: 'running' | 'completed' | 'failed';
  error: string | null;
}

/** Replay/paper metrics */
export interface ReplayMetrics {
  net_expectancy_per_trade: number;
  hit_rate: number;
  max_drawdown: number;
  fill_adjusted_ev_gap: number;
  precision_at_k: number;
  avg_hold_edge_decay: number;
  boundary_exit_performance: number;
  paper_live_discrepancy: number;
  total_trades: number;
  total_pnl_sol: number;
  forced_exits: number;
  avg_hold_time_s: number;
  missed_edge_regret_rate: number;
}
