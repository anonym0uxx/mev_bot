#!/usr/bin/env node
/**
 * config-snapshot.js — snapshot current config to config-history.jsonl
 * Call before every config change: node scripts/config-snapshot.js "reason for change"
 * 
 * Features:
 * - Appends full config snapshot to config-history.jsonl
 * - Shows diff vs previous snapshot
 * - Includes config_version string matching the Rust daemon format
 * 
 * Usage: node scripts/config-snapshot.js "raising trigger_min_buy_sol for selectivity"
 */
const fs = require('fs');
const path = require('path');

const BASE = path.join(__dirname, '..');
const CONFIG_PATH = path.join(BASE, 'config/canary.json');
const HISTORY_PATH = path.join(BASE, 'data/config-history.jsonl');
const TRADES_PATH = path.join(BASE, 'data/mev_paper_trades.jsonl');
const HB_STATE = path.join(BASE, 'data/heartbeat-trade-state.json');

const config = JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf8'));
const mev = config.mev || config;

const tradesRaw = fs.existsSync(TRADES_PATH) ? fs.readFileSync(TRADES_PATH, 'utf8').trim() : '';
const trades = tradesRaw ? tradesRaw.split('\n').filter(Boolean)
  .map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean) : [];

const wins = trades.filter(t => (t.pnlSol || 0) > 0).length;
const net = trades.reduce((s, t) => s + (t.netPnlSol ?? t.pnlSol ?? 0), 0);

// Compute config_version matching the Rust daemon format:
// format!("v{:.2}sol_{}ms_{}vsol", trigger_min_buy_sol, max_hold_ms, min_vsol_in_curve)
const triggerMinBuySol = mev.trigger_min_buy_sol || 0.1;
const maxHoldMs = mev.max_hold_ms || 1200;
const minVsolInCurve = mev.min_vsol_in_curve || 3;
const configVersion = `v${triggerMinBuySol.toFixed(2)}sol_${maxHoldMs}ms_${minVsolInCurve}vsol`;

const entry = {
  timestamp: new Date().toISOString(),
  reason: process.argv[2] || 'manual snapshot',
  config_version: configVersion,
  trade_count: trades.length,
  overall_wr: trades.length ? (wins / trades.length) : 0,
  net_pnl_sol: net,
  config: JSON.parse(JSON.stringify(mev)),
};

// ── Diff vs previous snapshot ───────────────────────────────────────
const historyLines = fs.existsSync(HISTORY_PATH) 
  ? fs.readFileSync(HISTORY_PATH, 'utf8').trim().split('\n').filter(Boolean)
  : [];
const prev = historyLines.length ? JSON.parse(historyLines[historyLines.length - 1]) : null;

if (prev && prev.config) {
  const diff = {};
  const curr = entry.config;
  const prevC = prev.config;
  Object.keys({...curr, ...prevC}).forEach(k => {
    if (JSON.stringify(curr[k]) !== JSON.stringify(prevC[k])) {
      diff[k] = { from: prevC[k], to: curr[k] };
    }
  });
  if (Object.keys(diff).length) {
    console.log('Config diff vs previous snapshot:');
    Object.entries(diff).forEach(([k, {from, to}]) => 
      console.log(`  ${k}: ${JSON.stringify(from)} → ${JSON.stringify(to)}`)
    );
  } else {
    console.log('No config changes vs previous snapshot');
  }
  if (prev.config_version && prev.config_version !== configVersion) {
    console.log(`Config version changed: ${prev.config_version} → ${configVersion}`);
  }
} else {
  console.log('First snapshot — no previous entry to diff against');
}

// ── Write snapshot ──────────────────────────────────────────────────
fs.appendFileSync(HISTORY_PATH, JSON.stringify(entry) + '\n');
console.log(`\nSnapshot written: ${entry.timestamp}`);
console.log(`Config version: ${configVersion}`);
console.log(`Trades: ${entry.trade_count} | WR: ${(entry.overall_wr * 100).toFixed(2)}% | Net: ${entry.net_pnl_sol.toFixed(4)} SOL`);
console.log(`Key params: trigger_min_buy_sol=${mev.trigger_min_buy_sol} max_hold_ms=${mev.max_hold_ms} min_vsol_in_curve=${mev.min_vsol_in_curve} max_vsol_in_curve=${mev.max_vsol_in_curve}`);
