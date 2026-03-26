#!/usr/bin/env node
/**
 * Loss analyzer — called by monitor or heartbeat.
 * Reads closed positions from DB, computes per-exit-reason stats,
 * and writes a structured JSON report to data/loss-analysis.json
 *
 * BUG FIX: Deduplicates orders by tx_signature before computing PnL.
 * If tx_signature is null/empty, falls back to grouping by
 * (mint, side, confirmed_at) within a 500ms window.
 */
const Database = require('better-sqlite3');
const fs = require('fs');
const path = require('path');

const DB_PATH = path.join(__dirname, '../data/pump-quant.db');
const OUT_PATH = path.join(__dirname, '../data/loss-analysis.json');

const db = new Database(DB_PATH, { readonly: true });

// --- Deduplicate orders by tx_signature ---
// Keep the entry with the HIGHEST realized_sol per tx_signature.
// Partial fills arrive first with lower realized_sol; the final confirmed fill has the
// actual on-chain amount. Keeping the max gives the correct PnL.
function deduplicateOrders(orders) {
  const seenSigs = new Map();      // tx_signature → order (keep highest realized_sol)
  const noSigGroups = new Map();   // "mint|side|bucket" → order (keep highest realized_sol, bucket = floor(confirmed_at/500)*500)

  for (const o of orders) {
    const sig = o.tx_signature;
    if (sig && sig.trim() !== '') {
      const existing = seenSigs.get(sig);
      if (!existing || (o.realized_sol || 0) > (existing.realized_sol || 0)) {
        seenSigs.set(sig, o);
      }
    } else {
      // Fallback: group by (mint, side, 500ms bucket)
      const bucket = Math.floor((o.confirmed_at || 0) / 500) * 500;
      const key = `${o.mint}|${o.side}|${bucket}`;
      const existing = noSigGroups.get(key);
      if (!existing || (o.realized_sol || 0) > (existing.realized_sol || 0)) {
        noSigGroups.set(key, o);
      }
    }
  }

  return [...seenSigs.values(), ...noSigGroups.values()];
}

// --- Build trade records from deduplicated orders ---
const rawOrders = db.prepare(`SELECT * FROM orders WHERE status='confirmed' AND is_paper=0 ORDER BY created_at ASC`).all();
const dedupedOrders = deduplicateOrders(rawOrders);

console.error(`[analyze-losses] Orders: raw=${rawOrders.length} deduped=${dedupedOrders.length} (removed ${rawOrders.length - dedupedOrders.length} duplicates)`);

const byMint = {};
for (const o of dedupedOrders) {
  if (!byMint[o.mint]) byMint[o.mint] = [];
  byMint[o.mint].push(o);
}

// Convert to position-like objects
const positions = [];
for (const [mint, txs] of Object.entries(byMint)) {
  const buys = txs.filter(t => t.side === 'buy');
  const sells = txs.filter(t => t.side === 'sell');
  if (buys.length === 0) continue;
  const buySOL = buys.reduce((s, t) => s + (t.realized_sol || 0), 0);
  const sellSOL = sells.reduce((s, t) => s + (t.realized_sol || 0), 0);
  const fees = txs.reduce((s, t) => s + (t.fee_sol || 0) + (t.priority_fee_paid_sol || 0), 0);
  const pnl = sellSOL - buySOL - fees;
  const isClosed = sells.length > 0;
  if (!isClosed) continue;
  const openedAt = buys[0].confirmed_at;
  const closedAt = sells[sells.length - 1].confirmed_at;
  positions.push({
    mint,
    realized_pnl_sol: pnl,
    buy_sol: buySOL,
    sell_sol: sellSOL,
    fees_sol: fees,
    exit_reason: 'order',
    opened_at: openedAt,
    closed_at: closedAt,
    max_favorable_excursion_pct: null,
    max_adverse_excursion_pct: null,
    buy_count: buys.length,
    sell_count: sells.length,
  });
}

// --- Pull recent open positions ---
const open = db.prepare(`SELECT * FROM positions WHERE status='OPEN'`).all();

// --- Aggregate stats ---
const total = positions.length;
const wins = positions.filter(p => (p.realized_pnl_sol || 0) > 0);
const losses = positions.filter(p => (p.realized_pnl_sol || 0) <= 0);
const totalPnl = positions.reduce((s, p) => s + (p.realized_pnl_sol || 0), 0);
const avgPnl = total > 0 ? totalPnl / total : 0;

// Group by exit reason
const byExitReason = {};
for (const p of positions) {
  const reason = p.exit_reason || 'unknown';
  if (!byExitReason[reason]) byExitReason[reason] = { count: 0, pnl: 0, wins: 0 };
  byExitReason[reason].count++;
  byExitReason[reason].pnl += p.realized_pnl_sol || 0;
  if ((p.realized_pnl_sol || 0) > 0) byExitReason[reason].wins++;
}

// Hold time analysis
const holdTimes = positions
  .filter(p => p.opened_at && p.closed_at)
  .map(p => (p.closed_at - p.opened_at) / 1000);
const avgHold = holdTimes.length > 0 ? holdTimes.reduce((a, b) => a + b, 0) / holdTimes.length : 0;

// MFE/MAE analysis (from feature snapshots if available)
const mfeData = positions
  .filter(p => p.max_favorable_excursion_pct != null)
  .map(p => ({
    mint: p.mint,
    mfe: p.max_favorable_excursion_pct,
    mae: p.max_adverse_excursion_pct,
    pnl: p.realized_pnl_sol,
    exit: p.exit_reason,
  }));

const zeroMfe = mfeData.filter(p => p.mfe <= 0).length;
const doaRate = mfeData.length > 0 ? (zeroMfe / mfeData.length * 100).toFixed(1) : 'n/a';

// Recent 24h PnL
const since24h = Date.now() - 24 * 60 * 60 * 1000;
const recent = positions.filter(p => (p.closed_at || 0) > since24h);
const recent24hPnl = recent.reduce((s, p) => s + (p.realized_pnl_sol || 0), 0);

// --- Build report ---
const report = {
  generated_at: new Date().toISOString(),
  dedup_stats: {
    raw_orders: rawOrders.length,
    deduped_orders: dedupedOrders.length,
    duplicates_removed: rawOrders.length - dedupedOrders.length,
  },
  summary: {
    total_trades: total,
    open_positions: open.length,
    wins: wins.length,
    losses: losses.length,
    win_rate_pct: total > 0 ? (wins.length / total * 100).toFixed(1) : 'n/a',
    total_pnl_sol: totalPnl.toFixed(5),
    avg_pnl_per_trade_sol: avgPnl.toFixed(5),
    avg_hold_time_s: avgHold.toFixed(1),
    doa_rate_pct: doaRate,
    pnl_24h_sol: recent24hPnl.toFixed(5),
  },
  by_exit_reason: Object.fromEntries(
    Object.entries(byExitReason).map(([r, v]) => [r, {
      count: v.count,
      wins: v.wins,
      win_rate: v.count > 0 ? (v.wins / v.count * 100).toFixed(1) + '%' : '0%',
      total_pnl_sol: v.pnl.toFixed(5),
    }])
  ),
  worst_trades: losses
    .sort((a, b) => (a.realized_pnl_sol || 0) - (b.realized_pnl_sol || 0))
    .slice(0, 5)
    .map(p => ({
      mint: p.mint,
      pnl_sol: (p.realized_pnl_sol || 0).toFixed(5),
      buy_sol: p.buy_sol.toFixed(5),
      sell_sol: p.sell_sol.toFixed(5),
      fees_sol: p.fees_sol.toFixed(5),
      exit_reason: p.exit_reason,
      hold_s: p.opened_at && p.closed_at ? Math.round((p.closed_at - p.opened_at) / 1000) : null,
    })),
  best_trades: wins
    .sort((a, b) => (b.realized_pnl_sol || 0) - (a.realized_pnl_sol || 0))
    .slice(0, 5)
    .map(p => ({
      mint: p.mint,
      pnl_sol: (p.realized_pnl_sol || 0).toFixed(5),
      buy_sol: p.buy_sol.toFixed(5),
      sell_sol: p.sell_sol.toFixed(5),
      fees_sol: p.fees_sol.toFixed(5),
      exit_reason: p.exit_reason,
      hold_s: p.opened_at && p.closed_at ? Math.round((p.closed_at - p.opened_at) / 1000) : null,
    })),
  mfe_analysis: {
    sample_size: mfeData.length,
    zero_mfe_count: zeroMfe,
    zero_mfe_rate_pct: doaRate,
    avg_mfe_pct: mfeData.length > 0
      ? (mfeData.reduce((s, p) => s + p.mfe, 0) / mfeData.length).toFixed(2)
      : 'n/a',
    avg_mae_pct: mfeData.length > 0
      ? (mfeData.reduce((s, p) => s + (p.mae || 0), 0) / mfeData.length).toFixed(2)
      : 'n/a',
  },
};

fs.writeFileSync(OUT_PATH, JSON.stringify(report, null, 2));
console.log(JSON.stringify(report.summary));
