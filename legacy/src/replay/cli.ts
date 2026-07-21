/**
 * @module replay/cli
 * CLI entrypoint for replay mode.
 * Replays persisted raw_events through the decision pipeline.
 */

import dotenv from 'dotenv';
dotenv.config();

import { createLogger } from '../utils/logger';
import { getConfigManager } from '../config/loader';
import { getDatabase } from '../persistence/database';
import { ReplayEngine } from './engine';

const log = createLogger('replay-cli');

async function main(): Promise<void> {
  const args = process.argv.slice(2);

  if (args.length < 2) {
    console.log('Usage: node dist/replay/cli.js <start_date> <end_date> [config_path]');
    console.log('  Dates in ISO format: 2024-01-01T00:00:00Z');
    process.exit(1);
  }

  const startMs = new Date(args[0]).getTime();
  const endMs = new Date(args[1]).getTime();
  const configPath = args[2] || process.env.CONFIG_PATH || 'config/default.json';

  if (isNaN(startMs) || isNaN(endMs)) {
    console.error('Invalid date format');
    process.exit(1);
  }

  log.info(`Starting replay: ${args[0]} to ${args[1]}`);

  const configManager = getConfigManager();
  const config = configManager.loadFromFile(configPath);
  const db = getDatabase();

  const engine = new ReplayEngine(db, config);
  const run = await engine.runReplay(startMs, endMs);

  console.log('\n=== Replay Results ===');
  console.log(`Status: ${run.status}`);
  console.log(`Events processed: ${run.event_count}`);
  console.log(`Trades: ${run.trade_count}`);
  console.log(`Net PnL: ${run.net_pnl_sol?.toFixed(4) ?? 'N/A'} SOL`);

  if (run.metrics) {
    console.log('\n--- Metrics ---');
    console.log(`Hit rate: ${(run.metrics.hit_rate * 100).toFixed(1)}%`);
    console.log(`Net expectancy: ${run.metrics.net_expectancy_per_trade.toFixed(6)} SOL/trade`);
    console.log(`Max drawdown: ${run.metrics.max_drawdown.toFixed(4)} SOL`);
    console.log(`Avg hold time: ${run.metrics.avg_hold_time_s.toFixed(1)}s`);
    console.log(`Forced exits: ${run.metrics.forced_exits}`);
  }

  if (run.error) {
    console.error(`Error: ${run.error}`);
  }

  db.close();
}

main().catch(err => {
  console.error(`Replay failed: ${err.message}`);
  process.exit(1);
});
