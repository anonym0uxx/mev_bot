#!/usr/bin/env node
/**
 * P&L Summary reporter — 5-minute cron status update.
 * Delegates to rust-status.js which reads the correct momentum JSONL
 * and reports from the live Rust daemon at :9421.
 */
const { execSync } = require('child_process');
const path = require('path');

const rustStatus = path.join(__dirname, 'rust-status.js');

try {
  const out = execSync(`PAPER_MODE=true node ${rustStatus}`, {
    cwd: path.join(__dirname, '..'),
    timeout: 25000,
    env: { ...process.env, PAPER_MODE: 'true' }
  }).toString().trim();
  console.log(out);
} catch (e) {
  console.log('⚠️ Status script error: ' + (e.message || e));
}
