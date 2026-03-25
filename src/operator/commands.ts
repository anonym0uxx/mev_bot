/**
 * @module operator/commands
 * Operator command parsing for WhatsApp/chat control.
 * Supports all commands from spec section 17:
 * status, health, positions, top, inspect, pause, resume, pnl,
 * risk, set quick_spend, set risk_per_trade, set max_alloc, set slippage_cap,
 * profile, set profile
 */

import { createLogger } from '../utils/logger';
import { PumpQuantConfig } from '../types/config';
import { Position } from '../types/trade';
import { CandidatePacket } from '../types/state';
import { SystemHealth } from '../health/monitor';

const log = createLogger('operator');

export interface CommandResult {
  success: boolean;
  message: string;
  data?: Record<string, unknown>;
}

export type CommandHandler = (args: string[]) => Promise<CommandResult>;

/**
 * Parse an operator command string into command name and arguments.
 */
export function parseCommand(input: string): { command: string; args: string[] } | null {
  const trimmed = input.trim().toLowerCase();
  if (!trimmed) return null;

  const parts = trimmed.split(/\s+/);
  const command = parts[0];
  const args = parts.slice(1);

  // Handle "set" commands
  if (command === 'set' && args.length >= 1) {
    return { command: `set_${args[0]}`, args: args.slice(1) };
  }

  return { command, args };
}

/**
 * Format status response for operator.
 */
export function formatStatus(
  health: SystemHealth,
  positions: Position[],
  dailyPnl: number,
  isPaused: boolean
): string {
  const openPositions = positions.filter(p => p.status === 'open' || p.status === 'reducing');
  const totalUnrealized = openPositions.reduce((sum, p) => sum + p.unrealized_pnl_sol, 0);

  return [
    `🤖 Pump Quant Status`,
    `━━━━━━━━━━━━━━━━━`,
    `Health: ${health.overall === 'healthy' ? '✅' : health.overall === 'degraded' ? '⚠️' : '❌'} ${health.overall}`,
    `Trading: ${isPaused ? '⏸️ PAUSED' : '▶️ Active'}${health.pauseReason ? ` (${health.pauseReason})` : ''}`,
    `Positions: ${openPositions.length}`,
    `Unrealized: ${totalUnrealized >= 0 ? '+' : ''}${totalUnrealized.toFixed(4)} SOL`,
    `Daily PnL: ${dailyPnl >= 0 ? '+' : ''}${dailyPnl.toFixed(4)} SOL`,
  ].join('\n');
}

/**
 * Format health details for operator.
 */
export function formatHealth(health: SystemHealth): string {
  const lines = [
    `🏥 System Health`,
    `━━━━━━━━━━━━━━━━━`,
    `Overall: ${health.overall}`,
    `Trading: ${health.tradingAllowed ? '✅ Allowed' : '❌ Blocked'}`,
    ``,
  ];

  for (const sub of health.subsystems) {
    const icon = sub.status === 'healthy' ? '✅' : sub.status === 'degraded' ? '⚠️' : '❌';
    lines.push(`${icon} ${sub.name}: ${sub.message}`);
  }

  return lines.join('\n');
}

/**
 * Format positions for operator.
 */
export function formatPositions(positions: Position[]): string {
  const open = positions.filter(p => p.status === 'open' || p.status === 'reducing');

  if (open.length === 0) {
    return '📊 No open positions';
  }

  const lines = [`📊 Open Positions (${open.length})`, `━━━━━━━━━━━━━━━━━`];

  for (const pos of open) {
    const pnlStr = pos.unrealized_pnl_sol >= 0
      ? `+${pos.unrealized_pnl_sol.toFixed(4)}`
      : pos.unrealized_pnl_sol.toFixed(4);
    const pctStr = `${(pos.unrealized_pnl_pct * 100).toFixed(1)}%`;

    lines.push([
      `${pos.symbol} (${pos.status})`,
      `  Entry: ${pos.entry_sol.toFixed(4)} SOL`,
      `  PnL: ${pnlStr} SOL (${pctStr})`,
      `  Hold: ${pos.hold_duration_s.toFixed(0)}s`,
    ].join('\n'));
  }

  return lines.join('\n');
}

/**
 * Format top candidates for operator.
 */
export function formatTopCandidates(candidates: CandidatePacket[]): string {
  if (candidates.length === 0) {
    return '🎯 No active candidates';
  }

  const lines = [`🎯 Top Candidates (${candidates.length})`, `━━━━━━━━━━━━━━━━━`];

  for (const c of candidates) {
    const edge = c.entry_ev?.EntryEdge?.toFixed(6) || 'N/A';
    const ev = c.entry_ev?.EV_enter_now?.toFixed(6) || 'N/A';
    lines.push([
      `${c.symbol} [${c.state}] ${c.regime}`,
      `  EV: ${ev} | Edge: ${edge}`,
      `  Breadth: ${c.features.breadth_topology.breadth_score.toFixed(3)}`,
      `  Manipulation: ${c.features.manipulation_distribution.manipulation_penalty.toFixed(3)}`,
    ].join('\n'));
  }

  return lines.join('\n');
}

/**
 * Format candidate inspection for operator.
 */
export function formatInspection(packet: CandidatePacket): string {
  const f = packet.features;
  return [
    `🔍 ${packet.symbol} (${packet.mint.substring(0, 8)}...)`,
    `━━━━━━━━━━━━━━━━━`,
    `State: ${packet.state} | Regime: ${packet.regime}`,
    `Curve: ${(packet.bonding_curve_progress * 100).toFixed(1)}% | MCap: ${packet.market_cap_sol.toFixed(2)} SOL`,
    ``,
    `Flow/Momentum:`,
    `  Buy vel (5s): ${f.flow_momentum.buy_notional_velocity_5s.toFixed(4)}`,
    `  Imbalance (5s): ${f.flow_momentum.buy_sell_imbalance_5s.toFixed(3)}`,
    `  Acceleration: ${f.flow_momentum.buy_velocity_acceleration_5s.toFixed(4)}`,
    ``,
    `Breadth:`,
    `  Unique buyers: ${f.breadth_topology.unique_buyers_total}`,
    `  Breadth score: ${f.breadth_topology.breadth_score.toFixed(3)}`,
    `  Top-10 conc: ${(f.breadth_topology.top_10_concentration * 100).toFixed(1)}%`,
    ``,
    `Manipulation: ${f.manipulation_distribution.manipulation_penalty.toFixed(3)} ${f.manipulation_distribution.hard_shock ? '🚨 SHOCK' : ''}`,
    `Creator sold: ${f.manipulation_distribution.creator_sell ? '⚠️ YES' : '✅ No'}`,
    ``,
    `Entry EV: ${packet.entry_ev?.EV_enter_now?.toFixed(6) || 'N/A'}`,
    `Entry Edge: ${packet.entry_ev?.EntryEdge?.toFixed(6) || 'N/A'}`,
    `Position Size: ${packet.sizing?.position_size?.toFixed(4) || 'N/A'} SOL`,
  ].join('\n');
}

/**
 * Format risk settings for operator.
 */
export function formatRiskSettings(config: PumpQuantConfig): string {
  const r = config.risk;
  return [
    `⚙️ Risk Settings`,
    `━━━━━━━━━━━━━━━━━`,
    `Bankroll: ${r.bankroll_sol} SOL`,
    `Quick spend: ${r.quick_spend_sol} SOL`,
    `Risk/trade: ${(r.risk_per_trade_pct * 100).toFixed(1)}%`,
    `Max alloc: ${(r.max_alloc_pct * 100).toFixed(1)}%`,
    `Max positions: ${r.max_positions}`,
    `Stop loss: ${(r.raw_stop_pct * 100).toFixed(1)}%`,
    `Slippage cap: ${r.slippage_cap_sol} SOL`,
    `Daily loss limit: ${r.max_daily_loss_sol} SOL`,
  ].join('\n');
}

/**
 * Format PnL summary for operator.
 */
export function formatPnl(
  dailyPnl: number,
  positions: Position[]
): string {
  const closed = positions.filter(p => p.status === 'closed');
  const open = positions.filter(p => p.status === 'open' || p.status === 'reducing');
  const totalRealized = closed.reduce((sum, p) => sum + (p.realized_pnl_sol ?? 0), 0);
  const totalUnrealized = open.reduce((sum, p) => sum + p.unrealized_pnl_sol, 0);
  const totalFees = positions.reduce((sum, p) => sum + p.total_fees_sol, 0);

  return [
    `💰 PnL Summary`,
    `━━━━━━━━━━━━━━━━━`,
    `Daily PnL: ${dailyPnl >= 0 ? '+' : ''}${dailyPnl.toFixed(4)} SOL`,
    `Realized: ${totalRealized >= 0 ? '+' : ''}${totalRealized.toFixed(4)} SOL`,
    `Unrealized: ${totalUnrealized >= 0 ? '+' : ''}${totalUnrealized.toFixed(4)} SOL`,
    `Total fees: ${totalFees.toFixed(4)} SOL`,
    `Trades: ${closed.length} closed, ${open.length} open`,
  ].join('\n');
}
