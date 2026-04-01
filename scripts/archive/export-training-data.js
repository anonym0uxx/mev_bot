#!/usr/bin/env node
/**
 * Export ML training data from pump-quant.db for LightGBM training.
 * Label: realized_pnl > entry_cost * 0.02 (2% gain = win)
 * Features: all feat_* columns + derived features
 */
const Database = require('better-sqlite3');
const fs = require('fs');
const path = require('path');

const DB_PATH = path.join(__dirname, '../data/pump-quant.db');
const OUT_PATH = path.join(__dirname, '../data/ml_training_data.csv');

const db = new Database(DB_PATH, { readonly: true });

// Check which columns exist
const cols = db.prepare('PRAGMA table_info(orders)').all().map(c => c.name);
const hasFeatures = cols.includes('feat_p_cont');

if (!hasFeatures) {
  console.log('No ML feature columns found. Run after collecting trades with migration 005+.');
  process.exit(0);
}

// Get completed paper trades with features
const trades = db.prepare(`
  SELECT o.*, 
    buy.realized_sol as buy_sol,
    sell.realized_sol as sell_sol,
    buy.fee_sol + buy.priority_fee_paid_sol + sell.fee_sol + sell.priority_fee_paid_sol as total_fees
  FROM orders o
  JOIN orders buy ON buy.mint = o.mint AND buy.side = 'buy' AND buy.is_paper = o.is_paper
  JOIN orders sell ON sell.mint = o.mint AND sell.side = 'sell' AND sell.is_paper = o.is_paper
  WHERE o.side = 'buy' 
    AND o.is_paper = 1
    AND o.feat_p_cont IS NOT NULL
    AND o.status = 'confirmed'
  LIMIT 10000
`).all();

if (trades.length === 0) {
  console.log('No trades with ML features found yet. Keep paper trading.');
  process.exit(0);
}

// Build CSV
const headers = ['label','feat_p_cont','feat_bcd_score','feat_manip_score','feat_creator_prior',
  'feat_velocity','feat_breadth_score','feat_unique_buyers','feat_mcap_sol',
  'active_stop_pct','active_target_pct','active_max_hold_s'];

const rows = [headers.join(',')];
let labeled = 0;
for (const t of trades) {
  const pnl = (t.sell_sol || 0) - (t.buy_sol || 0) - (t.total_fees || 0);
  const label = pnl > (t.buy_sol || 0.005) * 0.02 ? 1 : 0;
  const row = [
    label,
    t.feat_p_cont ?? 0.5,
    t.feat_bcd_score ?? 0.5,
    t.feat_manip_score ?? 0.5,
    t.feat_creator_prior ?? 0.5,
    t.feat_velocity ?? 0,
    t.feat_breadth_score ?? 0.5,
    t.feat_unique_buyers ?? 0,
    t.feat_mcap_sol ?? 0,
    t.active_stop_pct ?? 0.15,
    t.active_target_pct ?? 0.20,
    t.active_max_hold_s ?? 12,
  ];
  rows.push(row.join(','));
  labeled++;
}

fs.writeFileSync(OUT_PATH, rows.join('\n'));
console.log(`Exported ${labeled} labeled trades to ${OUT_PATH}`);
console.log(`Positives: ${rows.filter(r=>r.startsWith('1,')).length} | Negatives: ${rows.filter(r=>r.startsWith('0,')).length}`);
