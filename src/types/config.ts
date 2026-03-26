/**
 * @module types/config
 * Configuration type definitions matching config/schema.json
 */

export interface RegimeConfig {
  early_curve_max_progress: number;
  mid_curve_max_progress: number;
  late_curve_max_progress: number;
  graduation_boundary_start: number;
  graduation_boundary_end: number;
  max_token_age_s: number;
  exclude_mayhem: boolean;
  exclude_tokenized_agent: boolean;
}

export interface ManipulationPenaltyWeights {
  creator_sell: number;
  same_size_prints: number;
  price_breadth_divergence: number;
  concentration_worsening: number;
  cluster_correlation: number;
  suspicious_burst: number;
  slippage_shock: number;
}

export interface ManipulationConfig {
  hard_threshold: number;
  creator_sell_instant_exit: boolean;
  same_size_print_min_count: number;
  same_size_print_window_s: number;
  same_size_tolerance_pct: number;
  price_breadth_divergence_threshold: number;
  concentration_worsening_threshold: number;
  cluster_correlation_threshold: number;
  suspicious_burst_threshold: number;
  slippage_shock_threshold: number;
  continuous_penalty_weights: ManipulationPenaltyWeights;
}

export interface FrictionConfig {
  stale_threshold_s: number;
  safety_buffer_pct: number;
  slippage_estimation_method: 'empirical' | 'model' | 'hybrid';
  default_entry_slippage_pct: number;
  default_exit_slippage_pct: number;
  landing_degradation_local_pct: number;
  landing_degradation_lightning_pct: number;
}

export interface ProbabilityWeights {
  bonding_curve_dynamics: number;
  flow_momentum: number;
  breadth_topology: number;
  creator_wallet_prior: number;
  friction_execution: number;
  manipulation_distribution: number;
  multimodal_junk: number;
}

export interface CalibrationConfig {
  continuation_bias: number;
  reversal_bias: number;
  manipulation_bias: number;
}

export interface EntryConfig {
  min_entry_edge: number;
  observation_window_s: number;
  min_breadth_for_entry: number;
  min_unique_buyers: number;
  max_concentration_top10: number;
  max_slippage_pct: number;
  ev_wait_horizon_s: number;
  ev_enter_horizon_s: number;
  /** Minimum P_continuation required to enter. Tokens with P_cont below this are rejected
   *  regardless of EV — guards against entering on weak/noisy signals. */
  min_p_continuation: number;
  probability_weights: ProbabilityWeights;
  calibration: CalibrationConfig;
  /** Minimum number of raw trades before analysis is allowed (cold-start bootstrap guard) */
  min_trades_for_analysis?: number;
}

export interface ExitConfig {
  hold_horizon_s: number;
  retrace_threshold_base: number;
  retrace_tightening_boundary: number;
  retrace_tightening_slippage: number;
  retrace_tightening_hold_edge: number;
  retrace_tightening_time: number;
  time_decay_start_s: number;
  time_decay_pressure_per_s: number;
  max_hold_time_s: number;
  take_profit_pct?: number;            // Hard take-profit % (e.g. 0.5 = exit at +50%)
  trailing_stop_activation_pct?: number; // Activate trailing stop after this gain % (e.g. 0.025 = +2.5%)
  trailing_stop_distance_pct?: number;   // Trail distance from peak (e.g. 0.012 = 1.2%)
  tier1_profit_pct?: number;             // Partial reduce trigger % (e.g. 0.04 = +4%)
  tier1_reduce_pct?: number;             // How much to reduce at tier1 (e.g. 50 = sell 50%)
}

export interface RiskConfig {
  bankroll_sol: number;
  risk_per_trade_pct: number;
  max_alloc_pct: number;
  max_positions: number;
  quick_spend_sol: number;
  max_position_size_sol: number;
  max_daily_entries: number;
  raw_stop_pct: number;
  take_profit_pct: number;
  liquidity_cap_sol: number;
  slippage_cap_sol: number;
  max_daily_loss_sol: number;
  /** L3 circuit breaker: session halt if net PnL drops below this SOL value (negative) */
  circuit_breaker_session_halt_sol?: number;
}

export interface RoutePromotionConfig {
  enabled: boolean;
  opportunity_half_life_threshold_s: number;
  min_edge_for_lightning: number;
  min_edge_for_jito: number;
  demotion_cooldown_s: number;
}

export interface RouteHealthConfig {
  landing_latency_warn_ms: number;
  landing_latency_fail_ms: number;
  retry_rate_warn: number;
  retry_rate_fail: number;
  congestion_threshold: number;
  freshness_max_s: number;
}

export interface ExecutionConfig {
  default_route_mode: RouteMode;
  default_slippage_bps: number;
  default_priority_fee_sol: number;
  /** Dynamic priority fee: query getRecentPrioritizationFees and use p75 + 20% buffer */
  dynamic_priority_fee?: boolean;
  /** Percentile to use for dynamic fee (0–100, default 75) */
  dynamic_priority_fee_percentile?: number;
  /** Floor for dynamic fee in SOL */
  priority_fee_floor_sol?: number;
  /** Cap for dynamic fee in SOL */
  priority_fee_cap_sol?: number;
  skip_preflight: boolean;
  confirmation_timeout_ms: number;
  max_retries: number;
  route_promotion: RoutePromotionConfig;
  route_health: RouteHealthConfig;
  private_route: PrivateRouteConfig;
  bundle_route: BundleRouteConfig;
  /**
   * Explicit Jito on/off toggle.
   * When true + bundle_route.enabled, Jito bundle submission is attempted for
   * trades with route_mode === 'jito'. Failures always fall back to PumpPortal.
   * Default: false (opt-in).
   */
  jito_enabled?: boolean;
  /**
   * Tip to pay per Jito bundle in lamports.
   * Range: 1_000–100_000 (0.000001–0.0001 SOL). Default: 10_000.
   * Overrides private_route.jito_tip_lamports when present.
   */
  jito_tip_lamports?: number;
}

export interface RegimeFeeOverride {
  pump_fee_pct?: number;
  pump_swap_fee_pct?: number;
}

export interface FeesConfig {
  pump_fee_pct: number;
  pump_swap_fee_pct: number;
  pump_portal_fee_pct: number;
  solana_base_fee_sol: number;
  priority_fee_default_sol: number;
  regime_fee_overrides: Record<string, RegimeFeeOverride>;
}

export interface LLMConfig {
  provider: 'anthropic';
  default_model: string; // anthropic/claude-sonnet-4-6
  escalation_model: string; // anthropic/claude-opus-4-6
  /** Task classes that use escalation_model instead of default_model */
  escalation_task_classes: string[];
  supervisory_thinking_budget: {
    candidate_adjudication: 'low' | 'medium' | 'high';
    daily_analysis: 'low' | 'medium' | 'high';
    operator_summary: 'low' | 'medium' | 'high';
    weekly_review: 'low' | 'medium' | 'high';
    complex_attribution: 'low' | 'medium' | 'high';
    policy_improvement: 'low' | 'medium' | 'high';
  };
  supervisory_timeout_ms: number;
}

/** CoreCast gRPC feed configuration (primary fast-lane) */
export interface CoreCastConfig {
  enabled: boolean;
  endpoint: string; // gRPC endpoint
  api_key_env: string; // env var name for API key
  /** Subscriptions for Solana/Pump.fun */
  subscribe_new_tokens: boolean;
  subscribe_trades: boolean;
  subscribe_migrations: boolean;
  /** Reconnect policy */
  reconnect_base_ms: number;
  reconnect_max_ms: number;
  /** Staleness threshold for fast-lane health */
  stale_threshold_ms: number;
  /** When true, PumpPortal becomes fallback-only for market data */
  primary_for_market_data: boolean;
}

/** MEV-aware route class */
export type RouteClass = 'LOCAL' | 'LIGHTNING' | 'PRIVATE' | 'BUNDLE';

/** Jito/private route configuration */
export interface PrivateRouteConfig {
  enabled: boolean;
  jito_block_engine_url: string;
  jito_tip_lamports: number;
  /** Minimum expected edge improvement (SOL) to justify private submission */
  min_edge_for_private: number;
  /** Maximum tip as fraction of trade size */
  max_tip_pct: number;
  /** Use private for exits when slippage > threshold */
  exit_slippage_trigger_pct: number;
}

/** Bundle route configuration */
export interface BundleRouteConfig {
  enabled: boolean;
  /** Only for true multi-tx atomic requirements */
  max_bundle_size: number;
  min_edge_for_bundle: number;
}

export interface QualifiedWalletPriorConfig {
  enabled: boolean;
  max_positive_boost: number;
  max_negative_penalty: number;
  min_core_ev_for_boost: number;
  creator_history_weight: number;
  qualified_wallet_weight: number;
  top_trader_weight: number;
  first100_persistence_weight: number;
  dispersion_quality_weight: number;
  distribution_penalty_weight: number;
}

export interface MultimodalJunkFilterConfig {
  enabled: boolean;
  async_timeout_ms: number;
  exclusion_threshold: number;
  tiebreak_weight: number;
  ticker_clarity_weight: number;
  name_clarity_weight: number;
  logo_presence_weight: number;
  logo_quality_weight: number;
  metadata_spam_weight: number;
  comment_entropy_weight: number;
}

export interface FeaturesConfig {
  windows_s: number[];
  qualified_wallet_prior: QualifiedWalletPriorConfig;
  multimodal_junk_filter: MultimodalJunkFilterConfig;
}

export interface HourlyMicroCalibrationConfig {
  enabled: boolean;
  targets: string[];
}

export interface DailyReplayConfig {
  enabled: boolean;
  session_cut_hour_utc: number;
}

export interface DailyCanaryPromotionConfig {
  enabled: boolean;
  min_sample_size: number;
  min_net_expectancy: number;
  max_drawdown: number;
  min_precision_at_k: number;
}

export interface WeeklyRetrainConfig {
  enabled: boolean;
  day_of_week: number;
  hour_utc: number;
}

export interface ChampionChallengerConfig {
  max_challengers: number;
  canary_pct: number;
  promotion_min_trades: number;
  rollback_drawdown_threshold: number;
}

export interface LearningConfig {
  enabled: boolean;
  hourly_micro_calibration: HourlyMicroCalibrationConfig;
  daily_replay: DailyReplayConfig;
  daily_canary_promotion: DailyCanaryPromotionConfig;
  weekly_retrain: WeeklyRetrainConfig;
  champion_challenger: ChampionChallengerConfig;
}

export interface HealthConfig {
  check_interval_s: number;
  market_feed_stale_s: number;
  probability_stale_s: number;
  execution_stale_s: number;
  auto_pause_on_degraded: boolean;
}

export interface ScheduledSummaryConfig {
  mid_session_enabled: boolean;
  end_of_day_enabled: boolean;
  mid_session_hour_utc: number;
  end_of_day_hour_utc: number;
}

export interface AlertsConfig {
  immediate: string[];
  scheduled_summary: ScheduledSummaryConfig;
  log_only: string[];
}

export interface PumpQuantConfig {
  regime: RegimeConfig;
  manipulation: ManipulationConfig;
  friction: FrictionConfig;
  entry: EntryConfig;
  exit: ExitConfig;
  risk: RiskConfig;
  execution: ExecutionConfig;
  fees: FeesConfig;
  llm: LLMConfig;
  corecast: CoreCastConfig;
  features: FeaturesConfig;
  learning: LearningConfig;
  health: HealthConfig;
  alerts: AlertsConfig;
}

export type RouteMode = 'local' | 'lightning' | 'private' | 'jito';

/** Versioned config snapshot for audit trail */
export interface ConfigVersion {
  version: number;
  config: PumpQuantConfig;
  timestamp: number;
  source: 'file' | 'operator' | 'learning' | 'challenger';
  description: string;
}
