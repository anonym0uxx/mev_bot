#!/usr/bin/env node
/**
 * status-daemon.js — standalone background process
 * Sends P&L status to Telegram every 5 minutes
 * Alerts on new trades, daemon crashes, fee drag
 * Run via: nohup node scripts/status-daemon.js > logs/status-daemon.log 2>&1 &
 */
const Database = require('better-sqlite3');
const path = require('path');
const { execSync } = require('child_process');

const DB_PATH = path.join(__dirname, '../data/pump-quant.db');
const TELEGRAM_TARGET = 'telegram:5024153101';
const CHECK_INTERVAL_MS = 5 * 60 * 1000; // 5 minutes
const API_URL = 'http://127.0.0.1:9420';

let lastTradeCount = 0;
let lastReportedPnl = null;
let startedAt = Date.now();

function log(msg) {
  console.log(new Date().toISOString(), msg);
}

function sendMessage(text) {
  try {
    // Write to temp file, use process substitution via stdin
    const fs = require('fs');
    const tmpFile = '/tmp/status-msg.txt';
    fs.writeFileSync(tmpFile, text);
    execSync(`openclaw message send --channel telegram --target "${TELEGRAM_TARGET}" --message "$(cat ${tmpFile})"`, {
      timeout: 10000,
      stdio: ['ignore', 'pipe', 'pipe'],
      shell: '/bin/bash'
    });
    log('Sent via openclaw: ' + text.slice(0, 60));
    return true;
  } catch (err) {
    log('openclaw send failed: ' + err.message);
    return false;
  }
}

async function sendViaCurl(text) {
  // Direct Telegram API call using stored credentials
  // Read bot token from environment or config
  try {
    const fs = require('fs');
    const envPath = path.join(__dirname, '../.env');
    const env = fs.readFileSync(envPath, 'utf8');
    const tokenMatch = env.match(/TELEGRAM_BOT_TOKEN=(.+)/);
    const chatIdMatch = env.match(/TELEGRAM_CHAT_ID=(.+)/);
    if (!tokenMatch || !chatIdMatch) return false;
    const token = tokenMatch[1].trim();
    const chatId = chatIdMatch[1].trim();
    const encoded = encodeURIComponent(text);
    execSync(`curl -s "https://api.telegram.org/bot${token}/sendMessage?chat_id=${chatId}&text=${encoded}&parse_mode=Markdown"`, {
      timeout: 10000, stdio: ['ignore', 'pipe', 'pipe']
    });
    return true;
  } catch {
    return false;
  }
}

function buildStatusReport() {
  try {
    const db = new Database(DB_PATH, { readonly: true });
    const orders = db.prepare("SELECT * FROM orders WHERE status='confirmed' AND is_paper=0 ORDER BY created_at ASC").all();
    const open = db.prepare("SELECT * FROM positions WHERE status='OPEN'").all();

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
      const buySOL = buys.reduce((s,t) => s+(t.realized_sol||0), 0);
      const sellSOL = sells.reduce((s,t) => s+(t.realized_sol||0), 0);
      const fees = txs.reduce((s,t) => s+(t.fee_sol||0)+(t.priority_fee_paid_sol||0), 0);
      const pnl = sellSOL - buySOL - fees;
      trades.push({ mint, pnl, holdS: Math.round((sells[sells.length-1].confirmed_at - buys[0].confirmed_at)/1000), ts: buys[0].confirmed_at });
    }

    const totalPnl = trades.reduce((s,t) => s+t.pnl, 0);
    const wins = trades.filter(t => t.pnl > 0).length;
    const losses = trades.filter(t => t.pnl <= 0).length;
    const totalFees = orders.reduce((s,o) => s+(o.fee_sol||0)+(o.priority_fee_paid_sol||0), 0);
    const totalBuy = orders.filter(o=>o.side==='buy').reduce((s,o)=>s+(o.realized_sol||0),0);
    const feeDrag = totalBuy > 0 ? (totalFees/totalBuy*100).toFixed(1) : '0';
    const winRate = trades.length > 0 ? (wins/trades.length*100).toFixed(0) : 0;
    const pnlSign = totalPnl >= 0 ? '+' : '';
    const pnlEmoji = totalPnl >= 0 ? '🟢' : '🔴';
    const now = new Date().toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', timeZone: 'America/Los_Angeles' });

    const recent = [...trades].sort((a,b) => b.ts - a.ts).slice(0, 5);

    db.close();
    return {
      text: `📊 *Bot Status — ${now} PDT*\n\n${pnlEmoji} *Net PnL:* \`${pnlSign}${totalPnl.toFixed(4)} SOL\`\n🎯 *Win Rate:* ${winRate}% (${wins}W / ${losses}L)\n📈 *Trades:* ${trades.length} closed\n💸 *Fee Drag:* ${feeDrag}%\n🏦 *Open Positions:* ${open.length}\n\n*Recent trades:*\n${recent.map(t => `${t.pnl>=0?'✅':'❌'} \`${t.mint.slice(0,8)}\` ${(t.pnl>=0?'+':'')+t.pnl.toFixed(4)} SOL (${t.holdS}s)`).join('\n') || '_No trades yet_'}`,
      tradeCount: trades.length,
      totalPnl,
    };
  } catch (err) {
    return { text: `⚠️ Status report error: ${err.message}`, tradeCount: 0, totalPnl: 0 };
  }
}

function checkDaemonHealth() {
  try {
    const result = execSync(`curl -s ${API_URL}/api/health`, { timeout: 5000 }).toString();
    const data = JSON.parse(result);
    return data?.data?.overall === 'healthy';
  } catch {
    return false;
  }
}

async function runCheck() {
  const report = buildStatusReport();

  // Always send status report — try openclaw CLI first
  const sent = sendMessage(report.text);
  if (!sent) await sendViaCurl(report.text);

  // Alert on new trades
  if (report.tradeCount > lastTradeCount && lastTradeCount > 0) {
    log(`New trades: ${lastTradeCount} → ${report.tradeCount}`);
  }
  lastTradeCount = report.tradeCount;

  // Alert if PnL dropped significantly
  if (lastReportedPnl !== null && report.totalPnl < lastReportedPnl - 0.01) {
    const alertSent = await sendViaCurl(`🔴 *PnL Alert*: dropped ${(report.totalPnl - lastReportedPnl).toFixed(4)} SOL (now ${report.totalPnl.toFixed(4)} SOL)`);
    if (!alertSent) sendMessage(`🔴 PnL Alert: dropped ${(report.totalPnl - lastReportedPnl).toFixed(4)} SOL`);
  }
  lastReportedPnl = report.totalPnl;

  // Check daemon health
  const healthy = checkDaemonHealth();
  if (!healthy) {
    log('Daemon unhealthy — attempting restart');
    sendMessage(`⚠️ Daemon down — restarting...`);
    try {
      execSync(`cd ${path.join(__dirname, '..')} && pkill -f "bash run-daemon.sh" 2>/dev/null; sleep 2; nohup bash run-daemon.sh > logs/supervisor.log 2>&1 &`, { timeout: 15000 });
    } catch {}
  }
}

log('Status daemon starting — reporting every 5 minutes');
runCheck(); // immediate first report
setInterval(runCheck, CHECK_INTERVAL_MS);
