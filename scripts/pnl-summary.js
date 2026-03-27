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

// Scalper stats
const rawOrders = db.prepare(`SELECT * FROM orders WHERE status='confirmed' AND is_paper=${paperFlag} ORDER BY created_at ASC`).all();
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
const scalperTrades = [];
for (const [mint, txs] of Object.entries(byMint)) {
  const buys = txs.filter(t => t.side === 'buy');
  const sells = txs.filter(t => t.side === 'sell');
  if (!buys.length || !sells.length) continue;
  const buySOL = buys.reduce((s,t) => s + (t.realized_sol||0), 0);
  const sellSOL = sells.reduce((s,t) => s + (t.realized_sol||0), 0);
  const fees = txs.reduce((s,t) => s + (t.fee_sol||0) + (t.priority_fee_paid_sol||0), 0);
  scalperTrades.push({ pnl: sellSOL - buySOL - fees });
}
const sWins = scalperTrades.filter(t => t.pnl > 0).length;
const sTotal = scalperTrades.length;
const sPnl = scalperTrades.reduce((s,t) => s + t.pnl, 0);
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
