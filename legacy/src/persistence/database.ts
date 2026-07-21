/**
 * @module persistence/database
 * SQLite database manager with typed CRUD operations for all tables.
 */

import path from 'path';
import fs from 'fs';
import Database from 'better-sqlite3';
import { v4 as uuidv4 } from 'uuid';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { runMigrations } from './migrations';
import {
  RawEvent, HealthEvent, HealthSubsystem, HealthStatus,
} from '../types/events';
import {
  TokenState, Regime, CandidatePacket,
  ProbabilityOutputs, EntryEVCalculation, ExitEVCalculation, PositionSizing,
} from '../types/state';
import { FeatureSnapshot } from '../types/features';
import {
  TradeIntent, Order, OrderStatus, Position, PositionStatus,
  LearningLedgerRecord, ReplayRun,
} from '../types/trade';
import { ConfigVersion, PumpQuantConfig } from '../types/config';

const log = createLogger('database');

const PROJECT_ROOT = path.resolve(__dirname, '..', '..');
const DEFAULT_DB_PATH = path.join(PROJECT_ROOT, 'data', 'pump-quant.db');

export class PumpQuantDB {
  private db: Database.Database;

  constructor(dbPath?: string) {
    const resolvedPath = dbPath || process.env.DB_PATH || DEFAULT_DB_PATH;
    const dir = path.dirname(resolvedPath);
    if (!fs.existsSync(dir)) {
      fs.mkdirSync(dir, { recursive: true });
    }

    this.db = new Database(resolvedPath);
    this.db.pragma('journal_mode = WAL');
    this.db.pragma('foreign_keys = ON');
    this.db.pragma('busy_timeout = 5000');

    log.info(`Database opened: ${resolvedPath}`);
  }

  /** Run all pending migrations */
  migrate(): void {
    runMigrations(this.db);
  }

  /** Get underlying database instance */
  getDb(): Database.Database {
    return this.db;
  }

  /** Close the database */
  /** Direct access to the underlying better-sqlite3 instance for ad-hoc queries (use sparingly). */
  raw(): Database.Database {
    return this.db;
  }

  close(): void {
    this.db.close();
    log.info('Database closed');
  }

  // ====== RAW EVENTS ======

  insertRawEvent(event: RawEvent): void {
    const id = event.id || uuidv4();
    this.db.prepare(`
      INSERT INTO raw_events (id, type, mint, data, timestamp, received_at)
      VALUES (?, ?, ?, ?, ?, ?)
    `).run(id, event.type, (event.data as any).mint || null, JSON.stringify(event.data), event.timestamp, event.received_at);
  }

  getRawEvents(mint: string, limit: number = 1000, offset: number = 0): RawEvent[] {
    return this.db.prepare(`
      SELECT * FROM raw_events WHERE mint = ? ORDER BY timestamp DESC LIMIT ? OFFSET ?
    `).all(mint, limit, offset).map(this.mapRawEvent);
  }

  getRawEventsByTimeRange(startMs: number, endMs: number): RawEvent[] {
    return this.db.prepare(`
      SELECT * FROM raw_events WHERE timestamp >= ? AND timestamp <= ? ORDER BY timestamp ASC
    `).all(startMs, endMs).map(this.mapRawEvent);
  }

  private mapRawEvent(row: any): RawEvent {
    return {
      id: row.id,
      type: row.type,
      data: JSON.parse(row.data),
      timestamp: row.timestamp,
      received_at: row.received_at,
    };
  }

  // ====== TOKEN STATE ======

  upsertTokenState(packet: CandidatePacket): void {
    this.db.prepare(`
      INSERT INTO token_state (mint, symbol, name, creator, bonding_curve_key, state, regime,
        v_tokens_in_curve, v_sol_in_curve, market_cap_sol, bonding_curve_progress,
        uri, metadata_fetched, first_seen, last_updated, state_entered_at, ban_reason, config_version)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      ON CONFLICT(mint) DO UPDATE SET
        state = excluded.state,
        regime = excluded.regime,
        v_tokens_in_curve = excluded.v_tokens_in_curve,
        v_sol_in_curve = excluded.v_sol_in_curve,
        market_cap_sol = excluded.market_cap_sol,
        bonding_curve_progress = excluded.bonding_curve_progress,
        uri = excluded.uri,
        metadata_fetched = excluded.metadata_fetched,
        last_updated = excluded.last_updated,
        state_entered_at = excluded.state_entered_at,
        ban_reason = excluded.ban_reason,
        config_version = excluded.config_version
    `).run(
      packet.mint, packet.symbol, packet.name, packet.creator, packet.bonding_curve_key,
      packet.state, packet.regime,
      packet.v_tokens_in_curve, packet.v_sol_in_curve, packet.market_cap_sol,
      packet.bonding_curve_progress, packet.uri, packet.metadata_fetched ? 1 : 0,
      packet.first_seen, packet.last_updated, packet.state_entered_at,
      packet.ban_reason, packet.config_version
    );
  }

  getTokenState(mint: string): any | null {
    return this.db.prepare('SELECT * FROM token_state WHERE mint = ?').get(mint) || null;
  }

  getTokensByState(state: TokenState): any[] {
    return this.db.prepare('SELECT * FROM token_state WHERE state = ?').all(state);
  }

  getAllActiveTokens(): any[] {
    return this.db.prepare(
      "SELECT * FROM token_state WHERE state NOT IN ('EXIT', 'BAN')"
    ).all();
  }

  // ====== FEATURE SNAPSHOTS ======

  insertFeatureSnapshot(snapshot: FeatureSnapshot): string {
    const id = uuidv4();
    this.db.prepare(`
      INSERT INTO feature_snapshots (id, mint, timestamp, flow_momentum, breadth_topology,
        creator_wallet_prior, friction_execution, manipulation_distribution, multimodal_junk)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      id, snapshot.mint, snapshot.timestamp,
      JSON.stringify(snapshot.flow_momentum),
      JSON.stringify(snapshot.breadth_topology),
      JSON.stringify(snapshot.creator_wallet_prior),
      JSON.stringify(snapshot.friction_execution),
      JSON.stringify(snapshot.manipulation_distribution),
      JSON.stringify(snapshot.multimodal_junk)
    );
    return id;
  }

  getLatestFeatureSnapshot(mint: string): FeatureSnapshot | null {
    const row = this.db.prepare(
      'SELECT * FROM feature_snapshots WHERE mint = ? ORDER BY timestamp DESC LIMIT 1'
    ).get(mint) as any;
    if (!row) return null;
    return {
      timestamp: row.timestamp,
      mint: row.mint,
      flow_momentum: JSON.parse(row.flow_momentum),
      breadth_topology: JSON.parse(row.breadth_topology),
      creator_wallet_prior: JSON.parse(row.creator_wallet_prior),
      friction_execution: JSON.parse(row.friction_execution),
      manipulation_distribution: JSON.parse(row.manipulation_distribution),
      multimodal_junk: JSON.parse(row.multimodal_junk),
      // BCD fields added in signal stack v2; default to zero if not present in DB row
      bonding_curve_dynamics: row.bonding_curve_dynamics ? (() => {
        const bcd = JSON.parse(row.bonding_curve_dynamics);
        return {
          ...bcd,
          initial_capital_efficiency: bcd.initial_capital_efficiency ?? 0.5,
          accumulation_shape: bcd.accumulation_shape ?? 0.5,
          initial_burst_impact: bcd.initial_burst_impact ?? 0,
          high_impact_fraction: bcd.high_impact_fraction ?? 0,
          max_impact_ratio_normalized: bcd.max_impact_ratio_normalized ?? 0,
          organic_diversity_score: bcd.organic_diversity_score ?? 0.5,
        };
      })() : {
        capital_efficiency_raw: 0, capital_efficiency_normalized: 0,
        window_capital_efficiency: 0, efficiency_trend: 0.5,
        curve_fill_rate_sol_per_min: 0, curve_fill_rate_normalized: 0,
        large_trade_fraction: 0, median_trade_size_sol: 0,
        median_trade_size_normalized: 0,
        initial_capital_efficiency: 0.5,
        accumulation_shape: 0.5,
        initial_burst_impact: 0,
        high_impact_fraction: 0,
        max_impact_ratio_normalized: 0,
        organic_diversity_score: 0.5,
        bcd_score: 0,
      },
      creator_net_sol_position: row.creator_net_sol_position ?? 0,
      total_swap_count: row.total_swap_count ?? 0,
    };
  }

  // ====== CANDIDATE PACKETS ======

  insertCandidatePacket(packet: CandidatePacket): void {
    const featureSnapshotId = this.insertFeatureSnapshot(packet.features);
    this.db.prepare(`
      INSERT INTO candidate_packets (id, mint, state, regime, bonding_curve_progress,
        market_cap_sol, feature_snapshot_id, probabilities, entry_ev, exit_ev, sizing,
        config_version, timestamp)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      packet.id, packet.mint, packet.state, packet.regime, packet.bonding_curve_progress,
      packet.market_cap_sol, featureSnapshotId,
      JSON.stringify(packet.probabilities),
      packet.entry_ev ? JSON.stringify(packet.entry_ev) : null,
      packet.exit_ev ? JSON.stringify(packet.exit_ev) : null,
      packet.sizing ? JSON.stringify(packet.sizing) : null,
      packet.config_version, packet.last_updated
    );
  }

  // ====== TRADE INTENTS ======

  insertTradeIntent(intent: TradeIntent): void {
    this.db.prepare(`
      INSERT INTO trade_intents (id, mint, side, size_sol, amount_pct, slippage_bps,
        priority_fee_sol, route_mode, reason, config_version, ev_at_intent, created_at)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      intent.id, intent.mint, intent.side, intent.size_sol, intent.amount_pct ?? null,
      intent.slippage_bps, intent.priority_fee_sol, intent.route_mode,
      intent.reason, intent.config_version, intent.ev_at_intent, intent.created_at
    );
  }

  // ====== ORDERS ======

  insertOrder(order: Order): void {
    // INSERT OR IGNORE: if tx_signature already exists (unique index), silently skip the
    // duplicate insert. This prevents the confirmation poller from writing multiple rows
    // for the same on-chain transaction.
    this.db.prepare(`
      INSERT OR IGNORE INTO orders (id, trade_intent_id, mint, side, size_sol, amount_pct,
        slippage_bps, priority_fee_sol, route_mode, status, tx_signature,
        created_at, sent_at, confirmed_at, realized_sol, realized_tokens,
        realized_price, realized_slippage_pct, fee_sol, priority_fee_paid_sol,
        error, retry_count, config_version, is_paper)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      order.id, order.trade_intent_id, order.mint, order.side, order.size_sol,
      order.amount_pct ?? null, order.slippage_bps, order.priority_fee_sol,
      order.route_mode, order.status, order.tx_signature,
      order.created_at, order.sent_at, order.confirmed_at,
      order.realized_sol, order.realized_tokens, order.realized_price,
      order.realized_slippage_pct, order.fee_sol, order.priority_fee_paid_sol,
      order.error, order.retry_count, order.config_version, order.is_paper ? 1 : 0
    );
  }

  /**
   * Upsert an order by tx_signature — prevents duplicate rows when the confirmation
   * poller fires multiple times for the same on-chain transaction.
   * If no tx_signature (pending order), falls back to a plain insert.
   * Uses INSERT OR IGNORE on the internal id PK to be safe.
   */
  upsertOrderBySignature(order: Order): void {
    if (!order.tx_signature || order.tx_signature.trim() === '') {
      // No sig yet — just insert (pending/sent state)
      this.insertOrder(order);
      return;
    }

    // Check if an order with this tx_signature already exists
    const existing = this.db.prepare(
      'SELECT id FROM orders WHERE tx_signature = ? LIMIT 1'
    ).get(order.tx_signature) as { id: string } | undefined;

    if (existing) {
      // Update existing row — keep the highest realized_sol (final fill beats partial fill)
      const existingFull = this.getOrder(existing.id);
      const existingRealizedSol = existingFull?.realized_sol ?? 0;
      const newRealizedSol = order.realized_sol ?? 0;

      const updates: Partial<Order> = {
        status: order.status,
        confirmed_at: order.confirmed_at,
        fee_sol: order.fee_sol,
        priority_fee_paid_sol: order.priority_fee_paid_sol,
        error: order.error,
      };
      // Only upgrade realized_sol if the new value is higher (final fill > partial fill)
      if (newRealizedSol > existingRealizedSol) {
        updates.realized_sol = order.realized_sol;
        updates.realized_tokens = order.realized_tokens;
        updates.realized_price = order.realized_price;
        updates.realized_slippage_pct = order.realized_slippage_pct;
      }
      this.updateOrder(existing.id, updates);
      // Update the in-memory order id to point to the canonical row
      order.id = existing.id;
    } else {
      this.insertOrder(order);
    }
  }

  updateOrder(id: string, updates: Partial<Order>): void {
    const fields: string[] = [];
    const values: unknown[] = [];

    for (const [key, val] of Object.entries(updates)) {
      if (key === 'id') continue;
      fields.push(`${key} = ?`);
      values.push(key === 'is_paper' ? (val ? 1 : 0) : val);
    }

    if (fields.length === 0) return;
    values.push(id);

    this.db.prepare(`UPDATE orders SET ${fields.join(', ')} WHERE id = ?`).run(...values);
  }

  getOrder(id: string): Order | null {
    const row = this.db.prepare('SELECT * FROM orders WHERE id = ?').get(id) as any;
    if (!row) return null;
    return { ...row, is_paper: !!row.is_paper };
  }

  getOrdersByMint(mint: string): Order[] {
    return (this.db.prepare('SELECT * FROM orders WHERE mint = ? ORDER BY created_at DESC').all(mint) as any[])
      .map(r => ({ ...r, is_paper: !!r.is_paper }));
  }

  // ====== POSITIONS ======

  insertPosition(pos: Position): void {
    // INSERT OR IGNORE: if a unique constraint fires (duplicate open position for same mint),
    // silently drop the duplicate rather than creating a second row.
    // RENO FIX: includes entry_features and feat_* columns for ML feature logging.
    this.db.prepare(`
      INSERT OR IGNORE INTO positions (id, mint, symbol, name, regime, entry_order_id,
        entry_price_sol, entry_sol, entry_tokens, entry_timestamp, entry_route_mode,
        entry_config_version, current_tokens, current_value_sol, unrealized_pnl_sol,
        unrealized_pnl_pct, peak_net_exit_value, exit_orders, exit_price_sol, exit_sol,
        exit_timestamp, exit_reason, exit_route_mode, realized_pnl_sol, realized_pnl_pct,
        total_fees_sol, status, opened_at, closed_at, hold_duration_s, mfe_sol, mae_sol,
        is_paper, config_version,
        entry_features, feat_p_cont, feat_bcd_score, feat_manip_score, feat_creator_prior,
        feat_velocity, feat_breadth_score, feat_unique_buyers, feat_mcap_sol,
        entry_ts, active_stop_pct, active_target_pct, active_max_hold_s)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
              ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      pos.id, pos.mint, pos.symbol, pos.name, pos.regime, pos.entry_order_id,
      pos.entry_price_sol, pos.entry_sol, pos.entry_tokens, pos.entry_timestamp,
      pos.entry_route_mode, pos.entry_config_version, pos.current_tokens,
      pos.current_value_sol, pos.unrealized_pnl_sol, pos.unrealized_pnl_pct,
      pos.peak_net_exit_value, JSON.stringify(pos.exit_orders),
      pos.exit_price_sol, pos.exit_sol, pos.exit_timestamp, pos.exit_reason,
      pos.exit_route_mode, pos.realized_pnl_sol, pos.realized_pnl_pct,
      pos.total_fees_sol, pos.status, pos.opened_at, pos.closed_at,
      pos.hold_duration_s, pos.mfe_sol, pos.mae_sol, pos.is_paper ? 1 : 0,
      pos.config_version,
      pos.entry_features ?? null, pos.feat_p_cont ?? null, pos.feat_bcd_score ?? null,
      pos.feat_manip_score ?? null, pos.feat_creator_prior ?? null,
      pos.feat_velocity ?? null, pos.feat_breadth_score ?? null,
      pos.feat_unique_buyers ?? null, pos.feat_mcap_sol ?? null,
      pos.entry_ts ?? null, pos.active_stop_pct ?? null,
      pos.active_target_pct ?? null, pos.active_max_hold_s ?? null
    );
  }

  updatePosition(id: string, updates: Partial<Position>): void {
    const fields: string[] = [];
    const values: unknown[] = [];

    for (const [key, val] of Object.entries(updates)) {
      if (key === 'id') continue;
      let dbVal: unknown = val;
      if (key === 'exit_orders' && Array.isArray(val)) dbVal = JSON.stringify(val);
      if (key === 'is_paper') dbVal = val ? 1 : 0;
      fields.push(`${key} = ?`);
      values.push(dbVal);
    }

    if (fields.length === 0) return;
    values.push(id);
    this.db.prepare(`UPDATE positions SET ${fields.join(', ')} WHERE id = ?`).run(...values);
  }

  getPosition(id: string): Position | null {
    const row = this.db.prepare('SELECT * FROM positions WHERE id = ?').get(id) as any;
    return row ? this.mapPosition(row) : null;
  }

  getOpenPositions(): Position[] {
    return (this.db.prepare("SELECT * FROM positions WHERE status IN ('open', 'reducing')").all() as any[])
      .map(this.mapPosition);
  }

  getPositionByMint(mint: string): Position | null {
    const row = this.db.prepare(
      "SELECT * FROM positions WHERE mint = ? AND status IN ('open', 'reducing') LIMIT 1"
    ).get(mint) as any;
    return row ? this.mapPosition(row) : null;
  }

  // Returns ALL open positions for a mint — used for exit/close to avoid leaving ghost positions
  getAllOpenPositionsByMint(mint: string): Position[] {
    return (this.db.prepare(
      "SELECT * FROM positions WHERE mint = ? AND status IN ('open', 'reducing') ORDER BY opened_at ASC"
    ).all(mint) as any[]).map(this.mapPosition);
  }

  getAllPositions(limit: number = 100): Position[] {
    return (this.db.prepare('SELECT * FROM positions ORDER BY opened_at DESC LIMIT ?').all(limit) as any[])
      .map(this.mapPosition);
  }

  private mapPosition(row: any): Position {
    return {
      ...row,
      exit_orders: JSON.parse(row.exit_orders || '[]'),
      is_paper: !!row.is_paper,
    };
  }

  // ====== CONFIG VERSIONS ======

  insertConfigVersion(cv: ConfigVersion): void {
    this.db.prepare(`
      INSERT OR REPLACE INTO config_versions (version, config, timestamp, source, description)
      VALUES (?, ?, ?, ?, ?)
    `).run(cv.version, JSON.stringify(cv.config), cv.timestamp, cv.source, cv.description);
  }

  getConfigVersion(version: number): ConfigVersion | null {
    const row = this.db.prepare('SELECT * FROM config_versions WHERE version = ?').get(version) as any;
    if (!row) return null;
    return { ...row, config: JSON.parse(row.config) };
  }

  getLatestConfigVersion(): ConfigVersion | null {
    const row = this.db.prepare('SELECT * FROM config_versions ORDER BY version DESC LIMIT 1').get() as any;
    if (!row) return null;
    return { ...row, config: JSON.parse(row.config) };
  }

  // ====== HEALTH EVENTS ======

  insertHealthEvent(event: HealthEvent): void {
    this.db.prepare(`
      INSERT INTO health_events (id, subsystem, status, message, timestamp)
      VALUES (?, ?, ?, ?, ?)
    `).run(event.id, event.subsystem, event.status, event.message, event.timestamp);
  }

  getLatestHealthBySubsystem(subsystem: HealthSubsystem): HealthEvent | null {
    const row = this.db.prepare(
      'SELECT * FROM health_events WHERE subsystem = ? ORDER BY timestamp DESC LIMIT 1'
    ).get(subsystem) as any;
    return row || null;
  }

  // ====== LEARNING LEDGER ======

  insertLearningRecord(record: LearningLedgerRecord): void {
    this.db.prepare(`
      INSERT INTO learning_ledger (id, timestamp, event_type, mint, regime, config_version,
        route_mode, feature_snapshot, candidate_packet_id, realized_fill_quality,
        realized_pnl_sol, mfe_sol, mae_sol, fast_lane_decision, deep_lane_decision,
        lane_agreement, exit_timing_quality, reject_regret,
        attribution_flow_momentum, attribution_breadth_topology,
        attribution_creator_wallet_prior, attribution_multimodal_junk,
        attribution_manipulation_penalty, attribution_friction_route,
        attribution_regime_boundary, route_attribution, wallet_prior_attribution,
        multimodal_filter_attribution)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      record.id, record.timestamp, record.event_type, record.mint, record.regime,
      record.config_version, record.route_mode, record.feature_snapshot,
      record.candidate_packet_id, record.realized_fill_quality,
      record.realized_pnl_sol, record.mfe_sol, record.mae_sol,
      record.fast_lane_decision, record.deep_lane_decision,
      record.lane_agreement ? 1 : 0, record.exit_timing_quality, record.reject_regret,
      record.attribution_flow_momentum, record.attribution_breadth_topology,
      record.attribution_creator_wallet_prior, record.attribution_multimodal_junk,
      record.attribution_manipulation_penalty, record.attribution_friction_route,
      record.attribution_regime_boundary, record.route_attribution,
      record.wallet_prior_attribution, record.multimodal_filter_attribution
    );
  }

  getLearningRecords(limit: number = 1000, offset: number = 0): LearningLedgerRecord[] {
    return (this.db.prepare(
      'SELECT * FROM learning_ledger ORDER BY timestamp DESC LIMIT ? OFFSET ?'
    ).all(limit, offset) as any[]).map(r => ({
      ...r,
      lane_agreement: !!r.lane_agreement,
    }));
  }

  // ====== REPLAY RUNS ======

  insertReplayRun(run: ReplayRun): void {
    this.db.prepare(`
      INSERT INTO replay_runs (id, started_at, finished_at, config_version, event_count,
        trade_count, net_pnl_sol, metrics, status, error)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      run.id, run.started_at, run.finished_at, run.config_version,
      run.event_count, run.trade_count, run.net_pnl_sol,
      run.metrics ? JSON.stringify(run.metrics) : null, run.status, run.error
    );
  }

  updateReplayRun(id: string, updates: Partial<ReplayRun>): void {
    const fields: string[] = [];
    const values: unknown[] = [];
    for (const [key, val] of Object.entries(updates)) {
      if (key === 'id') continue;
      let dbVal = val;
      if (key === 'metrics' && val) dbVal = JSON.stringify(val);
      fields.push(`${key} = ?`);
      values.push(dbVal);
    }
    if (fields.length === 0) return;
    values.push(id);
    this.db.prepare(`UPDATE replay_runs SET ${fields.join(', ')} WHERE id = ?`).run(...values);
  }

  // ====== STATE TRANSITIONS ======

  insertStateTransition(mint: string, fromState: string, toState: string,
    reason: string, configVersion: number, featureSnapshotId?: string): void {
    this.db.prepare(`
      INSERT INTO state_transitions (mint, from_state, to_state, reason, timestamp, config_version, feature_snapshot_id)
      VALUES (?, ?, ?, ?, ?, ?, ?)
    `).run(mint, fromState, toState, reason, nowMs(), configVersion, featureSnapshotId || null);
  }

  // ====== AGGREGATE QUERIES ======

  /** Count new entries (buys) executed today — used to enforce max_daily_entries. */
  getDailyEntryCount(): number {
    const startOfDay = new Date();
    startOfDay.setHours(0, 0, 0, 0);
    const row = this.db.prepare(`
      SELECT COUNT(*) as cnt
      FROM positions
      WHERE opened_at >= ?
        AND is_paper = 0
    `).get(startOfDay.getTime()) as any;
    return row?.cnt ?? 0;
  }

  /**
   * Get realized PnL since a given timestamp (default: start of calendar day).
   *
   * @param sinceMs - Optional epoch ms. Pass configChangeEpoch to get PnL only
   *   from trades after a mid-session config change, so the new daily limit applies
   *   fresh to the new config's trades rather than the full day's accumulated loss.
   *
   * NOTE: L3 circuit breaker uses sinceMs=undefined (full day) intentionally — a
   * -0.30 SOL session loss warrants halting regardless of config changes mid-day.
   */
  getDailyPnl(sinceMs?: number): number {
    const startOfDay = new Date();
    startOfDay.setHours(0, 0, 0, 0);
    const epochMs = sinceMs !== undefined ? Math.max(sinceMs, startOfDay.getTime()) : startOfDay.getTime();
    const row = this.db.prepare(`
      SELECT COALESCE(SUM(realized_pnl_sol), 0) as total
      FROM positions
      WHERE closed_at >= ?
        AND status = 'closed'
        AND entry_sol > 0
        AND realized_pnl_sol IS NOT NULL
    `).get(epochMs) as any;
    return row?.total ?? 0;
  }

  getOpenPositionCount(): number {
    const row = this.db.prepare(
      "SELECT COUNT(*) as cnt FROM positions WHERE status IN ('open', 'reducing')"
    ).get() as any;
    return row?.cnt ?? 0;
  }
}

/** Global database singleton */
let _db: PumpQuantDB | null = null;

export function getDatabase(dbPath?: string): PumpQuantDB {
  if (!_db) {
    _db = new PumpQuantDB(dbPath);
    _db.migrate();
  }
  return _db;
}
