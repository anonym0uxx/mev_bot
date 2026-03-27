#!/usr/bin/env node
/**
 * Quant Monitor — runs every 30 min via OpenClaw cron.
 * Analyzes CURRENT SESSION scalper trades only (positions opened since last daemon start).
 * Uses positions table with 24h window to avoid stale historical data triggering false alerts.
 */
const Database = require('better-sqlite3');
const fs = require('fs');
const path = require('path');

const DB_PATH = path.join(__dirname, '../data/pump-quant.db');
const STATE_PATH = path.join(__dirname, '../data/heartbeat-trade-state.json');
const LOG_PATH = path.join(__dirname, '../data/improvement-log.json');

const db = new Database(DB_PATH, { readonly: true });

let state = { last_trade_count: 0, last_check_ts: 0, last_win_rate: null, pending_analysis: false };
try { state = JSON.parse(fs.readFileSync(STATE_PATH, 'utf8')); } catch {}

const isPaperMode = process.env.PAPER_MODE === 'true' || process.env.PAPER_MODE === '1';
const paperFilter = isPaperMode ? 1 : 0;

// Use 24h window on positions table — avoids stale historical sessions
const windowStart = Date.now() - 24 * 60 * 60 * 1000;

const positions = db.prepare(`
  SELECT realized_pnl_sol as pnl, opened_at as ts, exit_reason, regime
  FROM positions
  WHERE is_paper=? AND status='closed' AND opened_at >= ?
  ORDER BY opened_at ASC
`).all(paperFilter, windowStart);

const totalTrades = positions.length;
const wins = positions.filter(p => p.pnl > 0).length;
const winRate = totalTrades > 0 ? wins / totalTrades : 0;
const totalPnl = positions.reduce((s, p) => s + (p.pnl || 0), 0);

const recent10 = positions.slice(-10);
const recent10WR = recent10.length > 0 ? recent10.filter(p => p.pnl > 0).length / recent10.length : 0;
const recent10Pnl = recent10.reduce((s, p) => s + p.pnl, 0);

const newTrades = Math.max(0, totalTrades - (state.last_trade_count || 0));

// Fee drag from real orders only (live mode metric)
const rawOrders = db.prepare(`SELECT fee_sol, priority_fee_paid_sol, realized_sol, side FROM orders WHERE status='confirmed' AND is_paper=0`).all();
const allFees = rawOrders.reduce((s, o) => s + (o.fee_sol || 0) + (o.priority_fee_paid_sol || 0), 0);
const allBuys = rawOrders.filter(o => o.side === 'buy').reduce((s, o) => s + (o.realized_sol || 0), 0);
const feeDrag = allBuys > 0 ? (allFees / allBuys * 100) : 0;

// Threshold stats from learning ledger
let thresholdStats = {};
try {
  const edges = db.prepare(`SELECT feat_entry_edge FROM learning_ledger WHERE feat_entry_edge IS NOT NULL ORDER BY feat_entry_edge ASC`).all().map(r => r.feat_entry_edge);
  if (!edges.length) {
    const altEdges = db.prepare(`SELECT json_extract(feature_snapshot, '$.entry_edge') as e FROM learning_ledger WHERE feature_snapshot IS NOT NULL`).all().map(r => r.e).filter(v => v != null);
    if (altEdges.length) {
      altEdges.sort((a,b)=>a-b);
      const p = (arr, pct) => arr[Math.floor(arr.length * pct)];
      thresholdStats = { edgeP50: p(altEdges,0.5)?.toFixed(6), edgeMax: p(altEdges,0.99)?.toFixed(6), samples: altEdges.length };
    }
  } else {
    const p = (arr, pct) => arr[Math.floor(arr.length * pct)];
    thresholdStats = { edgeP50: p(edges,0.5)?.toFixed(6), edgeMax: p(edges,0.99)?.toFixed(6), samples: edges.length };
  }
} catch(_) {}

const lastTradeTs = positions.length > 0 ? positions[positions.length - 1].ts : 0;
const sessionDormantMs = Date.now() - lastTradeTs;
// Dormant if: no trades in 24h window OR no new trades since last check AND last trade >3h ago
const sessionDormant = totalTrades === 0 || (newTrades === 0 && sessionDormantMs > 3 * 60 * 60 * 1000);

const report = {
  generated_at: new Date().toISOString(),
  mode: isPaperMode ? 'paper' : 'live',
  window: '24h',
  total_trades: totalTrades,
  new_since_last_check: newTrades,
  wins, losses: totalTrades - wins,
  win_rate_pct: (winRate * 100).toFixed(1),
  total_pnl_sol: totalPnl.toFixed(5),
  recent_10_win_rate_pct: (recent10WR * 100).toFixed(1),
  recent_10_pnl_sol: recent10Pnl.toFixed(5),
  fee_drag_pct: feeDrag.toFixed(2),
  threshold_stats: thresholdStats,
  worst_recent: recent10.filter(p => p.pnl < 0).sort((a,b) => a.pnl - b.pnl).slice(0,3).map(p => ({ pnl: p.pnl.toFixed(5), exit: p.exit_reason })),
  alerts: [],
  recommendation_needed: false,
};

if (sessionDormant) {
  report.alerts.push(`SCALPER_DORMANT: ${totalTrades === 0 ? 'no trades in last 24h' : `no new trades in ${(sessionDormantMs/3600000).toFixed(1)}h`} — skipping auto-tune`);
}

if (!sessionDormant) {
  if (winRate < 0.30 && totalTrades >= 30) report.alerts.push(`WIN_RATE_LOW: ${(winRate*100).toFixed(1)}% on ${totalTrades} trades`);
  if (totalPnl < -0.03 && totalTrades >= 30) report.alerts.push(`PNL_CRITICAL: ${totalPnl.toFixed(4)} SOL`);
  if (feeDrag > 5) report.alerts.push(`FEE_DRAG_HIGH: ${feeDrag.toFixed(1)}%`);
  if (newTrades >= 5 && recent10WR < 0.35 && totalTrades >= 30) {
    report.alerts.push(`LOSS_PATTERN: ${newTrades} new trades, recent WR=${(recent10WR*100).toFixed(0)}%`);
    report.recommendation_needed = true;
  }
}

console.log(JSON.stringify(report, null, 2));
db.close();
