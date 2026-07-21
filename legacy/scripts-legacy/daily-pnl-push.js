#!/usr/bin/env node
/**
 * Daily PnL summary push — called by heartbeat or cron.
 * Outputs the status report to stdout for Apollo to send via Telegram.
 */
const Database = require('better-sqlite3');
const path = require('path');
require('dotenv').config({ path: path.join(__dirname, '../.env') });

const DB_PATH = path.join(__dirname, '../data/pump-quant.db');
const db = new Database(DB_PATH, { readonly: true });

// Dedup orders (keep max realized_sol per tx_sig)
const rawOrders = db.prepare("SELECT * FROM orders WHERE status='confirmed' AND is_paper=0 ORDER BY created_at ASC").all();
const sigBest = new Map();
for (const o of rawOrders) {
  if (!o.tx_signature) continue;
  const prev = sigBest.get(o.tx_signature);
  if (!prev || (o.realized_sol || 0) > (prev.realized_sol || 0)) sigBest.set(o.tx_signature, o);
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
  const buySOL = buys.reduce((s, t) => s + (t.realized_sol || 0), 0);
  const sellSOL = sells.reduce((s, t) => s + (t.realized_sol || 0), 0);
  const fees = txs.reduce((s, t) => s + (t.fee_sol || 0) + (t.priority_fee_paid_sol || 0), 0);
  trades.push({ pnl: sellSOL - buySOL - fees, win: (sellSOL - buySOL - fees) > 0 });
}

// Today only
const since24h = Date.now() - 86400000;
const todayOrders = orders.filter(o => (o.confirmed_at || 0) > since24h);
const todayMints = {};
for (const o of todayOrders) {
  if (!todayMints[o.mint]) todayMints[o.mint] = [];
  todayMints[o.mint].push(o);
}
const todayTrades = [];
for (const [mint, txs] of Object.entries(todayMints)) {
  const buys = txs.filter(t => t.side === 'buy');
  const sells = txs.filter(t => t.side === 'sell');
  if (!buys.length || !sells.length) continue;
  const buySOL = buys.reduce((s, t) => s + (t.realized_sol || 0), 0);
  const sellSOL = sells.reduce((s, t) => s + (t.realized_sol || 0), 0);
  const fees = txs.reduce((s, t) => s + (t.fee_sol || 0) + (t.priority_fee_paid_sol || 0), 0);
  todayTrades.push({ pnl: sellSOL - buySOL - fees, win: (sellSOL - buySOL - fees) > 0 });
}

const allPnl = trades.reduce((s, t) => s + t.pnl, 0);
const allWins = trades.filter(t => t.win).length;
const todayPnl = todayTrades.reduce((s, t) => s + t.pnl, 0);
const todayWins = todayTrades.filter(t => t.win).length;
const sign = allPnl >= 0 ? '+' : '';
const todaySign = todayPnl >= 0 ? '+' : '';
const emoji = allPnl >= 0 ? '🟢' : '🔴';
const todayEmoji = todayPnl >= 0 ? '📈' : '📉';

const now = new Date().toLocaleString('en-US', { timeZone: 'America/Los_Angeles', hour: '2-digit', minute: '2-digit', month: 'short', day: 'numeric' });

const msg = `🤖 *Daily PnL Summary — ${now} PDT*

${emoji} *All-Time PnL:* \`${sign}${allPnl.toFixed(4)} SOL\`
${todayEmoji} *Today PnL:* \`${todaySign}${todayPnl.toFixed(4)} SOL\`

📊 *All-Time:* ${allWins}W / ${trades.length - allWins}L (${trades.length > 0 ? (allWins / trades.length * 100).toFixed(0) : 0}% WR)
📊 *Today:* ${todayWins}W / ${todayTrades.length - todayWins}L (${todayTrades.length > 0 ? (todayWins / todayTrades.length * 100).toFixed(0) : 0}% WR)`;

console.log(msg);
db.close();
