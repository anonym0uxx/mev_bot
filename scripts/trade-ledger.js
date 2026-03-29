#!/usr/bin/env node
/**
 * trade-ledger.js — Historical P&L ledger across all paper and real trades.
 * Pulls from positions table (scalper) + backrun_paper_trades.jsonl (MEV paper)
 * and produces a full breakdown by mode, engine, day, and regime.
 *
 * Usage:
 *   node scripts/trade-ledger.js              # full report
 *   node scripts/trade-ledger.js --json       # raw JSON output
 *   node scripts/trade-ledger.js --today      # today only
 *   node scripts/trade-ledger.js --days 7     # last N days
 */

const Database = require('better-sqlite3');
const fs = require('fs');
const path = require('path');

const DB_PATH = path.join(__dirname, '../data/pump-quant.db');
const MEV_LOG = path.join(__dirname, '../data/backrun_paper_trades.jsonl');

const args = process.argv.slice(2);
const jsonMode = args.includes('--json');
const todayMode = args.includes('--today');
const daysIdx = args.indexOf('--days');
const daysWindow = daysIdx !== -1 ? parseInt(args[daysIdx + 1]) : null;

const db = new Database(DB_PATH, { readonly: true });

// ── Time window ──────────────────────────────────────────────────────────────
const now = Date.now();
let windowStart = 0;
if (todayMode) {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  windowStart = d.getTime();
} else if (daysWindow) {
  windowStart = now - daysWindow * 24 * 60 * 60 * 1000;
}

const windowLabel = todayMode ? 'today' : daysWindow ? `last ${daysWindow}d` : 'all time';

// ── Helpers ───────────────────────────────────────────────────────────────────
const fmt = (n) => (n >= 0 ? '+' : '') + n.toFixed(4);
const pct = (w, t) => t > 0 ? (w / t * 100).toFixed(1) + '%' : '—';
const dayKey = (ts) => new Date(ts).toISOString().slice(0, 10);

function summarise(trades) {
  const total = trades.length;
  const wins = trades.filter(t => t.pnl > 0).length;
  const pnl = trades.reduce((s, t) => s + t.pnl, 0);
  const fees = trades.reduce((s, t) => s + (t.fees || 0), 0);
  const avgHold = total > 0 ? trades.reduce((s, t) => s + (t.holdS || 0), 0) / total : 0;
  return { total, wins, losses: total - wins, wr: pct(wins, total), pnl, fees, avgHoldS: avgHold };
}

// ── Scalper trades (positions table) ─────────────────────────────────────────
const scalperRows = db.prepare(`
  SELECT
    is_paper, regime, status,
    realized_pnl_sol as pnl,
    total_fees_sol as fees,
    hold_duration_s as holdS,
    opened_at, closed_at,
    exit_reason
  FROM positions
  WHERE status = 'closed'
    AND opened_at >= ?
  ORDER BY opened_at ASC
`).all(windowStart);

const scalperPaper = scalperRows.filter(r => r.is_paper === 1).map(r => ({
  engine: 'scalper', mode: 'paper', day: dayKey(r.opened_at),
  pnl: r.pnl || 0, fees: r.fees || 0, holdS: r.holdS || 0,
  regime: r.regime, exitReason: r.exit_reason, ts: r.opened_at,
}));

const scalperReal = scalperRows.filter(r => r.is_paper === 0).map(r => ({
  engine: 'scalper', mode: 'real', day: dayKey(r.opened_at),
  pnl: r.pnl || 0, fees: r.fees || 0, holdS: r.holdS || 0,
  regime: r.regime, exitReason: r.exit_reason, ts: r.opened_at,
}));

// ── MEV trades (JSONL — paper only for now) ───────────────────────────────────
const mevPaper = [];
try {
  const lines = fs.readFileSync(MEV_LOG, 'utf8').trim().split('\n').filter(Boolean);
  for (const line of lines) {
    const r = JSON.parse(line);
    const ts = r.exitTimestampMs || r.entryTimestampMs || r.recordedAt || 0;
    if (ts < windowStart) continue;
    mevPaper.push({
      engine: 'mev', mode: 'paper', day: dayKey(ts),
      pnl: r.pnlSol || 0, fees: 0, holdS: (r.holdMs || 0) / 1000,
      regime: null, exitReason: r.exitReason, ts,
      curvePct: r.curvePct, triggerBuySol: r.triggerBuySol, score: r.score,
    });
  }
} catch (_) {}

// ── Combine all ───────────────────────────────────────────────────────────────
const allTrades = [...scalperPaper, ...scalperReal, ...mevPaper];

// ── Top-level summary ─────────────────────────────────────────────────────────
const summary = {
  window: windowLabel,
  generated_at: new Date().toISOString(),
  paper: {
    scalper: summarise(scalperPaper),
    mev: summarise(mevPaper),
    combined: summarise([...scalperPaper, ...mevPaper]),
  },
  real: {
    scalper: summarise(scalperReal),
    mev: summarise([]), // placeholder — MEV real not yet live
    combined: summarise(scalperReal),
  },
  overall: summarise(allTrades),
};

// ── Daily breakdown ───────────────────────────────────────────────────────────
const byDay = {};
for (const t of allTrades) {
  const key = `${t.day}|${t.mode}|${t.engine}`;
  if (!byDay[key]) byDay[key] = { day: t.day, mode: t.mode, engine: t.engine, trades: [] };
  byDay[key].trades.push(t);
}
const daily = Object.values(byDay)
  .sort((a, b) => a.day.localeCompare(b.day) || a.mode.localeCompare(b.mode) || a.engine.localeCompare(b.engine))
  .map(({ day, mode, engine, trades }) => ({ day, mode, engine, ...summarise(trades) }));

// ── Exit reason breakdown ─────────────────────────────────────────────────────
const exitBreakdown = {};
for (const t of allTrades) {
  const key = `${t.mode}|${t.engine}|${t.exitReason || 'unknown'}`;
  if (!exitBreakdown[key]) exitBreakdown[key] = { mode: t.mode, engine: t.engine, reason: t.exitReason, count: 0, pnl: 0 };
  exitBreakdown[key].count++;
  exitBreakdown[key].pnl += t.pnl;
}
const exits = Object.values(exitBreakdown).sort((a, b) => b.count - a.count);

// ── Regime breakdown (scalper only) ──────────────────────────────────────────
const byRegime = {};
for (const t of [...scalperPaper, ...scalperReal]) {
  const key = `${t.mode}|${t.regime || 'unknown'}`;
  if (!byRegime[key]) byRegime[key] = { mode: t.mode, regime: t.regime, trades: [] };
  byRegime[key].trades.push(t);
}
const regimes = Object.values(byRegime)
  .sort((a, b) => a.mode.localeCompare(b.mode) || (a.regime || '').localeCompare(b.regime || ''))
  .map(({ mode, regime, trades }) => ({ mode, regime, ...summarise(trades) }));

// ── Output ────────────────────────────────────────────────────────────────────
if (jsonMode) {
  console.log(JSON.stringify({ summary, daily, exits, regimes }, null, 2));
  process.exit(0);
}

// Human-readable report
const hr = '─'.repeat(56);
const line = (label, s) =>
  `  ${label.padEnd(22)} ${String(s.total).padStart(5)} trades | WR ${(s.wr).padStart(6)} | ${fmt(s.pnl).padStart(10)} SOL`;

console.log(`\n📊 PUMP-QUANT TRADE LEDGER — ${windowLabel.toUpperCase()}`);
console.log(hr);

console.log('\n📄 PAPER');
console.log(line('Scalper', summary.paper.scalper));
console.log(line('MEV Backrun', summary.paper.mev));
console.log(line('TOTAL PAPER', summary.paper.combined));

console.log('\n💵 REAL');
console.log(line('Scalper', summary.real.scalper));
console.log(line('MEV Backrun', summary.real.mev));
console.log(line('TOTAL REAL', summary.real.combined));

console.log('\n💰 OVERALL');
console.log(line('All engines', summary.overall));

console.log(`\n${hr}`);
console.log('\n📅 DAILY BREAKDOWN');
let lastDay = '';
for (const d of daily) {
  if (d.day !== lastDay) { console.log(`\n  ${d.day}`); lastDay = d.day; }
  const tag = d.mode === 'paper' ? '📄' : '💵';
  console.log(`    ${tag} ${d.engine.padEnd(8)} ${String(d.total).padStart(4)} trades | WR ${d.wr.padStart(6)} | ${fmt(d.pnl).padStart(10)} SOL`);
}

console.log(`\n${hr}`);
console.log('\n🚪 EXIT REASONS');
const exitGroups = {};
for (const e of exits) {
  const k = `${e.mode}|${e.engine}`;
  if (!exitGroups[k]) exitGroups[k] = [];
  exitGroups[k].push(e);
}
for (const [key, group] of Object.entries(exitGroups)) {
  const [mode, engine] = key.split('|');
  const tag = mode === 'paper' ? '📄' : '💵';
  console.log(`\n  ${tag} ${engine}`);
  for (const e of group.slice(0, 6)) {
    console.log(`    ${(e.reason || 'unknown').padEnd(38)} ${String(e.count).padStart(4)}x | ${fmt(e.pnl).padStart(10)} SOL`);
  }
}

console.log(`\n${hr}`);
console.log('\n🔀 SCALPER REGIME BREAKDOWN');
for (const r of regimes) {
  const tag = r.mode === 'paper' ? '📄' : '💵';
  console.log(`  ${tag} ${(r.regime || 'unknown').padEnd(18)} ${String(r.total).padStart(5)} trades | WR ${r.wr.padStart(6)} | ${fmt(r.pnl).padStart(10)} SOL`);
}

console.log(`\n${hr}`);
console.log(`  Generated: ${new Date().toLocaleString('en-US', { timeZone: 'America/Los_Angeles' })} PT\n`);

db.close();
