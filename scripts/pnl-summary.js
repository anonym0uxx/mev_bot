#!/usr/bin/env node
/**
 * P&L Summary reporter — two blocks sent every 5 minutes via heartbeat.
 *
 * Block 1: Engine Status — current session (since last daemon restart), engine version, mode
 * Block 2: Overall P&L  — all-time totals across scalper + MEV
 */
const Database = require('better-sqlite3');
const fs = require('fs');
const path = require('path');

const DB_PATH = path.join(__dirname, '../data/pump-quant.db');
const MEV_LOG = path.join(__dirname, '../data/mev_paper_trades.jsonl');
const ENGINE_STATE = path.join(__dirname, '../data/engine-state.json');
const isPaper = process.env.PAPER_MODE === 'true' || process.env.PAPER_MODE === '1';
const paperFlag = isPaper ? 1 : 0;
const mode = isPaper ? '📄 PAPER' : '🔴 LIVE';

const db = new Database(DB_PATH, { readonly: true });

// --- Determine session start (daemon restart time) ---
// Read from engine-state.json if available, else fall back to process uptime heuristic
let sessionStartMs = Date.now() - 24 * 60 * 60 * 1000; // fallback: last 24h
try {
  const state = JSON.parse(fs.readFileSync(ENGINE_STATE, 'utf8'));
  if (state.daemonStartedAt) sessionStartMs = state.daemonStartedAt;
} catch (_) {}

// --- Load MEV JSONL ---
let allMevTrades = [];
try {
  allMevTrades = fs.readFileSync(MEV_LOG, 'utf8').trim().split('\n').filter(Boolean).map(l => JSON.parse(l));
} catch (_) {}

// Session trades = since last daemon restart
const sessionTrades = allMevTrades.filter(t => t.entryTimestampMs >= sessionStartMs && !t.excludeFromAnalysis);
const sSesTotal = sessionTrades.length;
const sSesWins = sessionTrades.filter(t => t.pnlSol > 0).length;
const sSesLosses = sessionTrades.filter(t => t.pnlSol < 0).length;
const sSesFlat = sessionTrades.filter(t => t.pnlSol === 0).length;
const sSesWR = sSesTotal > 0 ? (sSesWins / sSesTotal * 100).toFixed(1) : '—';
const sSesPnl = sessionTrades.reduce((s, t) => s + (t.pnlSol || 0), 0);
const sSesNetPnl = sessionTrades.reduce((s, t) => s + (t.netPnlSol != null ? t.netPnlSol : (t.pnlSol || 0) - (t.sizeSol || 0) * 0.02 - 0.0001), 0);
const sSesTP = sessionTrades.filter(t => t.exitReason === 'take_profit').length;
const sSesNB = sessionTrades.filter(t => t.exitReason === 'next_buyer').length;
const sSesSL = sessionTrades.filter(t => t.exitReason === 'stop_loss').length;
const sSesMH = sessionTrades.filter(t => t.exitReason === 'max_hold').length;

// Engine version from state file
let engineVersion = 'v5';
let configVersion = '—';
try {
  const state = JSON.parse(fs.readFileSync(ENGINE_STATE, 'utf8'));
  if (state.engineVersion) engineVersion = state.engineVersion;
  if (state.configVersion) configVersion = state.configVersion;
} catch (_) {}

// --- All-time MEV stats (exclude bug trades) ---
const cleanMevTrades = allMevTrades.filter(l => !l.excludeFromAnalysis);
const mTotal = cleanMevTrades.length;
const mWins = cleanMevTrades.filter(l => l.pnlSol > 0).length;
const mPnl = cleanMevTrades.reduce((s, l) => s + (l.pnlSol || 0), 0);
const mNetPnl = cleanMevTrades.reduce((s, l) => s + (l.netPnlSol != null ? l.netPnlSol : (l.pnlSol || 0) - (l.sizeSol || 0) * 0.02 - 0.0001), 0);
const mWR = mTotal > 0 ? (mWins / mTotal * 100).toFixed(1) : '—';

// --- Scalper all-time ---
let sTotal = 0, sWins = 0, sPnl = 0;
try {
  const scalperRows = db.prepare(
    `SELECT realized_pnl_sol FROM positions WHERE is_paper=${paperFlag} AND status='closed'`
  ).all();
  sTotal = scalperRows.length;
  sWins = scalperRows.filter(r => r.realized_pnl_sol > 0).length;
  sPnl = scalperRows.reduce((s, r) => s + (r.realized_pnl_sol || 0), 0);
} catch (_) {}
const sWR = sTotal > 0 ? (sWins / sTotal * 100).toFixed(1) : '—';

const combinedPnl = sPnl + mPnl;

// --- Format session start label ---
const uptimeMs = Date.now() - sessionStartMs;
const uptimeMins = Math.floor(uptimeMs / 60000);
const uptimeLabel = uptimeMins < 60
  ? `${uptimeMins}m`
  : `${Math.floor(uptimeMins / 60)}h${uptimeMins % 60}m`;

const combinedNetPnl = mNetPnl + sPnl; // scalper has no fee model yet

// --- Block 1: Engine Status ---
const block1 = [
  `⚙️ Engine ${engineVersion} | ${mode} | Up ${uptimeLabel}`,
  ``,
  `Session (${sSesTotal} trades):`,
  `  WR ${sSesWR}% | Gross ${sSesPnl >= 0 ? '+' : ''}${sSesPnl.toFixed(4)} | Net ${sSesNetPnl >= 0 ? '+' : ''}${sSesNetPnl.toFixed(4)} SOL`,
  `  tp=${sSesTP} nb=${sSesNB} sl=${sSesSL} mh=${sSesMH}`,
].join('\n');

// --- Block 2: Overall P&L ---
const block2 = [
  `📊 Overall ${mode} P&L`,
  ``,
  `🎯 MEV: ${mTotal} trades | WR ${mWR}% | Gross ${mPnl >= 0 ? '+' : ''}${mPnl.toFixed(4)} | Net ${mNetPnl >= 0 ? '+' : ''}${mNetPnl.toFixed(4)} SOL`,
  `💰 Net: ${mNetPnl >= 0 ? '+' : ''}${mNetPnl.toFixed(4)} SOL`,
  `   (break-even WR: ~66.5% gross)`,
].join('\n');

console.log(block1);
console.log('');
console.log(block2);
