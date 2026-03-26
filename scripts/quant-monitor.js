#!/usr/bin/env node
/**
 * Quant Monitor — runs every 30 min via OpenClaw cron.
 * Analyzes recent trades, detects loss patterns, outputs recommendation JSON.
 * Apollo reads this output and decides whether to apply + alert.
 */
const Database = require('better-sqlite3');
const fs = require('fs');
const path = require('path');

const DB_PATH = path.join(__dirname, '../data/pump-quant.db');
const STATE_PATH = path.join(__dirname, '../data/heartbeat-trade-state.json');
const LOG_PATH = path.join(__dirname, '../data/improvement-log.json');

const db = new Database(DB_PATH, { readonly: true });

// Load state
let state = { last_trade_count: 0, last_check_ts: 0, last_win_rate: null, pending_analysis: false };
try { state = JSON.parse(fs.readFileSync(STATE_PATH, 'utf8')); } catch {}

// Dedup orders
const rawOrders = db.prepare("SELECT * FROM orders WHERE status='confirmed' AND is_paper=0 ORDER BY created_at ASC").all();
const sigBest = new Map();
for (const o of rawOrders) {
  if (!o.tx_signature) continue;
  const prev = sigBest.get(o.tx_signature);
  if (!prev || (o.realized_sol||0) > (prev.realized_sol||0)) sigBest.set(o.tx_signature, o);
}
const orders = [...sigBest.values(), ...rawOrders.filter(o => !o.tx_signature)];

const byMint = {};
for (const o of orders) {
  if (!byMint[o.mint]) byMint[o.mint] = [];
  byMint[o.mint].push(o);
}

const trades = [];
for (const [mint, txs] of Object.entries(byMint)) {
  const buys = txs.filter(t => t.side === 'buy');
  const sells = txs.filter(t => t.side === 'sell');
  if (!buys.length || !sells.length) continue;
  const buySOL = buys.reduce((s,t) => s + (t.realized_sol||0), 0);
  const sellSOL = sells.reduce((s,t) => s + (t.realized_sol||0), 0);
  const fees = txs.reduce((s,t) => s + (t.fee_sol||0) + (t.priority_fee_paid_sol||0), 0);
  const pnl = sellSOL - buySOL - fees;
  const latestTs = Math.max(...txs.map(t => t.confirmed_at || 0));
  trades.push({ mint: mint.slice(0,8), pnl, win: pnl > 0, ts: latestTs });
}
trades.sort((a,b) => a.ts - b.ts);

const totalTrades = trades.length;
const wins = trades.filter(t => t.win).length;
const winRate = totalTrades > 0 ? wins / totalTrades : 0;
const totalPnl = trades.reduce((s,t) => s + t.pnl, 0);

// Recent 10 trades
const recent10 = trades.slice(-10);
const recent10WR = recent10.length > 0 ? recent10.filter(t=>t.win).length / recent10.length : 0;
const recent10Pnl = recent10.reduce((s,t) => s+t.pnl, 0);

// New trades since last check
const newTrades = totalTrades - state.last_trade_count;

// Fee drag
const allFees = rawOrders.reduce((s,o) => s + (o.fee_sol||0) + (o.priority_fee_paid_sol||0), 0);
const allBuys = rawOrders.filter(o=>o.side==='buy').reduce((s,o) => s + (o.realized_sol||0), 0);
const feeDrag = allBuys > 0 ? (allFees / allBuys * 100) : 0;

const report = {
  generated_at: new Date().toISOString(),
  total_trades: totalTrades,
  new_since_last_check: newTrades,
  wins, losses: totalTrades - wins,
  win_rate_pct: (winRate * 100).toFixed(1),
  total_pnl_sol: totalPnl.toFixed(5),
  recent_10_win_rate_pct: (recent10WR * 100).toFixed(1),
  recent_10_pnl_sol: recent10Pnl.toFixed(5),
  fee_drag_pct: feeDrag.toFixed(2),
  worst_recent: recent10.filter(t=>!t.win).sort((a,b)=>a.pnl-b.pnl).slice(0,3).map(t=>({mint:t.mint,pnl:t.pnl.toFixed(5)})),
  alerts: [],
  recommendation_needed: false,
};

if (winRate < 0.30 && totalTrades >= 10) report.alerts.push(`WIN_RATE_LOW: ${(winRate*100).toFixed(1)}% on ${totalTrades} trades`);
if (totalPnl < -0.03) report.alerts.push(`PNL_CRITICAL: ${totalPnl.toFixed(4)} SOL`);
if (feeDrag > 5) report.alerts.push(`FEE_DRAG_HIGH: ${feeDrag.toFixed(1)}%`);
if (newTrades >= 5 && recent10WR < 0.35) {
  report.alerts.push(`LOSS_PATTERN: ${newTrades} new trades, recent WR=${(recent10WR*100).toFixed(0)}%`);
  report.recommendation_needed = true;
}

// CEILING VIOLATION CHECK: detect if config threshold is above model's empirical output ceiling
// This is the root cause of entry droughts. Auto-fix by resetting to adaptive p50.
try {
  const THRESHOLD_STATE_PATH = path.join(__dirname, '../data/threshold_state.json');
  const CONFIG_PATH = path.join(__dirname, '../config/canary.json');
  if (fs.existsSync(THRESHOLD_STATE_PATH) && fs.existsSync(CONFIG_PATH)) {
    const threshState = JSON.parse(fs.readFileSync(THRESHOLD_STATE_PATH, 'utf8'));
    const cfg = JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf8'));
    const edgeWindow = threshState.edgeWindow || [];
    if (edgeWindow.length >= 50) {
      const sorted = [...edgeWindow].sort((a,b) => a-b);
      const edgeMax = sorted[sorted.length - 1];
      const edgeP50 = sorted[Math.floor(sorted.length * 0.5)];
      const configThreshold = cfg.entry?.min_entry_edge || 0;
      if (configThreshold > edgeMax && edgeMax > 0) {
        // AUTO-FIX: ceiling violation — threshold above all observed signals
        const newThreshold = Math.max(0.0001, edgeP50 * 0.8);
        cfg.entry.min_entry_edge = parseFloat(newThreshold.toFixed(6));
        fs.writeFileSync(CONFIG_PATH, JSON.stringify(cfg, null, 2));
        report.alerts.push(`CEILING_VIOLATION_AUTOFIX: min_entry_edge ${configThreshold.toFixed(6)} > edgeMax ${edgeMax.toFixed(6)} — reset to ${newThreshold.toFixed(6)}`);
        report.recommendation_needed = false; // we already fixed it
      }
      // Add threshold stats to report
      report.threshold_stats = { edgeP50: edgeP50.toFixed(6), edgeMax: edgeMax.toFixed(6), configThreshold: configThreshold.toFixed(6), samples: edgeWindow.length };
    }
  }
} catch (e) { /* non-fatal */ }

// Update state
const newState = {
  last_trade_count: totalTrades,
  last_check_ts: Date.now(),
  last_win_rate: winRate,
  pending_analysis: report.recommendation_needed,
};
fs.writeFileSync(STATE_PATH, JSON.stringify(newState, null, 2));

console.log(JSON.stringify(report, null, 2));
db.close();
