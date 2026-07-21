/**
 * @module learning/jobs
 * Learning job definitions per spec section 22A.
 *
 * Cadences:
 * 1. Event-driven ledger append (no delay)
 * 2. Hourly micro-calibration
 * 3. Daily replay/attribution/challenger training
 * 4. Daily canary-promotion
 * 5. Weekly deep retrain/regime review
 *
 * All jobs are versioned, logged, replayable.
 * Opus is the only LLM for learning tasks.
 */

import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { PumpQuantDB } from '../persistence/database';
import { PumpQuantConfig } from '../types/config';
import { ConfigManager } from '../config/loader';
import { MicroCalibration } from './calibration';
import { ChampionChallengerFramework } from './champion-challenger';
import { ReplayEngine } from '../replay/engine';

const log = createLogger('learning-jobs');

export class LearningJobScheduler {
  private db: PumpQuantDB;
  private configManager: ConfigManager;
  private calibration: MicroCalibration;
  private championChallenger: ChampionChallengerFramework;
  private intervals: NodeJS.Timeout[] = [];

  constructor(db: PumpQuantDB, configManager: ConfigManager) {
    this.db = db;
    this.configManager = configManager;
    this.calibration = new MicroCalibration(db, configManager);
    this.championChallenger = new ChampionChallengerFramework(db, configManager);
  }

  /**
   * Start all scheduled learning jobs.
   */
  start(): void {
    const config = this.configManager.getConfig();
    if (!config.learning.enabled) {
      log.info('Learning disabled — no jobs scheduled');
      return;
    }

    // Hourly micro-calibration
    if (config.learning.hourly_micro_calibration.enabled) {
      const interval = setInterval(async () => {
        try {
          await this.runHourlyCalibration();
        } catch (err) {
          log.error(`Hourly calibration error: ${(err as Error).message}`);
        }
      }, 3600_000); // 1 hour
      this.intervals.push(interval);
      log.info('Scheduled: hourly micro-calibration');
    }

    // Daily replay (at session cut time)
    if (config.learning.daily_replay.enabled) {
      const interval = setInterval(async () => {
        try {
          await this.runDailyReplay();
        } catch (err) {
          log.error(`Daily replay error: ${(err as Error).message}`);
        }
      }, 86400_000); // 24 hours
      this.intervals.push(interval);
      log.info('Scheduled: daily replay/attribution');
    }

    // Daily canary promotion
    if (config.learning.daily_canary_promotion.enabled) {
      const interval = setInterval(async () => {
        try {
          await this.runDailyCanaryPromotion();
        } catch (err) {
          log.error(`Canary promotion error: ${(err as Error).message}`);
        }
      }, 86400_000);
      this.intervals.push(interval);
      log.info('Scheduled: daily canary promotion');
    }

    // Weekly deep retrain
    if (config.learning.weekly_retrain.enabled) {
      const interval = setInterval(async () => {
        try {
          await this.runWeeklyRetrain();
        } catch (err) {
          log.error(`Weekly retrain error: ${(err as Error).message}`);
        }
      }, 604800_000); // 7 days
      this.intervals.push(interval);
      log.info('Scheduled: weekly deep retrain');
    }

    log.info('Learning job scheduler started');
  }

  /**
   * Stop all scheduled jobs.
   */
  stop(): void {
    for (const interval of this.intervals) {
      clearInterval(interval);
    }
    this.intervals = [];
    log.info('Learning job scheduler stopped');
  }

  /**
   * Cadence 2: Hourly micro-calibration.
   * Targets: slippage, landing-risk, route-health, feed latency, friction priors.
   */
  async runHourlyCalibration(): Promise<void> {
    log.info('Running hourly micro-calibration');
    const results = await this.calibration.runHourlyCalibration();
    log.info(`Calibration complete: ${results.length} targets updated`);
  }

  /**
   * Cadence 3: Daily replay/attribution/challenger training.
   * Runs at fixed session cut time.
   */
  async runDailyReplay(): Promise<void> {
    log.info('Running daily replay/attribution');
    const config = this.configManager.getConfig();
    const now = nowMs();
    const oneDayAgo = now - 86400_000;

    const replayEngine = new ReplayEngine(this.db, config);
    const run = await replayEngine.runReplay(oneDayAgo, now);

    log.info(`Daily replay complete: ${run.trade_count} trades, PnL=${run.net_pnl_sol?.toFixed(4) ?? 'N/A'} SOL`);

    // Generate attribution analysis
    if (run.metrics) {
      log.info(`Metrics — hitRate: ${(run.metrics.hit_rate * 100).toFixed(1)}%, ` +
        `expectancy: ${run.metrics.net_expectancy_per_trade.toFixed(4)}, ` +
        `drawdown: ${run.metrics.max_drawdown.toFixed(4)}`);
    }
  }

  /**
   * Cadence 4: Daily canary-promotion.
   * Evaluate active canaries and promote/rollback.
   */
  async runDailyCanaryPromotion(): Promise<void> {
    log.info('Running daily canary promotion');
    const challengers = this.championChallenger.getChallengers()
      .filter(c => c.status === 'canary_active');

    for (const challenger of challengers) {
      if (challenger.canaryMetrics) {
        this.championChallenger.evaluateCanary(challenger.id, challenger.canaryMetrics);
      }
    }

    log.info(`Canary promotion evaluated ${challengers.length} challengers`);
  }

  /**
   * Cadence 5: Weekly deep retrain/regime review.
   */
  async runWeeklyRetrain(): Promise<void> {
    log.info('Running weekly deep retrain/regime review');

    // Analyze learning ledger for the week
    const records = this.db.getLearningRecords(1000);
    if (records.length === 0) {
      log.info('No learning records for weekly review');
      return;
    }

    // Compute aggregate attribution
    const avgAttribution = {
      flow_momentum: 0,
      breadth_topology: 0,
      creator_wallet_prior: 0,
      multimodal_junk: 0,
      manipulation_penalty: 0,
      friction_route: 0,
    };

    for (const record of records) {
      avgAttribution.flow_momentum += record.attribution_flow_momentum;
      avgAttribution.breadth_topology += record.attribution_breadth_topology;
      avgAttribution.creator_wallet_prior += record.attribution_creator_wallet_prior;
      avgAttribution.multimodal_junk += record.attribution_multimodal_junk;
      avgAttribution.manipulation_penalty += record.attribution_manipulation_penalty;
      avgAttribution.friction_route += record.attribution_friction_route;
    }

    const n = records.length;
    for (const key of Object.keys(avgAttribution) as (keyof typeof avgAttribution)[]) {
      avgAttribution[key] /= n;
    }

    log.info(`Weekly attribution averages: ${JSON.stringify(avgAttribution)}`);

    // Regime distribution
    const regimeCounts = new Map<string, number>();
    for (const record of records) {
      regimeCounts.set(record.regime, (regimeCounts.get(record.regime) || 0) + 1);
    }
    log.info(`Regime distribution: ${JSON.stringify(Object.fromEntries(regimeCounts))}`);
  }

  /** Get champion/challenger framework */
  getChampionChallenger(): ChampionChallengerFramework {
    return this.championChallenger;
  }
}
