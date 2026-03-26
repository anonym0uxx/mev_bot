/**
 * @module types/features
 * Feature family type definitions
 */

/** Flow/momentum features (section 7.1) */
export interface FlowMomentumFeatures {
  buy_notional_velocity_1s: number;
  buy_notional_velocity_5s: number;
  buy_notional_velocity_15s: number;
  buy_notional_velocity_30s: number;
  trade_count_velocity_1s: number;
  trade_count_velocity_5s: number;
  trade_count_velocity_15s: number;
  trade_count_velocity_30s: number;
  buy_velocity_acceleration_5s: number;
  buy_velocity_acceleration_15s: number;
  curve_progress_acceleration_5s: number;
  curve_progress_acceleration_15s: number;
  buy_sell_imbalance_5s: number;
  buy_sell_imbalance_15s: number;
  buy_sell_imbalance_30s: number;
  avg_trade_size_5s: number;
  avg_trade_size_15s: number;
  size_dispersion_5s: number;
  size_dispersion_15s: number;
}

/** Breadth/topology features (section 7.2) */
export interface BreadthTopologyFeatures {
  unique_buyers_growth_5s: number;
  unique_buyers_growth_15s: number;
  unique_buyers_total: number;
  repeat_wallet_ratio: number;
  fresh_wallet_ratio: number;
  non_dev_participation: number;
  first_100_persistence: number;
  top_10_concentration: number;
  top_20_concentration: number;
  breadth_score: number;
}

/** Creator/qualified wallet prior features (section 7.3) */
export interface CreatorWalletPriorFeatures {
  creator_history_score: number;
  creator_sell_flag: boolean;
  creator_holdings_trend: number;
  qualified_wallet_score: number;
  top_trader_score: number;
  first_100_persistence_contribution: number;
  dispersion_quality_score: number;
  distribution_penalty: number;
  /** Composite prior: capped boost or penalty */
  composite_prior: number;
}

/** Friction/execution features (section 7.4) */
export interface FrictionExecutionFeatures {
  expected_entry_slippage: number;
  expected_exit_slippage: number;
  route_mode: string;
  priority_fee_burden: number;
  landing_risk_estimate: number;
  retry_failure_rate: number;
  execution_freshness_s: number;
  route_score: number;
  route_ev_adjustment: number;
  route_health_prior: number;
  latency_budget_utilization: number;
}

/** Manipulation/distribution features (section 7.5) */
export interface ManipulationDistributionFeatures {
  creator_sell: boolean;
  same_size_print_count: number;
  price_breadth_divergence: number;
  concentration_worsening: number;
  cluster_correlation: number;
  suspicious_burst: number;
  slippage_shock: number;
  distribution_signatures: number;
  /** Continuous manipulation penalty [0,1] */
  manipulation_penalty: number;
  /** Hard shock detected */
  hard_shock: boolean;
}

/** Secondary multimodal junk filter features (section 7.6) */
export interface MultimodalJunkFeatures {
  ticker_clarity: number;
  name_clarity: number;
  logo_presence: number;
  logo_quality: number;
  metadata_spam: number;
  comment_entropy: number;
  social_pickup: number;
  /** Composite junk score [0=junk, 1=clean] */
  junk_score: number;
  /** Whether the filter result is stale/unavailable */
  is_stale: boolean;
}

/** Bonding curve dynamics features — primary graduation predictor (arXiv:2602.14860) */
export interface BondingCurveDynamicsFeatures {
  // Primary signal: vSol accumulated per swap (higher = fewer decisive trades = better)
  capital_efficiency_raw: number;        // vSolInBondingCurve / totalSwapCount
  capital_efficiency_normalized: number; // clamp(raw / CE_SCALE, 0, 1) → [0,1] higher=better

  // Window efficiency: recent accumulation quality
  window_capital_efficiency: number;     // windowVSolAccumulated / windowSwapCount

  // Trend: is efficiency improving (more SOL/trade over time) or degrading?
  efficiency_trend: number;              // [0,1] where 1=strongly improving

  // Fill rate: SOL per minute of curve fill (absolute velocity)
  curve_fill_rate_sol_per_min: number;   // raw
  curve_fill_rate_normalized: number;    // clamp(raw / 10.0, 0, 1)

  // Large trade presence: fraction of trades >= 0.10 SOL
  large_trade_fraction: number;          // largeTradeCount / totalSwapCount, [0,1]

  // Median trade size (approximated as mean for simplicity)
  median_trade_size_sol: number;         // approximated from recent trades
  median_trade_size_normalized: number;  // clamp(value / 0.05, 0, 1)

  // Composite bonding curve dynamics score [0,1]
  bcd_score: number;
}

/** Complete feature snapshot across all families */
export interface FeatureSnapshot {
  timestamp: number;
  mint: string;
  flow_momentum: FlowMomentumFeatures;
  breadth_topology: BreadthTopologyFeatures;
  creator_wallet_prior: CreatorWalletPriorFeatures;
  friction_execution: FrictionExecutionFeatures;
  manipulation_distribution: ManipulationDistributionFeatures;
  multimodal_junk: MultimodalJunkFeatures;
  bonding_curve_dynamics: BondingCurveDynamicsFeatures;
  /** Creator net SOL position: positive = dumping, negative = still holding (for veto) */
  creator_net_sol_position: number;
  /** Total swap count since creation (for veto rules) */
  total_swap_count: number;
}

/** Rolling window data point for feature computation */
export interface TradeDataPoint {
  timestamp: number;
  txType: 'buy' | 'sell';
  solAmount: number;
  tokenAmount: number;
  traderPublicKey: string;
  vTokensInBondingCurve: number;
  vSolInBondingCurve: number;
  marketCapSol: number;
  newTokenBalance: number;
}

/** Windowed trade buffer for rolling computations */
export interface WindowedTradeBuffer {
  trades: TradeDataPoint[];
  windowSizeS: number;
  /** Remove trades older than window */
  prune(now: number): void;
}
