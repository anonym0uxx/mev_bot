/**
 * @module alerts/system
 * Alert system per spec section 17.
 * Three delivery modes: immediate_alert, scheduled_summary, log_only.
 *
 * Bot trades autonomously — chat is for exceptions, summaries, and operator controls only.
 * Bot must NEVER wait for operator approval to enter/reduce/exit/auto-pause.
 */

import { v4 as uuidv4 } from 'uuid';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { AlertEvent } from '../types/events';
import { PumpQuantConfig } from '../types/config';
import { Position } from '../types/trade';

const log = createLogger('alerts');

export type AlertDeliveryMode = 'immediate_alert' | 'scheduled_summary' | 'log_only';

export interface AlertCallback {
  (alert: AlertEvent): void;
}

export class AlertSystem {
  private config: PumpQuantConfig;
  private pendingSummaryAlerts: AlertEvent[] = [];
  private onImmediateAlert: AlertCallback | null = null;
  private lastMidSessionSummary: number = 0;
  private lastEndOfDaySummary: number = 0;

  constructor(config: PumpQuantConfig) {
    this.config = config;
  }

  /** Update config */
  updateConfig(config: PumpQuantConfig): void {
    this.config = config;
  }

  /** Register callback for immediate alerts */
  onImmediate(callback: AlertCallback): void {
    this.onImmediateAlert = callback;
  }

  /**
   * Emit an alert with automatic routing to the correct delivery mode.
   */
  emit(
    type: string,
    message: string,
    data: Record<string, unknown> = {},
    severityOverride?: AlertDeliveryMode
  ): AlertEvent {
    const severity = severityOverride || this.classifyAlert(type);

    const alert: AlertEvent = {
      id: uuidv4(),
      type,
      severity,
      message,
      data,
      timestamp: nowMs(),
      delivered: false,
    };

    switch (severity) {
      case 'immediate_alert':
        this.deliverImmediate(alert);
        break;
      case 'scheduled_summary':
        this.pendingSummaryAlerts.push(alert);
        break;
      case 'log_only':
        log.debug(`[alert:log] ${type}: ${message}`);
        break;
    }

    return alert;
  }

  // ====== SPECIFIC ALERT EMITTERS ======

  /** Buy filled alert */
  emitBuyFilled(mint: string, symbol: string, sol: number, price: number): void {
    this.emit('buy_filled', `🟢 Bought ${symbol}: ${sol.toFixed(4)} SOL`, {
      mint, symbol, sol, price,
    });
  }

  /** Reduce filled alert */
  emitReduceFilled(mint: string, symbol: string, pct: number, sol: number): void {
    this.emit('reduce_filled', `🟡 Reduced ${symbol}: ${pct}% (${sol.toFixed(4)} SOL)`, {
      mint, symbol, pct, sol,
    });
  }

  /** Full exit alert */
  emitFullExit(mint: string, symbol: string, pnl: number, reason: string): void {
    const emoji = pnl >= 0 ? '✅' : '🔴';
    this.emit('full_exit', `${emoji} Exited ${symbol}: ${pnl >= 0 ? '+' : ''}${pnl.toFixed(4)} SOL (${reason})`, {
      mint, symbol, pnl, reason,
    });
  }

  /** Forced exit alert */
  emitForcedExit(mint: string, symbol: string, reason: string): void {
    this.emit('forced_exit', `🚨 FORCED EXIT ${symbol}: ${reason}`, {
      mint, symbol, reason,
    });
  }

  /** Auto-pause alert */
  emitAutoPause(reason: string): void {
    this.emit('auto_pause', `⏸️ Auto-paused: ${reason}`, { reason });
  }

  /** Stale feed alert */
  emitStaleFeed(subsystem: string, staleSinceS: number): void {
    this.emit('stale_feed', `⚠️ Stale ${subsystem}: ${staleSinceS.toFixed(0)}s`, {
      subsystem, staleSinceS,
    });
  }

  /** Execution failure alert */
  emitExecutionFailure(mint: string, error: string): void {
    this.emit('execution_failure', `❌ Execution failed: ${error}`, { mint, error });
  }

  /** Config integrity failure alert */
  emitConfigIntegrityFailure(error: string): void {
    this.emit('config_integrity_failure', `🔧 Config integrity: ${error}`, { error });
  }

  // ====== SUMMARIES ======

  /**
   * Generate mid-session summary if meaningful change occurred.
   */
  generateMidSessionSummary(
    positions: Position[],
    dailyPnl: number,
    tradesToday: number
  ): string | null {
    if (!this.config.alerts.scheduled_summary.mid_session_enabled) return null;
    if (tradesToday === 0 && positions.length === 0) return null;

    const openPositions = positions.filter(p => p.status === 'open' || p.status === 'reducing');
    const totalUnrealized = openPositions.reduce((sum, p) => sum + p.unrealized_pnl_sol, 0);

    const summary = [
      `📊 Mid-Session Summary`,
      `Trades today: ${tradesToday}`,
      `Daily PnL: ${dailyPnl >= 0 ? '+' : ''}${dailyPnl.toFixed(4)} SOL`,
      `Open positions: ${openPositions.length}`,
      openPositions.length > 0 ? `Unrealized: ${totalUnrealized >= 0 ? '+' : ''}${totalUnrealized.toFixed(4)} SOL` : '',
      ...openPositions.map(p =>
        `  • ${p.symbol}: ${p.unrealized_pnl_pct >= 0 ? '+' : ''}${(p.unrealized_pnl_pct * 100).toFixed(1)}%`
      ),
    ].filter(Boolean).join('\n');

    this.lastMidSessionSummary = nowMs();
    return summary;
  }

  /**
   * Generate end-of-day summary.
   */
  generateEndOfDaySummary(
    positions: Position[],
    dailyPnl: number,
    tradesToday: number,
    hitRate: number,
    forcedExits: number
  ): string {
    const closedToday = positions.filter(p => p.status === 'closed');
    const openPositions = positions.filter(p => p.status === 'open' || p.status === 'reducing');

    const summary = [
      `📊 End of Day Summary`,
      `━━━━━━━━━━━━━━━━━`,
      `Trades: ${tradesToday}`,
      `Win rate: ${(hitRate * 100).toFixed(0)}%`,
      `Daily PnL: ${dailyPnl >= 0 ? '+' : ''}${dailyPnl.toFixed(4)} SOL`,
      `Forced exits: ${forcedExits}`,
      `Open positions: ${openPositions.length}`,
      closedToday.length > 0 ? `\nClosed trades:` : '',
      ...closedToday.map(p =>
        `  • ${p.symbol}: ${(p.realized_pnl_sol ?? 0) >= 0 ? '+' : ''}${(p.realized_pnl_sol ?? 0).toFixed(4)} SOL`
      ),
    ].filter(Boolean).join('\n');

    this.lastEndOfDaySummary = nowMs();
    return summary;
  }

  // ====== HELPERS ======

  /** Classify alert type to delivery mode */
  private classifyAlert(type: string): AlertDeliveryMode {
    if (this.config.alerts.immediate.includes(type)) {
      return 'immediate_alert';
    }
    if (this.config.alerts.log_only.includes(type)) {
      return 'log_only';
    }
    return 'scheduled_summary';
  }

  /** Deliver an immediate alert */
  private deliverImmediate(alert: AlertEvent): void {
    log.info(`[alert:immediate] ${alert.type}: ${alert.message}`);
    alert.delivered = true;

    if (this.onImmediateAlert) {
      try {
        this.onImmediateAlert(alert);
      } catch (err) {
        log.error(`Alert callback error: ${(err as Error).message}`);
      }
    }
  }

  /** Get and clear pending summary alerts */
  flushPendingSummary(): AlertEvent[] {
    const alerts = [...this.pendingSummaryAlerts];
    this.pendingSummaryAlerts = [];
    return alerts;
  }
}
