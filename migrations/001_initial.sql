-- Migration 001: Initial schema for pump-quant
-- All tables for: raw_events, token_state, feature_snapshots, candidate_packets,
-- trade_intents, orders, positions, config_versions, replay_runs, health_events, learning_ledger

CREATE TABLE IF NOT EXISTS raw_events (
  id TEXT PRIMARY KEY,
  type TEXT NOT NULL,
  mint TEXT,
  data TEXT NOT NULL,
  timestamp INTEGER NOT NULL,
  received_at INTEGER NOT NULL,
  replay_run_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_raw_events_mint ON raw_events(mint);
CREATE INDEX IF NOT EXISTS idx_raw_events_timestamp ON raw_events(timestamp);
CREATE INDEX IF NOT EXISTS idx_raw_events_type ON raw_events(type);
CREATE INDEX IF NOT EXISTS idx_raw_events_replay ON raw_events(replay_run_id);

CREATE TABLE IF NOT EXISTS token_state (
  mint TEXT PRIMARY KEY,
  symbol TEXT NOT NULL,
  name TEXT NOT NULL,
  creator TEXT NOT NULL,
  bonding_curve_key TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'OBSERVE',
  regime TEXT NOT NULL DEFAULT 'EXCLUDED',
  v_tokens_in_curve REAL NOT NULL DEFAULT 0,
  v_sol_in_curve REAL NOT NULL DEFAULT 0,
  market_cap_sol REAL NOT NULL DEFAULT 0,
  bonding_curve_progress REAL NOT NULL DEFAULT 0,
  uri TEXT NOT NULL DEFAULT '',
  metadata_fetched INTEGER NOT NULL DEFAULT 0,
  first_seen INTEGER NOT NULL,
  last_updated INTEGER NOT NULL,
  state_entered_at INTEGER NOT NULL,
  ban_reason TEXT,
  config_version INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_token_state_state ON token_state(state);
CREATE INDEX IF NOT EXISTS idx_token_state_regime ON token_state(regime);

CREATE TABLE IF NOT EXISTS feature_snapshots (
  id TEXT PRIMARY KEY,
  mint TEXT NOT NULL,
  timestamp INTEGER NOT NULL,
  flow_momentum TEXT NOT NULL,
  breadth_topology TEXT NOT NULL,
  creator_wallet_prior TEXT NOT NULL,
  friction_execution TEXT NOT NULL,
  manipulation_distribution TEXT NOT NULL,
  multimodal_junk TEXT NOT NULL,
  FOREIGN KEY (mint) REFERENCES token_state(mint)
);
CREATE INDEX IF NOT EXISTS idx_feature_snapshots_mint ON feature_snapshots(mint);
CREATE INDEX IF NOT EXISTS idx_feature_snapshots_timestamp ON feature_snapshots(timestamp);

CREATE TABLE IF NOT EXISTS candidate_packets (
  id TEXT PRIMARY KEY,
  mint TEXT NOT NULL,
  state TEXT NOT NULL,
  regime TEXT NOT NULL,
  bonding_curve_progress REAL NOT NULL,
  market_cap_sol REAL NOT NULL,
  feature_snapshot_id TEXT,
  probabilities TEXT NOT NULL,
  entry_ev TEXT,
  exit_ev TEXT,
  sizing TEXT,
  config_version INTEGER NOT NULL,
  timestamp INTEGER NOT NULL,
  FOREIGN KEY (mint) REFERENCES token_state(mint),
  FOREIGN KEY (feature_snapshot_id) REFERENCES feature_snapshots(id)
);
CREATE INDEX IF NOT EXISTS idx_candidate_packets_mint ON candidate_packets(mint);
CREATE INDEX IF NOT EXISTS idx_candidate_packets_state ON candidate_packets(state);
CREATE INDEX IF NOT EXISTS idx_candidate_packets_timestamp ON candidate_packets(timestamp);

CREATE TABLE IF NOT EXISTS trade_intents (
  id TEXT PRIMARY KEY,
  mint TEXT NOT NULL,
  side TEXT NOT NULL,
  size_sol REAL NOT NULL,
  amount_pct REAL,
  slippage_bps INTEGER NOT NULL,
  priority_fee_sol REAL NOT NULL,
  route_mode TEXT NOT NULL,
  reason TEXT NOT NULL,
  config_version INTEGER NOT NULL,
  ev_at_intent REAL NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_trade_intents_mint ON trade_intents(mint);

CREATE TABLE IF NOT EXISTS orders (
  id TEXT PRIMARY KEY,
  trade_intent_id TEXT NOT NULL,
  mint TEXT NOT NULL,
  side TEXT NOT NULL,
  size_sol REAL NOT NULL,
  amount_pct REAL,
  slippage_bps INTEGER NOT NULL,
  priority_fee_sol REAL NOT NULL,
  route_mode TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  tx_signature TEXT,
  created_at INTEGER NOT NULL,
  sent_at INTEGER,
  confirmed_at INTEGER,
  realized_sol REAL,
  realized_tokens REAL,
  realized_price REAL,
  realized_slippage_pct REAL,
  fee_sol REAL,
  priority_fee_paid_sol REAL,
  error TEXT,
  retry_count INTEGER NOT NULL DEFAULT 0,
  config_version INTEGER NOT NULL,
  is_paper INTEGER NOT NULL DEFAULT 0,
  FOREIGN KEY (trade_intent_id) REFERENCES trade_intents(id)
);
CREATE INDEX IF NOT EXISTS idx_orders_mint ON orders(mint);
CREATE INDEX IF NOT EXISTS idx_orders_status ON orders(status);

CREATE TABLE IF NOT EXISTS positions (
  id TEXT PRIMARY KEY,
  mint TEXT NOT NULL,
  symbol TEXT NOT NULL,
  name TEXT NOT NULL,
  regime TEXT NOT NULL,
  entry_order_id TEXT NOT NULL,
  entry_price_sol REAL NOT NULL,
  entry_sol REAL NOT NULL,
  entry_tokens REAL NOT NULL,
  entry_timestamp INTEGER NOT NULL,
  entry_route_mode TEXT NOT NULL,
  entry_config_version INTEGER NOT NULL,
  current_tokens REAL NOT NULL,
  current_value_sol REAL NOT NULL DEFAULT 0,
  unrealized_pnl_sol REAL NOT NULL DEFAULT 0,
  unrealized_pnl_pct REAL NOT NULL DEFAULT 0,
  peak_net_exit_value REAL NOT NULL DEFAULT 0,
  exit_orders TEXT NOT NULL DEFAULT '[]',
  exit_price_sol REAL,
  exit_sol REAL,
  exit_timestamp INTEGER,
  exit_reason TEXT,
  exit_route_mode TEXT,
  realized_pnl_sol REAL,
  realized_pnl_pct REAL,
  total_fees_sol REAL NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'open',
  opened_at INTEGER NOT NULL,
  closed_at INTEGER,
  hold_duration_s REAL NOT NULL DEFAULT 0,
  mfe_sol REAL NOT NULL DEFAULT 0,
  mae_sol REAL NOT NULL DEFAULT 0,
  is_paper INTEGER NOT NULL DEFAULT 0,
  config_version INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_positions_mint ON positions(mint);
CREATE INDEX IF NOT EXISTS idx_positions_status ON positions(status);

CREATE TABLE IF NOT EXISTS config_versions (
  version INTEGER PRIMARY KEY,
  config TEXT NOT NULL,
  timestamp INTEGER NOT NULL,
  source TEXT NOT NULL,
  description TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS replay_runs (
  id TEXT PRIMARY KEY,
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  config_version INTEGER NOT NULL,
  event_count INTEGER NOT NULL DEFAULT 0,
  trade_count INTEGER NOT NULL DEFAULT 0,
  net_pnl_sol REAL,
  metrics TEXT,
  status TEXT NOT NULL DEFAULT 'running',
  error TEXT
);

CREATE TABLE IF NOT EXISTS health_events (
  id TEXT PRIMARY KEY,
  subsystem TEXT NOT NULL,
  status TEXT NOT NULL,
  message TEXT NOT NULL,
  timestamp INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_health_events_subsystem ON health_events(subsystem);
CREATE INDEX IF NOT EXISTS idx_health_events_timestamp ON health_events(timestamp);

CREATE TABLE IF NOT EXISTS learning_ledger (
  id TEXT PRIMARY KEY,
  timestamp INTEGER NOT NULL,
  event_type TEXT NOT NULL,
  mint TEXT NOT NULL,
  regime TEXT NOT NULL,
  config_version INTEGER NOT NULL,
  route_mode TEXT NOT NULL,
  feature_snapshot TEXT NOT NULL,
  candidate_packet_id TEXT NOT NULL,
  realized_fill_quality REAL,
  realized_pnl_sol REAL,
  mfe_sol REAL,
  mae_sol REAL,
  fast_lane_decision TEXT NOT NULL,
  deep_lane_decision TEXT,
  lane_agreement INTEGER NOT NULL DEFAULT 1,
  exit_timing_quality REAL,
  reject_regret REAL,
  attribution_flow_momentum REAL NOT NULL DEFAULT 0,
  attribution_breadth_topology REAL NOT NULL DEFAULT 0,
  attribution_creator_wallet_prior REAL NOT NULL DEFAULT 0,
  attribution_multimodal_junk REAL NOT NULL DEFAULT 0,
  attribution_manipulation_penalty REAL NOT NULL DEFAULT 0,
  attribution_friction_route REAL NOT NULL DEFAULT 0,
  attribution_regime_boundary REAL NOT NULL DEFAULT 0,
  route_attribution REAL NOT NULL DEFAULT 0,
  wallet_prior_attribution REAL NOT NULL DEFAULT 0,
  multimodal_filter_attribution REAL NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_learning_ledger_mint ON learning_ledger(mint);
CREATE INDEX IF NOT EXISTS idx_learning_ledger_timestamp ON learning_ledger(timestamp);
CREATE INDEX IF NOT EXISTS idx_learning_ledger_event_type ON learning_ledger(event_type);

CREATE TABLE IF NOT EXISTS state_transitions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  mint TEXT NOT NULL,
  from_state TEXT NOT NULL,
  to_state TEXT NOT NULL,
  reason TEXT NOT NULL,
  timestamp INTEGER NOT NULL,
  config_version INTEGER NOT NULL,
  feature_snapshot_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_state_transitions_mint ON state_transitions(mint);

-- Schema version tracking
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at INTEGER NOT NULL,
  filename TEXT NOT NULL
);
