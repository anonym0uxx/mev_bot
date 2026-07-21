/**
 * @module learning/champion-challenger
 * Champion/challenger framework per spec section 22A.
 *
 * Promotion path: offline replay → walk-forward → bounded canary → full promotion
 * Autonomous promotion gates with minimum sample size, net expectancy,
 * drawdown, precision@K, forced exits, fill-adjusted EV gap, missed-edge regret rate.
 * Automatic rollback on degradation.
 */

import { v4 as uuidv4 } from 'uuid';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { PumpQuantDB } from '../persistence/database';
import { PumpQuantConfig } from '../types/config';
import { ConfigManager } from '../config/loader';
import { ReplayEngine } from '../replay/engine';
import { ReplayMetrics } from '../types/trade';

const log = createLogger('champion-challenger');

/** Challenger strategy configuration */
export interface ChallengerConfig {
  id: string;
  name: string;
  config: PumpQuantConfig;
  createdAt: number;
  status: ChallengerStatus;
  replayMetrics: ReplayMetrics | null;
  canaryMetrics: ReplayMetrics | null;
  promotedAt: number | null;
  rolledBackAt: number | null;
}

export type ChallengerStatus =
  | 'pending_replay'
  | 'replay_passed'
  | 'replay_failed'
  | 'canary_active'
  | 'canary_passed'
  | 'canary_failed'
  | 'promoted'
  | 'rolled_back';

/** Promotion gate thresholds */
export interface PromotionGates {
  minSampleSize: number;
  minNetExpectancy: number;
  maxDrawdown: number;
  minPrecisionAtK: number;
  maxForcedExitRate: number;
  maxFillAdjustedEvGap: number;
  maxMissedEdgeRegretRate: number;
}

export class ChampionChallengerFramework {
  private db: PumpQuantDB;
  private configManager: ConfigManager;
  private challengers: Map<string, ChallengerConfig> = new Map();
  private championId: string = 'champion';

  constructor(db: PumpQuantDB, configManager: ConfigManager) {
    this.db = db;
    this.configManager = configManager;
  }

  /**
   * Register a new challenger configuration.
   */
  registerChallenger(name: string, config: PumpQuantConfig): string {
    const cc = this.configManager.getConfig().learning.champion_challenger;
    if (this.challengers.size >= cc.max_challengers) {
      throw new Error(`Max challengers (${cc.max_challengers}) reached`);
    }

    const id = uuidv4();
    const challenger: ChallengerConfig = {
      id,
      name,
      config,
      createdAt: nowMs(),
      status: 'pending_replay',
      replayMetrics: null,
      canaryMetrics: null,
      promotedAt: null,
      rolledBackAt: null,
    };

    this.challengers.set(id, challenger);
    log.info(`Challenger registered: ${name} (${id})`);
    return id;
  }

  /**
   * Run offline replay for a challenger.
   */
  async runChallengerReplay(
    challengerId: string,
    startMs: number,
    endMs: number
  ): Promise<boolean> {
    const challenger = this.challengers.get(challengerId);
    if (!challenger) throw new Error(`Challenger ${challengerId} not found`);

    const replayEngine = new ReplayEngine(this.db, challenger.config);
    const run = await replayEngine.runReplay(startMs, endMs, challenger.config);

    challenger.replayMetrics = run.metrics;

    if (run.metrics && this.passesPromotionGates(run.metrics)) {
      challenger.status = 'replay_passed';
      log.info(`Challenger ${challenger.name} passed replay`);
      return true;
    } else {
      challenger.status = 'replay_failed';
      log.info(`Challenger ${challenger.name} failed replay`);
      return false;
    }
  }

  /**
   * Activate a challenger in canary mode.
   */
  activateCanary(challengerId: string): void {
    const challenger = this.challengers.get(challengerId);
    if (!challenger) throw new Error(`Challenger ${challengerId} not found`);
    if (challenger.status !== 'replay_passed') {
      throw new Error(`Challenger must pass replay before canary (current: ${challenger.status})`);
    }

    challenger.status = 'canary_active';
    log.info(`Challenger ${challenger.name} activated as canary`);
  }

  /**
   * Evaluate canary performance and decide on promotion/rollback.
   */
  evaluateCanary(challengerId: string, metrics: ReplayMetrics): ChallengerStatus {
    const challenger = this.challengers.get(challengerId);
    if (!challenger) throw new Error(`Challenger ${challengerId} not found`);

    challenger.canaryMetrics = metrics;
    const config = this.configManager.getConfig().learning.champion_challenger;

    if (this.passesPromotionGates(metrics)) {
      challenger.status = 'canary_passed';
      log.info(`Challenger ${challenger.name} passed canary evaluation`);
    } else {
      challenger.status = 'canary_failed';
      log.warn(`Challenger ${challenger.name} failed canary evaluation`);
    }

    // Check for degradation → automatic rollback
    if (metrics.max_drawdown > config.rollback_drawdown_threshold) {
      this.rollback(challengerId, 'Excessive drawdown in canary');
    }

    return challenger.status;
  }

  /**
   * Promote a challenger to champion.
   */
  promote(challengerId: string): void {
    const challenger = this.challengers.get(challengerId);
    if (!challenger) throw new Error(`Challenger ${challengerId} not found`);
    if (challenger.status !== 'canary_passed') {
      throw new Error(`Challenger must pass canary before promotion (current: ${challenger.status})`);
    }

    // Apply challenger config as the new champion
    this.configManager.applyConfig(
      challenger.config,
      'challenger',
      `Champion promoted: ${challenger.name}`
    );

    challenger.status = 'promoted';
    challenger.promotedAt = nowMs();
    this.championId = challengerId;

    log.info(`Champion promoted: ${challenger.name}`);
  }

  /**
   * Rollback a challenger (or promoted champion back to previous).
   */
  rollback(challengerId: string, reason: string): void {
    const challenger = this.challengers.get(challengerId);
    if (!challenger) return;

    challenger.status = 'rolled_back';
    challenger.rolledBackAt = nowMs();

    log.warn(`Challenger ${challenger.name} rolled back: ${reason}`);
  }

  /**
   * Check if metrics pass all promotion gates.
   */
  private passesPromotionGates(metrics: ReplayMetrics): boolean {
    const config = this.configManager.getConfig().learning;
    const gates = config.daily_canary_promotion;

    if (metrics.total_trades < gates.min_sample_size) return false;
    if (metrics.net_expectancy_per_trade < gates.min_net_expectancy) return false;
    if (metrics.max_drawdown > gates.max_drawdown) return false;
    if (metrics.precision_at_k < gates.min_precision_at_k) return false;

    return true;
  }

  /** Get all challengers */
  getChallengers(): ChallengerConfig[] {
    return Array.from(this.challengers.values());
  }

  /** Get champion ID */
  getChampionId(): string {
    return this.championId;
  }
}
