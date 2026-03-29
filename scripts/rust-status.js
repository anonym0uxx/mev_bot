#!/usr/bin/env node
/**
 * rust-status.js — Rust MEV daemon status report for heartbeat
 * Reads from:
 *   - Rust API at :9421 (stats, health, latency)
 *   - data/mev_paper_trades.jsonl (trade history)
 *   - data/engine-state.json (session boundary)
 *   - data/heartbeat-trade-state.json (state tracking)
 */

const fs = require('fs');
const path = require('path');
const http = require('http');

const BASE = path.join(__dirname, '..');
const JSONL_PATH = path.join(BASE, 'data/mev_paper_trades.jsonl');
const STATE_PATH = path.join(BASE, 'data/engine-state.json');
const HB_STATE  = path.join(BASE, 'data/heartbeat-trade-state.json');

const isPaper = process.env.PAPER_MODE !== 'false';
const modeFlag = isPaper ? '📄 PAPER' : '🔴 LIVE';

// ── Helpers ──────────────────────────────────────────────────────────────────

function fetchJson(port, path) {
  return new Promise((resolve) => {
    const req = http.get({ host: '127.0.0.1', port, path, timeout: 2000 }, (res) => {
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

// ── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  // Load state files
  const engineState = loadJson(STATE_PATH);
  const hbState = loadJson(HB_STATE);
  const sessionStartMs = engineState.daemonStartedAt || (Date.now() - 24 * 3600 * 1000);

  // Fetch Rust API
  const [statsResp, healthResp] = await Promise.all([
    fetchJson(9421, '/api/stats'),
    fetchJson(9421, '/api/health'),
  ]);

  const stats  = statsResp?.data || null;
  const health = healthResp?.data || null;
  const alive  = !!stats;

  // ── Daemon alive check ────────────────────────────────────────────
  if (!alive) {
    console.log('❌ RUST DAEMON DOWN — port 9421 not responding');
    console.log('Restart: cd /data/.openclaw/workspace/projects/pump-quant && source rust/.env && PAPER_MODE=true RUST_LOG=info nohup ./rust/target/release/pump-quant > logs/rust-daemon.log 2>&1 &');
    process.exit(1);
  }

  // ── Trade history ─────────────────────────────────────────────────
  const allTrades = loadJsonl(JSONL_PATH);
  const sessionTrades = allTrades.filter(t => (t.exitTimestampMs || t.recordedAt || 0) >= sessionStartMs);
  const lastHbCount = hbState.last_trade_count || 0;
  const newTrades = allTrades.length - lastHbCount;

  // P&L calculations
  function pnlStats(trades) {
    const n = trades.length;
    const wins = trades.filter(t => (t.pnlSol || 0) > 0).length;
    const wr = n > 0 ? wins / n : 0;
    const gross = trades.reduce((s, t) => s + (t.pnlSol || 0), 0);
    const net   = trades.reduce((s, t) => s + (t.netPnlSol ?? t.pnlSol ?? 0), 0);
    const fees  = trades.reduce((s, t) => s + (t.feesSol || 0), 0);
    const byExit = { take_profit: 0, next_buyer: 0, stop_loss: 0, max_hold: 0, momentum_decay_flat: 0, momentum_decay_fade: 0 };
    trades.forEach(t => { const r = t.exitReason || 'unknown'; byExit[r] = (byExit[r] || 0) + 1; });
    return { n, wins, wr, gross, net, fees, byExit };
  }

  const ses = pnlStats(sessionTrades);
  const all = pnlStats(allTrades);

  // High-water mark tracking
  const prevHigh = hbState.mev_pnl_high_water || -Infinity;
  const newHigh  = all.net > prevHigh;

  // ── Feed health ───────────────────────────────────────────────────
  const feeds = health?.feeds || {};
  const ppAge  = feeds.pumpportal?.age_s ?? '?';
  const helAge = feeds.helius?.age_s ?? '?';
  const ppOk   = feeds.pumpportal?.status === 'healthy';
  const helOk  = feeds.helius?.status === 'healthy' || feeds.helius?.status === 'not_started';
  const paused = stats?.paused || health?.trading_paused || false;

  // ── Latency stats ─────────────────────────────────────────────────
  // Derived from API stats (uptime vs trades_seen = throughput proxy)
  const uptime   = stats?.uptime_s || 1;
  const tps      = (stats?.trades_seen || 0) / uptime;
  // Gate pass rate
  const gatePassRate = stats?.trades_seen > 0
    ? ((stats.gates_passed || 0) / stats.trades_seen * 100).toFixed(1)
    : '0.0';

  // ── Build report ──────────────────────────────────────────────────
  const uptimeMin = Math.floor(uptime / 60);
  const uptimeStr = uptimeMin >= 60
    ? `${Math.floor(uptimeMin/60)}h${uptimeMin%60}m`
    : `${uptimeMin}m`;

  let lines = [];
  lines.push(`⚙️ Engine v5-rust | ${modeFlag} | Up ${uptimeStr}`);
  lines.push('');

  // Session block
  if (ses.n === 0) {
    lines.push('Session: no trades yet');
  } else {
    lines.push(`Session (${ses.n} trades):`);
    lines.push(`  WR ${pct(ses.wr)} | Gross ${ses.gross >= 0 ? '+' : ''}${sol(ses.gross)} | Net ${ses.net >= 0 ? '+' : ''}${sol(ses.net)} SOL`);
    lines.push(`  tp=${ses.byExit.take_profit||0} nb=${ses.byExit.next_buyer||0} sl=${ses.byExit.stop_loss||0} mh=${ses.byExit.max_hold||0} md=${(ses.byExit.momentum_decay_flat||0)+(ses.byExit.momentum_decay_fade||0)}`);
    if (ses.fees > 0) lines.push(`  Fees: ${sol(ses.fees)} SOL | Fee drag: ${ses.gross > 0 ? (ses.fees/ses.gross*100).toFixed(1)+'%' : 'n/a'}`);
  }

  lines.push('');
  lines.push('📊 Overall P&L');
  lines.push(`🎯 MEV: ${all.n} trades | WR ${pct(all.wr)} | Gross ${all.gross >= 0 ? '+' : ''}${sol(all.gross)} | Net ${all.net >= 0 ? '+' : ''}${sol(all.net)} SOL`);
  if (all.n > 0) {
    const breakeven = all.fees > 0 && all.gross !== 0 ? (all.fees / (all.fees + all.gross) * 100).toFixed(1) : '~66.5';
    lines.push(`   (break-even WR: ~${breakeven}% gross)`);
  }

  lines.push('');
  lines.push('📡 Feeds & Latency');
  lines.push(`  PumpPortal: ${ppOk ? '✅' : '❌'} (${ppAge}s ago)`);
  lines.push(`  Helius:     ${helOk ? '✅' : '❌'} (${helAge}s ago)`);
  lines.push(`  Throughput: ${tps.toFixed(1)} events/s | Gate pass: ${gatePassRate}%`);
  lines.push(`  Positions open: ${stats?.positions_opened - stats?.positions_closed || 0} | Closed: ${stats?.positions_closed || 0}`);

  // Stream event counters (from API or fallback to log parsing)
  const migrations = stats?.migrations_seen;
  const lpRemovals = stats?.lp_removals_seen;
  const newTokens  = stats?.new_tokens_seen;
  const creatorSells = stats?.creator_sells_seen;

  if (migrations != null || lpRemovals != null || newTokens != null || creatorSells != null) {
    lines.push('');
    lines.push('📡 Stream Events (session)');
    lines.push(`  Migrations detected: ${migrations ?? 0}`);
    lines.push(`  LP removals: ${lpRemovals ?? 0}`);
    lines.push(`  New tokens pre-warmed: ${newTokens ?? 0}`);
    lines.push(`  Creator sells: ${creatorSells ?? 0}`);
  } else {
    // Fallback: parse from log file
    const { execSync } = require('child_process');
    try {
      const lastStats = execSync(
        'grep "engine stats" /data/.openclaw/workspace/projects/pump-quant/logs/rust-daemon.log | tail -1',
        { timeout: 2000 }
      ).toString().trim();
      if (lastStats) {
        const mig = lastStats.match(/migrations=(\d+)/);
        const lpr = lastStats.match(/lp_removals=(\d+)/);
        const ntk = lastStats.match(/new_tokens_prewarmed=(\d+)/);
        const crs = lastStats.match(/creator_sells=(\d+)/);
        if (mig || lpr || ntk || crs) {
          lines.push('');
          lines.push('📡 Stream Events (session, from log)');
          lines.push(`  Migrations detected: ${mig ? mig[1] : 0}`);
          lines.push(`  LP removals: ${lpr ? lpr[1] : 0}`);
          lines.push(`  New tokens pre-warmed: ${ntk ? ntk[1] : 0}`);
          lines.push(`  Creator sells: ${crs ? crs[1] : 0}`);
        }
      }
    } catch {}
  }

  if (paused) lines.push('  ⚠️  TRADING PAUSED');
  if (newHigh) lines.push(`\n🏆 NEW HIGH WATER: ${sol(all.net)} SOL net!`);

  // Alerts
  const alerts = [];
  if (!ppOk)                                    alerts.push('🔴 PumpPortal feed stale/down');
  if (typeof ppAge === 'number' && ppAge > 60)  alerts.push(`🔴 PumpPortal last seen ${ppAge}s ago`);
  if (ses.n >= 10 && ses.wr < 0.30)            alerts.push(`⚠️  Win rate critical: ${pct(ses.wr)} on ${ses.n} trades`);
  if (ses.net < -0.03)                          alerts.push(`⚠️  Session PnL: ${sol(ses.net)} SOL`);
  if (ses.fees > 0 && ses.gross > 0 && ses.fees/ses.gross > 0.05) alerts.push(`⚠️  Fee drag: ${(ses.fees/ses.gross*100).toFixed(1)}% (>5%)`);

  if (alerts.length > 0) {
    lines.push('');
    lines.push('🚨 Alerts:');
    alerts.forEach(a => lines.push('  ' + a));
  }

  console.log(lines.join('\n'));

  // Save updated state
  saveJson(HB_STATE, {
    last_trade_count: allTrades.length,
    last_check_ts: Date.now(),
    last_win_rate: all.wr,
    mev_pnl_high_water: Math.max(all.net, prevHigh),
    new_trades_this_hb: newTrades,
  });

  // Exit code 1 if critical alert (for heartbeat to notice)
  if (alerts.some(a => a.startsWith('🔴'))) process.exit(2);
}

main().catch(e => { console.error('rust-status error:', e.message); process.exit(1); });
