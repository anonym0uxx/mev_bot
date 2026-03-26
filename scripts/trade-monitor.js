#!/usr/bin/env node
/**
 * Real-time trade monitor - alerts on Telegram for every buy/sell
 */

const db = require('better-sqlite3')('/data/.openclaw/workspace/projects/pump-quant/data/pump-quant.db');
const fs = require('fs');

const STATE_FILE = '/data/.openclaw/workspace/projects/pump-quant/data/monitor-state.json';
const TELEGRAM_CHAT_ID = '5024153101';

// Load last known state
let lastState = { lastPositionId: 0, alertedPositions: [] };
try {
  if (fs.existsSync(STATE_FILE)) {
    lastState = JSON.parse(fs.readFileSync(STATE_FILE, 'utf8'));
  }
} catch (err) {
  console.error('Failed to load state:', err.message);
}

function saveState() {
  fs.writeFileSync(STATE_FILE, JSON.stringify(lastState));
}

function sendTelegramAlert(message) {
  // Use OpenClaw message tool via file trigger
  const alertFile = `/tmp/pump-alert-${Date.now()}.txt`;
  fs.writeFileSync(alertFile, JSON.stringify({
    channel: 'telegram',
    to: `telegram:${TELEGRAM_CHAT_ID}`,
    message
  }));
  
  // Trigger via exec (Apollo will pick this up)
  console.log('[ALERT]', message);
  
  // Also log to daemon log
  const logMsg = `[TRADE ALERT] ${message}`;
  fs.appendFileSync('/data/.openclaw/workspace/projects/pump-quant/logs/daemon.log', 
    `${new Date().toISOString()} ${logMsg}\n`);
}

function checkForNewTrades() {
  // Get all positions newer than last check
  const newPositions = db.prepare(`
    SELECT * FROM positions 
    WHERE id > ? 
    ORDER BY id ASC
  `).all(lastState.lastPositionId);

  if (newPositions.length === 0) return;

  newPositions.forEach(pos => {
    const mint = pos.mint.slice(0, 8);
    const ageMin = ((Date.now() - pos.opened_at) / 60000).toFixed(1);
    
    // Check if we already alerted about this position's entry
    if (!lastState.alertedPositions.includes(`${pos.id}-entry`)) {
      const entryMsg = `🟢 **BUY EXECUTED**\n` +
        `Token: ${mint}...\n` +
        `Size: ${pos.entry_sol.toFixed(4)} SOL\n` +
        `Price: ${pos.entry_price_sol.toFixed(8)} SOL\n` +
        `Regime: ${pos.regime}\n` +
        `Time: ${new Date(pos.opened_at).toLocaleString('en-US', { timeZone: 'America/Los_Angeles' })} PDT`;
      
      sendTelegramAlert(entryMsg);
      lastState.alertedPositions.push(`${pos.id}-entry`);
    }

    // Check if position was closed
    if (pos.closed_at && !lastState.alertedPositions.includes(`${pos.id}-exit`)) {
      const pnl = pos.realized_pnl_sol || 0;
      const pnlPct = pos.realized_pnl_pct || 0;
      const emoji = pnl > 0 ? '🟢' : '🔴';
      const sign = pnl > 0 ? '+' : '';
      
      const exitMsg = `${emoji} **SELL EXECUTED**\n` +
        `Token: ${mint}...\n` +
        `PnL: ${sign}${pnl.toFixed(4)} SOL (${sign}${pnlPct.toFixed(2)}%)\n` +
        `Exit Price: ${(pos.exit_price_sol || 0).toFixed(8)} SOL\n` +
        `Hold Time: ${(pos.hold_duration_s || 0).toFixed(0)}s\n` +
        `Reason: ${pos.exit_reason || 'unknown'}\n` +
        `Fees: ${pos.total_fees_sol.toFixed(4)} SOL`;
      
      sendTelegramAlert(exitMsg);
      lastState.alertedPositions.push(`${pos.id}-exit`);
    }

    // Update last seen position ID
    if (pos.id > lastState.lastPositionId) {
      lastState.lastPositionId = pos.id;
    }
  });

  // Keep only last 100 alerted positions (prevent unbounded growth)
  if (lastState.alertedPositions.length > 100) {
    lastState.alertedPositions = lastState.alertedPositions.slice(-100);
  }

  saveState();
}

// Main loop - check every 2 seconds
console.log('Trade monitor started - watching for new positions...');
setInterval(checkForNewTrades, 2000);

// Also check immediately
checkForNewTrades();
