#!/usr/bin/env node
/**
 * P&L Summary reporter — runs hourly via OpenClaw cron or heartbeat.
 * Outputs combined scalper + MEV stats as a formatted Telegram message.
 */
const Database = require('better-sqlite3');
const fs = require('fs');
const path = require('path');

const DB_PATH = path.join(__dirname, '../data/pump-quant.db');
const MEV_LOG = path.join(__dirname, '../data/mev_paper_trades.jsonl');
const isPaper = process.env.PAPER_MODE === 'true' || process.env.PAPER_MODE === '1';
const paperFlag = isPaper ? 1 : 0;

const db = new Database(DB_PATH, { readonly: true });

// Scalper stats — read from positions table (source of truth for realized P&L)
const scalperRows = db.prepare(
  `SELECT realized_pnl_sol, status FROM positions WHERE is_paper=${paperFlag} AND status='closed'`
).all();
const sTotal = scalperRows.length;
const sWins = scalperRows.filter(r => r.realized_pnl_sol > 0).length;
const sPnl = scalperRows.reduce((s, r) => s + (r.realized_pnl_sol || 0), 0);
const sWR = sTotal > 0 ? (sWins/sTotal*100).toFixed(1) : '—';

// MEV stats
let mWins=0, mTotal=0, mPnl=0;
try {
  const lines = fs.readFileSync(MEV_LOG,'utf8').trim().split('\n').filter(Boolean).map(l=>JSON.parse(l));
  mTotal = lines.length;
  mWins = lines.filter(l=>l.pnlSol>0).length;
  mPnl = lines.reduce((s,l)=>s+l.pnlSol,0);
} catch(_) {}
const mWR = mTotal > 0 ? (mWins/mTotal*100).toFixed(1) : '—';

const mode = isPaper ? '📄 PAPER' : '🔴 LIVE';
const msg = [
  `${mode} — Hourly P&L Summary`,
  ``,
  `📊 Scalper: ${sTotal} trades | WR ${sWR}% | PnL ${sPnl>=0?'+':''}${sPnl.toFixed(4)} SOL`,
  `🎯 MEV Backrun: ${mTotal} trades | WR ${mWR}% | PnL ${mPnl>=0?'+':''}${mPnl.toFixed(4)} SOL`,
  `💰 Combined PnL: ${(sPnl+mPnl)>=0?'+':''}${(sPnl+mPnl).toFixed(4)} SOL`,
].join('\n');

console.log(msg);
