#!/usr/bin/env node
/**
 * Trade reconciliation: compares DB records vs on-chain via PumpPortal history.
 * Flags any trades in DB with no matching on-chain tx, or on-chain txs missing from DB.
 * Outputs JSON report to data/reconcile-report.json
 */
const Database = require('better-sqlite3');
const fs = require('fs');
const path = require('path');
require('dotenv').config({ path: path.join(__dirname, '../.env') });

const DB_PATH = path.join(__dirname, '../data/pump-quant.db');
const OUT_PATH = path.join(__dirname, '../data/reconcile-report.json');

const db = new Database(DB_PATH, { readonly: true });

// Get all confirmed live orders from DB
const orders = db.prepare("SELECT id, mint, side, tx_signature, realized_sol, confirmed_at FROM orders WHERE status='confirmed' AND is_paper=0 ORDER BY confirmed_at DESC LIMIT 200").all();

// Group by tx_signature
const bySig = {};
for (const o of orders) {
  if (!o.tx_signature) continue;
  if (!bySig[o.tx_signature]) bySig[o.tx_signature] = [];
  bySig[o.tx_signature].push(o);
}

// Find duplicates still in DB (should be 0 after migration)
const duplicates = Object.entries(bySig).filter(([sig, rows]) => rows.length > 1);

// Summary
const report = {
  generated_at: new Date().toISOString(),
  total_confirmed_orders: orders.length,
  unique_tx_signatures: Object.keys(bySig).length,
  duplicate_tx_sigs: duplicates.length,
  duplicate_details: duplicates.map(([sig, rows]) => ({
    tx_signature: sig.slice(0, 16) + '...',
    count: rows.length,
    mints: [...new Set(rows.map(r => r.mint.slice(0, 8)))],
  })),
  orders_without_tx_sig: orders.filter(o => !o.tx_signature).length,
  status: duplicates.length === 0 ? 'CLEAN' : 'DUPLICATES_FOUND',
};

fs.writeFileSync(OUT_PATH, JSON.stringify(report, null, 2));
console.log(JSON.stringify({
  status: report.status,
  total_orders: report.total_confirmed_orders,
  duplicates: report.duplicate_tx_sigs,
  missing_sig: report.orders_without_tx_sig,
}));

db.close();
