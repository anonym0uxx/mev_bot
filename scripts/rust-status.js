#!/usr/bin/env node
/**
 * rust-status.js — Rust daemon status report for heartbeat
 * Reads from:
 *   - Rust API at :9421 (stats, health)
 *   - data/momentum_paper_trades.jsonl (momentum trades — primary engine)
 *   - data/backrun_paper_trades.jsonl (backrun trades — disabled, historical only)
 *   - data/engine-state.json (session boundary)
 *   - data/heartbeat-trade-state.json (state tracking)
 */

const fs = require('fs');
const path = require('path');
const http = require('http');

const BASE = path.join(__dirname, '..');
const MOMENTUM_JSONL = path.join(BASE, 'data/momentum_paper_trades.jsonl');
const BACKRUN_JSONL  = path.join(BASE, 'data/backrun_paper_trades.jsonl');
const STATE_PATH = path.join(BASE, 'data/engine-state.json');
const HB_STATE   = path.join(BASE, 'data/heartbeat-trade-state.json');

// Mode determined from API after fetch — defaults updated below
let isPaper = true;
let modeFlag = '📄 PAPER';

// ── Helpers ──────────────────────────────────────────────────────────────────

function fetchJson(port, urlPath) {
  return new Promise((resolve) => {
    const req = http.get({ host: '127.0.0.1', port, path: urlPath, timeout: 2000 }, (res) => {
      let data = '';
      res.on('data', d => data += d);
      res.on('end', () => {
        try { resolve(JSON.parse(data)); } catch { resolve(null); }
      });
    });
    req.on('error', () => resolve(null));
    req.on('timeout', () => { req.destroy(); resolve(null); });
  });
}

function loadJsonl(filePath) {
  try {
    return fs.readFileSync(filePath, 'utf8').trim().split('\n')
      .filter(Boolean).map(l => { try { return JSON.parse(l); } catch { return null; } })
      .filter(Boolean);
  } catch { return []; }
}

function loadJson(filePath) {
  try { return JSON.parse(fs.readFileSync(filePath, 'utf8')); } catch { return {}; }
}

function saveJson(filePath, obj) {
  try { fs.writeFileSync(filePath, JSON.stringify(obj, null, 2)); } catch {}
}

function sol(n) { return (n || 0).toFixed(4); }
function pct(n) { return ((n || 0) * 100).toFixed(1) + '%'; }

// ── Momentum P&L ────────────────────────────────────────────────────────────

function momentumStats(trades) {
  const n = trades.length;
  if (n === 0) return { n: 0, wins: 0, wr: 0, gross: 0, net: 0, fees: 0, byExit: {}, avgHoldMs: 0 };
  const wins = trades.filter(t => (t.net_pnl_sol || 0) > 0).length;
  const wr = wins / n;
  const gross = trades.reduce((s, t) => s + (t.gross_pnl_sol || 0), 0);
  const net = trades.reduce((s, t) => s + (t.net_pnl_sol || 0), 0);
  const fees = trades.reduce((s, t) => s + (t.fee_sol || 0), 0);
  const totalHold = trades.reduce((s, t) => s + (t.hold_ms || 0), 0);
  const avgHoldMs = totalHold / n;
  const byExit = {};
  trades.forEach(t => {
    const r = t.exit_reason || 'unknown';
    byExit[r] = (byExit[r] || 0) + 1;
  });
  return { n, wins, wr, gross, net, fees, byExit, avgHoldMs };
}

// ── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  const engineState = loadJson(STATE_PATH);
  const hbState = loadJson(HB_STATE);
  const sessionStartMs = engineState.daemonStartedAt || (Date.now() - 24 * 3600 * 1000);

  // Fetch Rust API
  const [statsResp, healthResp, submissionResp] = await Promise.all([
    fetchJson(9421, '/api/stats'),
    fetchJson(9421, '/api/health'),
    fetchJson(9421, '/api/metrics/submission'),
  ]);

  const stats  = statsResp?.data || null;
  const health = healthResp?.data || null;
  const alive  = !!stats;

  if (!alive) {
    console.log('❌ RUST DAEMON DOWN — port 9421 not responding');
    process.exit(1);
  }

  // Detect mode from config — canary.json momentum.paper_mode is authoritative.
  // PAPER_MODE env var in rust/.env is NOT used by the momentum engine (it reads canary.json).
  try {
    const cfg = JSON.parse(fs.readFileSync(path.join(BASE, 'config/canary.json'), 'utf8'));
    const momPaper = cfg?.momentum?.paper_mode;
    if (momPaper === false) { isPaper = false; modeFlag = '🔴 LIVE'; }
    else if (momPaper === true) { isPaper = true; modeFlag = '📄 PAPER'; }
  } catch {}

  // ── Load momentum trades (primary engine) ─────────────────────────
  const allMomentum = loadJsonl(MOMENTUM_JSONL);
  // Filter to Rust engine PumpSwap trades only (pool_type "pump_swap" or 1).
  // Excludes old TS daemon / Raydium trades that are no longer relevant.
  // Also: require size_sol field, filter phantom PnL, require non-zero price samples.
  const cleanMomentum = allMomentum.filter(t =>
    t.size_sol != null && t.size_sol > 0 &&
    (t.pool_type === 'pump_swap' || t.pool_type === 1) &&
    Math.abs(t.net_pnl_sol || 0) <= t.size_sol * 10 &&
    Array.isArray(t.price_samples_bps) && t.price_samples_bps.some(s => s !== 0)
  );
  const sessionMomentum = cleanMomentum.filter(t => (t.exit_timestamp_ms || 0) >= sessionStartMs);

  // Track new trades since last heartbeat
  const lastMomCount = hbState.last_momentum_count || 0;
  const newMomTrades = cleanMomentum.length - lastMomCount;

  const ses = momentumStats(sessionMomentum);
  const all = momentumStats(cleanMomentum);

  // High-water mark
  const prevHigh = hbState.momentum_pnl_high_water || -Infinity;
  const newHigh = all.net > prevHigh;

  // ── Feed health ───────────────────────────────────────────────────
  const feeds = health?.feeds || {};
  const ppOk  = feeds.pumpportal?.status === 'healthy';
  const helOk = feeds.helius?.status === 'healthy' || feeds.helius?.status === 'not_started';
  const ccOk  = feeds.corecast?.status === 'healthy' || feeds.corecast?.status === 'not_started';
  const ppAge = feeds.pumpportal?.age_s ?? '?';
  const helAge = feeds.helius?.age_s ?? '?';
  const ccAge = feeds.corecast?.age_s ?? '?';
  const paused = stats?.paused || health?.trading_paused || false;

  const uptime = stats?.uptime_s || 1;
  const tps = (stats?.trades_seen || 0) / uptime;
  const uptimeMin = Math.floor(uptime / 60);
  const uptimeStr = uptimeMin >= 60
    ? `${Math.floor(uptimeMin/60)}h${uptimeMin%60}m`
    : `${uptimeMin}m`;

  // ── Build report ──────────────────────────────────────────────────
  let lines = [];
  lines.push(`⚙️ Engine v5-rust | ${modeFlag} | Up ${uptimeStr}`);
  lines.push('');

  // ── Momentum (primary engine) ─────────────────────────────────────
  if (ses.n === 0) {
    lines.push('🚀 Momentum session: no trades yet');
  } else {
    lines.push(`🚀 Momentum session (${ses.n} trades):`);
    lines.push(`  WR ${pct(ses.wr)} | Gross ${ses.gross >= 0 ? '+' : ''}${sol(ses.gross)} | Net ${ses.net >= 0 ? '+' : ''}${sol(ses.net)} SOL`);
    const exitStr = Object.entries(ses.byExit)
      .sort(([,a],[,b]) => b - a)
      .map(([r, c]) => `${r}=${c}`)
      .join(' ');
    lines.push(`  ${exitStr}`);
    lines.push(`  Avg hold: ${(ses.avgHoldMs / 1000).toFixed(1)}s | Fees: ${sol(ses.fees)} SOL`);
    if (ses.wins > 0) {
      const avgWin = sessionMomentum.filter(t => t.net_pnl_sol > 0).reduce((s, t) => s + t.net_pnl_sol, 0) / ses.wins;
      const avgLoss = ses.n - ses.wins > 0
        ? sessionMomentum.filter(t => t.net_pnl_sol <= 0).reduce((s, t) => s + t.net_pnl_sol, 0) / (ses.n - ses.wins)
        : 0;
      lines.push(`  Avg win: ${sol(avgWin)} | Avg loss: ${sol(avgLoss)} SOL`);
    }
  }

  if (newMomTrades > 0) {
    lines.push(`  📈 +${newMomTrades} trades since last heartbeat`);
  }

  // ── Momentum all-time (clean build only) ──────────────────────────
  lines.push('');
  if (all.n > 0) {
    lines.push(`📊 Momentum overall (clean build): ${all.n} trades`);
    lines.push(`  WR ${pct(all.wr)} | Net ${all.net >= 0 ? '+' : ''}${sol(all.net)} SOL`);
    if (all.wins > 0) {
      const bestWin = Math.max(...cleanMomentum.map(t => t.net_pnl_sol || 0));
      const worstLoss = Math.min(...cleanMomentum.map(t => t.net_pnl_sol || 0));
      lines.push(`  Best: ${sol(bestWin)} | Worst: ${sol(worstLoss)} SOL`);
    }
  } else {
    lines.push('📊 Momentum overall: no clean-build trades yet');
  }

  // ── Feeds ─────────────────────────────────────────────────────────
  lines.push('');
  lines.push('📡 Feeds');
  lines.push(`  PumpPortal: ${ppOk ? '✅' : '❌'} (${ppAge}s) | Helius: ${helOk ? '✅' : '❌'} (${helAge}s) | CoreCast: ${ccOk ? '✅' : '❌'} (${ccAge}s)`);
  lines.push(`  Throughput: ${tps.toFixed(1)} evt/s | Migrations: ${stats?.migrations_seen ?? 0}`);

  if (paused) lines.push('  ⚠️ TRADING PAUSED');

  // ── TX Submission ─────────────────────────────────────────────────
  const sub = submissionResp; // direct response (no .data wrapper assumed)
  lines.push('');
  if (!sub) {
    lines.push('📡 TX Submission: not available');
  } else {
    const totalAttempts = (sub.rpc_attempts || 0) + (sub.jito_fallback_attempts || 0);
    const totalLanded = (sub.rpc_landed || 0) + (sub.jito_fallback_landed || 0);
    if (totalAttempts === 0) {
      lines.push('📡 TX Submission: no trades yet');
    } else {
      const overallRate = totalAttempts > 0 ? totalLanded / totalAttempts : 0;
      const rpcRate = sub.rpc_attempts > 0 ? sub.rpc_landed / sub.rpc_attempts : 0;
      const jitoRate = sub.jito_fallback_attempts > 0 ? sub.jito_fallback_landed / sub.jito_fallback_attempts : 0;
      const avgLatency = Math.round(sub.avg_confirm_latency_ms || 0);
      const costPerTx = sub.cost_per_landed_tx_lamports || 0;
      const totalCostLam = (sub.total_priority_fees_lamports || 0) + (sub.total_jito_tips_lamports || 0);
      const totalCostSol = totalCostLam / 1e9;

      // Format cost numbers: use K for thousands, M for millions
      const fmtLam = (v) => {
        if (v >= 1e6) return `${(v / 1e6).toFixed(2)}M`;
        if (v >= 1e3) return `${(v / 1e3).toFixed(0)}K`;
        return `${v}`;
      };

      lines.push(`📡 TX Submission`);
      lines.push(`  Mode: RPC Primary (circuit: ${sub.circuit_state || 'unknown'})`);
      lines.push(`  RPC: ${sub.rpc_landed}/${sub.rpc_attempts} landed (${(rpcRate * 100).toFixed(1)}%) | avg ${avgLatency}ms`);
      if (sub.jito_fallback_attempts > 0) {
        lines.push(`  Jito fallback: ${sub.jito_fallback_landed}/${sub.jito_fallback_attempts} landed (${(jitoRate * 100).toFixed(1)}%)`);
      }
      lines.push(`  Overall: ${totalLanded}/${totalAttempts} landed (${(overallRate * 100).toFixed(1)}%)`);
      lines.push(`  Cost: ${fmtLam(costPerTx)} lam/landed TX | Total: ${fmtLam(totalCostLam)} lam (${totalCostSol.toFixed(5)} SOL)`);
      lines.push(`  Consecutive fails: ${sub.consecutive_failures || 0}`);
    }
  }

  if (newHigh && all.net > 0) lines.push(`\n🏆 NEW HIGH WATER: ${sol(all.net)} SOL net!`);

  // ── Alerts ────────────────────────────────────────────────────────
  const alerts = [];
  if (!ppOk) alerts.push('🔴 PumpPortal feed down');
  if (!helOk) alerts.push('🔴 Helius feed down');
  if (!ccOk) alerts.push('🔴 CoreCast feed down');
  if (ses.n >= 10 && ses.wr < 0.30) alerts.push(`⚠️ WR critical: ${pct(ses.wr)} on ${ses.n} trades`);
  if (ses.net < -0.30) alerts.push(`⚠️ Session PnL: ${sol(ses.net)} SOL`);
  if (all.net < -2.0) alerts.push(`⚠️ Overall PnL: ${sol(all.net)} SOL`);

  // TX submission alerts
  if (sub) {
    const totalSubAttempts = (sub.rpc_attempts || 0) + (sub.jito_fallback_attempts || 0);
    if ((sub.consecutive_failures || 0) >= 3) {
      alerts.push(`🔴 TX submission: ${sub.consecutive_failures} consecutive failures`);
    }
    if (totalSubAttempts >= 10 && (sub.inclusion_rate || 0) < 0.5) {
      alerts.push(`⚠️ TX inclusion rate critical: ${((sub.inclusion_rate || 0) * 100).toFixed(1)}% on ${totalSubAttempts} attempts`);
    }
    if (sub.circuit_state === 'open') {
      alerts.push('🔴 TX circuit breaker OPEN — RPC degraded, Jito-only');
    }
  }

  if (alerts.length > 0) {
    lines.push('');
    lines.push('🚨 Alerts:');
    alerts.forEach(a => lines.push('  ' + a));
  }

  console.log(lines.join('\n'));

  // Save state (including TX submission metadata for programmatic checks)
  const stateObj = {
    last_momentum_count: cleanMomentum.length,
    last_trade_count: cleanMomentum.length,  // backward compat
    last_check_ts: Date.now(),
    last_win_rate: all.wr,
    momentum_pnl_high_water: Math.max(all.net, prevHigh),
    new_trades_this_hb: newMomTrades,
  };
  if (sub) {
    const totalSubAttempts = (sub.rpc_attempts || 0) + (sub.jito_fallback_attempts || 0);
    stateObj.tx_inclusion_rate = totalSubAttempts > 0
      ? ((sub.rpc_landed || 0) + (sub.jito_fallback_landed || 0)) / totalSubAttempts
      : null;
    stateObj.circuit_state = sub.circuit_state || 'unknown';
    stateObj.tx_submission_alert =
      (sub.consecutive_failures || 0) >= 3 ||
      (totalSubAttempts >= 10 && (sub.inclusion_rate || 0) < 0.5) ||
      sub.circuit_state === 'open';
  }
  saveJson(HB_STATE, stateObj);

  if (alerts.some(a => a.startsWith('🔴'))) process.exit(2);
}

main().catch(e => { console.error('rust-status error:', e.message); process.exit(1); });
