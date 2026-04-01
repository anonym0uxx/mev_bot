#!/usr/bin/env node
/**
 * One-time cleanup: fix realized_pnl_sol in positions table
 * by recomputing from deduplicated orders.
 *
 * Deduplication: by tx_signature (keep first), or fallback
 * to (mint, side, confirmed_at) 500ms bucket.
 *
 * Scope: all positions with status='closed' or 'CLOSED'
 */
const Database = require('better-sqlite3');
const path = require('path');

const DB_PATH = path.join(__dirname, '../data/pump-quant.db');
const db = new Database(DB_PATH);

function deduplicateOrders(orders) {
  const seenSigs = new Map();
  const noSigGroups = new Map();
  const deduped = [];

  for (const o of orders) {
    const sig = o.tx_signature;
    if (sig && sig.trim() !== '') {
      if (!seenSigs.has(sig)) {
        seenSigs.set(sig, o);
        deduped.push(o);
      }
    } else {
      const bucket = Math.floor((o.confirmed_at || 0) / 500) * 500;
      const key = `${o.mint}|${o.side}|${bucket}`;
      if (!noSigGroups.has(key)) {
        noSigGroups.set(key, o);
        deduped.push(o);
      }
    }
  }
  return deduped;
}

// Load all confirmed orders
const allOrders = db.prepare(`
  SELECT * FROM orders WHERE status='confirmed' AND is_paper=0 ORDER BY created_at ASC
`).all();

const dedupedOrders = deduplicateOrders(allOrders);
console.log(`Orders: raw=${allOrders.length} deduped=${dedupedOrders.length} removed=${allOrders.length - dedupedOrders.length}`);

// Group by mint
const byMint = {};
for (const o of dedupedOrders) {
  if (!byMint[o.mint]) byMint[o.mint] = [];
  byMint[o.mint].push(o);
}

// Load all closed positions
const positions = db.prepare(`
  SELECT * FROM positions WHERE status='closed' OR status='CLOSED'
`).all();

console.log(`\nPositions to fix: ${positions.length}`);
console.log('─'.repeat(80));

let fixed = 0;
let skipped = 0;
let totalBeforePnl = 0;
let totalAfterPnl = 0;

const updateStmt = db.prepare(`
  UPDATE positions SET realized_pnl_sol = ?, total_fees_sol = ? WHERE id = ?
`);

db.transaction(() => {
  for (const pos of positions) {
    const orders = byMint[pos.mint] || [];
    if (orders.length === 0) {
      console.log(`  SKIP ${pos.mint.slice(0,12)} id=${pos.id} — no orders found`);
      skipped++;
      continue;
    }

    const buys = orders.filter(o => o.side === 'buy');
    const sells = orders.filter(o => o.side === 'sell');
    if (sells.length === 0) {
      console.log(`  SKIP ${pos.mint.slice(0,12)} id=${pos.id} — no sell orders`);
      skipped++;
      continue;
    }

    const buySOL = buys.reduce((s, o) => s + (o.realized_sol || 0), 0);
    const sellSOL = sells.reduce((s, o) => s + (o.realized_sol || 0), 0);
    const fees = orders.reduce((s, o) => s + (o.fee_sol || 0) + (o.priority_fee_paid_sol || 0), 0);
    const newPnl = sellSOL - buySOL - fees;
    const oldPnl = pos.realized_pnl_sol || 0;

    totalBeforePnl += oldPnl;
    totalAfterPnl += newPnl;

    if (Math.abs(newPnl - oldPnl) > 0.000001) {
      console.log(`  FIX  ${pos.mint.slice(0,12)} id=${pos.id} pnl: ${oldPnl.toFixed(6)} → ${newPnl.toFixed(6)} (buy=${buySOL.toFixed(4)} sell=${sellSOL.toFixed(4)} fees=${fees.toFixed(5)})`);
      updateStmt.run(newPnl, fees, pos.id);
      fixed++;
    } else {
      console.log(`  OK   ${pos.mint.slice(0,12)} id=${pos.id} pnl=${oldPnl.toFixed(6)} (no change needed)`);
    }
  }
})();

console.log('\n' + '─'.repeat(80));
console.log(`Summary:`);
console.log(`  Positions checked: ${positions.length}`);
console.log(`  Fixed: ${fixed}`);
console.log(`  Skipped: ${skipped}`);
console.log(`  Total PnL BEFORE cleanup: ${totalBeforePnl.toFixed(6)} SOL`);
console.log(`  Total PnL AFTER  cleanup: ${totalAfterPnl.toFixed(6)} SOL`);
console.log(`  Delta: ${(totalAfterPnl - totalBeforePnl).toFixed(6)} SOL`);
console.log('\nDone.');
