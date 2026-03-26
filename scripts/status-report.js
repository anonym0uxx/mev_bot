#!/usr/bin/env node
/**
 * status-report.js — sends a P&L table to Telegram every 5 min
 * Run via: node scripts/status-report.js
 */
const Database = require('better-sqlite3');
const path = require('path');

const DB_PATH = path.join(__dirname, '../data/pump-quant.db');

// TODO(integration): sendTelegram is a stub. The file-write mechanism below is DEAD CODE —
// nothing reads pending-status-message.txt reliably.  To send status reports directly to
// Telegram, replace this function with a real HTTP call to the Bot API:
//
//   const TOKEN = process.env.TELEGRAM_BOT_TOKEN;
//   const CHAT  = process.env.TELEGRAM_CHAT_ID;
//   await fetch(`https://api.telegram.org/bot${TOKEN}/sendMessage`, {
//     method: 'POST',
//     headers: { 'Content-Type': 'application/json' },
//     body: JSON.stringify({ chat_id: CHAT, text: message, parse_mode: 'Markdown' }),
//   });
//
// Until that is implemented, status-report.js only prints to stdout (see bottom of file).
async function sendTelegram(message) {
  // DEAD MECHANISM — file is never reliably consumed; kept as no-op stub only.
  // See TODO above for the proper Telegram API integration path.
  void message;
}

function buildReport() {
  const db = new Database(DB_PATH, { readonly: true });

  const rawOrders = db.prepare("SELECT * FROM orders WHERE status='confirmed' AND is_paper=0 ORDER BY created_at ASC").all();
  const open = db.prepare("SELECT * FROM positions WHERE status='OPEN'").all();

  // Deduplicate by tx_signature — keep the entry with the highest realized_sol per sig.
  // Partial fills arrive first with lower realized_sol; without dedup, duplicate rows
  // inflate buySOL/sellSOL totals and distort PnL.
  const seenSigs = new Map();
  for (const o of rawOrders) {
    const sig = o.tx_signature;
    if (sig && sig.trim() !== '') {
      const existing = seenSigs.get(sig);
      if (!existing || (o.realized_sol || 0) > (existing.realized_sol || 0)) {
        seenSigs.set(sig, o);
      }
    }
  }
  // Include orders with no tx_signature as-is (should not happen for confirmed orders, but be safe)
  const noSigOrders = rawOrders.filter(o => !o.tx_signature || o.tx_signature.trim() === '');
  const orders = [...seenSigs.values(), ...noSigOrders];

  // Group closed trades by mint
  const byMint = {};
  for (const o of orders) {
    if (!byMint[o.mint]) byMint[o.mint] = [];
    byMint[o.mint].push(o);
  }

  const trades = [];
  for (const [mint, txs] of Object.entries(byMint)) {
    const buys = txs.filter(t => t.side === 'buy');
    const sells = txs.filter(t => t.side === 'sell');
    if (buys.length === 0 || sells.length === 0) continue;
    const buySOL = buys.reduce((s,t) => s+(t.realized_sol||0), 0);
    const sellSOL = sells.reduce((s,t) => s+(t.realized_sol||0), 0);
    const fees = txs.reduce((s,t) => s+(t.fee_sol||0)+(t.priority_fee_paid_sol||0), 0);
    const pnl = sellSOL - buySOL - fees;
    trades.push({ mint, pnl, buySOL, sellSOL, fees,
      holdS: Math.round((sells[sells.length-1].confirmed_at - buys[0].confirmed_at)/1000) });
  }

  const totalPnl = trades.reduce((s,t) => s+t.pnl, 0);
  const wins = trades.filter(t => t.pnl > 0).length;
  const losses = trades.filter(t => t.pnl <= 0).length;
  const winRate = trades.length > 0 ? (wins/trades.length*100).toFixed(0) : 0;
  const totalFees = orders.reduce((s,o) => s+(o.fee_sol||0)+(o.priority_fee_paid_sol||0), 0);
  const totalCapDeployed = orders.filter(o=>o.side==='buy').reduce((s,o)=>s+(o.realized_sol||0),0);
  const feeDrag = totalCapDeployed > 0 ? (totalFees/totalCapDeployed*100).toFixed(1) : '0';

  // Open positions
  let openStr = '';
  if (open.length > 0) {
    openStr = '\n🟡 *Open:* ' + open.map(p => p.mint.slice(0,8)).join(', ');
  }

  const pnlSign = totalPnl >= 0 ? '+' : '';
  const pnlEmoji = totalPnl >= 0 ? '🟢' : '🔴';

  const now = new Date().toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', timeZone: 'America/Los_Angeles' });

  const msg = `📊 *Bot Status — ${now} PDT*

${pnlEmoji} *Net PnL:* \`${pnlSign}${totalPnl.toFixed(4)} SOL\`
🎯 *Win Rate:* ${winRate}% (${wins}W / ${losses}L)
📈 *Trades:* ${trades.length} closed
💸 *Fee Drag:* ${feeDrag}%
🏦 *Open Positions:* ${open.length}${openStr}

*Recent trades:*
${trades.slice(-5).reverse().map(t => {
  const sign = t.pnl >= 0 ? '✅' : '❌';
  const pnlStr = (t.pnl >= 0 ? '+' : '') + t.pnl.toFixed(4);
  return `${sign} \`${t.mint.slice(0,8)}\` ${pnlStr} SOL (${t.holdS}s)`;
}).join('\n') || '_No trades yet_'}`;

  db.close();
  return msg;
}

module.exports = { buildReport };

// If run directly
if (require.main === module) {
  const msg = buildReport();
  console.log(msg);
}
