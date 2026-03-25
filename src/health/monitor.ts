/**
 * @module health/monitor
 * Health monitoring subsystem per spec section 19.
 *
 * Checks: market feed, friction estimate, probability layer, datastore,
 * execution adapter, config integrity.
 *
 * If any required subsystem is stale/broken:
 * - NO NEW TRADES
 * - Optionally flatten if in risk-off mode
 * - Surface error via get_bot_health()
 *
 * FAIL CLOSED TO NO_TRADE, NEVER FAIL OPEN.
 */

import { v4 as uuidv4 } from 'uuid';
import { createLogger } from '../utils/logger';
import { nowMs, ageS, isStale } from '../utils/time';
import { HealthEvent, HealthSubsystem, HealthStatus } from '../types/events';
import { PumpQuantConfig } from '../types/config';
import { PumpQuantDB } from '../persistence/database';

const log = createLogger('health');

/** Overall system health assessment */
export interface SystemHealth {
  overall: HealthStatus;
  tradingAllowed: boolean;
  subsystems: SubsystemHealth[];
  lastCheckAt: number;
  pauseReason: string | null;
}

export interface SubsystemHealth {
  name: HealthSubsystem;
  status: HealthStatus;
  lastUpdateAt: number;
  staleSinceS: number;
  message: string;
}

export class HealthMonitor {
  private config: PumpQuantConfig;
  private db: PumpQuantDB;
  private lastCheckAt: number = 0;
  private pauseReason: string | null = null;
  private isPaused: boolean = false;

  /** Last known update timestamps for each subsystem */
  private subsystemTimestamps: Map<HealthSubsystem, number> = new Map();

  constructor(db: PumpQuantDB, config: PumpQuantConfig) {
    this.db = db;
    this.config = config;

    // Initialize with current time
    const now = nowMs();
    for (const sub of [
      'market_feed', 'friction_estimate', 'probability_layer',
      'datastore', 'execution_adapter', 'config_integrity',
    ] as HealthSubsystem[]) {
      this.subsystemTimestamps.set(sub, now);
    }
  }

  /** Update config */
  updateConfig(config: PumpQuantConfig): void {
    this.config = config;
  }

  /** Record that a subsystem was updated */
  recordUpdate(subsystem: HealthSubsystem): void {
    this.subsystemTimestamps.set(subsystem, nowMs());
  }

  /** Pause trading with a reason */
  pause(reason: string): void {
    this.isPaused = true;
    this.pauseReason = reason;
    log.warn(`Trading PAUSED: ${reason}`);

    this.recordHealthEvent('execution_adapter', 'degraded', `Paused: ${reason}`);
  }

  /** Resume trading */
  resume(): void {
    this.isPaused = false;
    this.pauseReason = null;
    log.info('Trading RESUMED');
  }

  /** Check if trading is currently paused */
  get paused(): boolean {
    return this.isPaused;
  }

  /**
   * Run a full health check across all subsystems.
   * Returns overall health assessment.
   */
  check(): SystemHealth {
    const now = nowMs();
    this.lastCheckAt = now;

    const subsystems: SubsystemHealth[] = [];

    // Check each subsystem
    subsystems.push(this.checkSubsystem(
      'market_feed',
      this.config.health.market_feed_stale_s
    ));

    subsystems.push(this.checkSubsystem(
      'friction_estimate',
      this.config.friction.stale_threshold_s
    ));

    subsystems.push(this.checkSubsystem(
      'probability_layer',
      this.config.health.probability_stale_s
    ));

    subsystems.push(this.checkSubsystem(
      'datastore',
      60 // 60s stale threshold for datastore
    ));

    subsystems.push(this.checkSubsystem(
      'execution_adapter',
      this.config.health.execution_stale_s
    ));

    subsystems.push(this.checkSubsystem(
      'config_integrity',
      3600 // Config integrity checked less frequently
    ));

    // Determine overall status
    const hasFailed = subsystems.some(s => s.status === 'failed');
    const hasDegraded = subsystems.some(s => s.status === 'degraded');

    let overall: HealthStatus;
    if (hasFailed) {
      overall = 'failed';
    } else if (hasDegraded) {
      overall = 'degraded';
    } else {
      overall = 'healthy';
    }

    // Trading allowed?
    const tradingAllowed = !this.isPaused && overall === 'healthy';

    // Auto-pause on degraded if configured
    if (this.config.health.auto_pause_on_degraded && (overall === 'failed' || overall === 'degraded') && !this.isPaused) {
      const failedSubs = subsystems.filter(s => s.status !== 'healthy').map(s => s.name);
      this.pause(`Auto-pause: ${failedSubs.join(', ')} unhealthy`);
    }

    // Auto-resume if all subsystems recovered and pause was auto-triggered
    if (this.isPaused && overall === 'healthy' && this.pauseReason?.startsWith('Auto-pause:')) {
      log.info('All subsystems healthy — auto-resuming');
      this.resume();
    }

    return {
      overall,
      tradingAllowed,
      subsystems,
      lastCheckAt: now,
      pauseReason: this.pauseReason,
    };
  }

  /** Check a single subsystem */
  private checkSubsystem(
    name: HealthSubsystem,
    staleThresholdS: number
  ): SubsystemHealth {
    const lastUpdate = this.subsystemTimestamps.get(name) || 0;
    const staleSinceS = ageS(lastUpdate);

    let status: HealthStatus;
    let message: string;

    if (staleSinceS > staleThresholdS * 2) {
      status = 'failed';
      message = `Stale for ${staleSinceS.toFixed(0)}s (threshold: ${staleThresholdS}s)`;
    } else if (staleSinceS > staleThresholdS) {
      status = 'degraded';
      message = `Degraded: stale for ${staleSinceS.toFixed(0)}s`;
    } else {
      status = 'healthy';
      message = 'OK';
    }

    // Record health event on status change
    const prevEvent = this.db.getLatestHealthBySubsystem(name);
    if (!prevEvent || prevEvent.status !== status) {
      this.recordHealthEvent(name, status, message);
    }

    return {
      name,
      status,
      lastUpdateAt: lastUpdate,
      staleSinceS,
      message,
    };
  }

  /** Persist a health event */
  private recordHealthEvent(subsystem: HealthSubsystem, status: HealthStatus, message: string): void {
    const event: HealthEvent = {
      id: uuidv4(),
      subsystem,
      status,
      message,
      timestamp: nowMs(),
    };
    this.db.insertHealthEvent(event);
  }
}
