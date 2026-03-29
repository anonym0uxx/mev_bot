#!/usr/bin/env node
/**
 * config-snapshot.js — snapshot current config to config-history.jsonl
 * Call before every config change: node scripts/config-snapshot.js "reason for change"
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
const tradesRaw = fs.existsSync(TRADES_PATH) ? fs.readFileSync(TRADES_PATH, 'utf8').trim() : '';
const trades = tradesRaw ? tradesRaw.split('\n').filter(Boolean)
  .map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean) : [];

const wins = trades.filter(t => (t.pnlSol || 0) > 0).length;
const net = trades.reduce((s, t) => s + (t.netPnlSol ?? t.pnlSol ?? 0), 0);
const hbState = fs.existsSync(HB_STATE) ? JSON.parse(fs.readFileSync(HB_STATE, 'utf8')) : {};

const entry = {
  timestamp: new Date().toISOString(),
  reason: process.argv[2] || 'manual snapshot',
  trade_count: trades.length,
  overall_wr: trades.length ? (wins / trades.length) : 0,
  net_pnl_sol: net,
  config: JSON.parse(JSON.stringify(config.mev || config)),
};

fs.appendFileSync(HISTORY_PATH, JSON.stringify(entry) + '\n');
console.log(`Snapshot written: ${entry.timestamp}`);
console.log(`Trades: ${entry.trade_count} | WR: ${(entry.overall_wr * 100).toFixed(2)}% | Net: ${entry.net_pnl_sol.toFixed(4)} SOL`);
console.log(`Key params: trigger_min_buy_sol=${entry.config.trigger_min_buy_sol} max_hold_ms=${entry.config.max_hold_ms} pre_trigger_min_buys_1s=${entry.config.pre_trigger_min_buys_1s}`);
