/**
 * @module learning/calibration
 * Hourly micro-calibration and learning job structures.
 * Targets: slippage, landing-risk, route-health, feed latency, friction priors.
 */

import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { PumpQuantDB } from '../persistence/database';
import { PumpQuantConfig } from '../types/config';
import { ConfigManager } from '../config/loader';

const log = createLogger('calibration');

export interface CalibrationResult {
  target: string;
  oldValue: number;
  newValue: number;
  sampleSize: number;
  timestamp: number;
}

export class MicroCalibration {
  private db: PumpQuantDB;
  private configManager: ConfigManager;
  private lastCalibrationAt: number = 0;

  constructor(db: PumpQuantDB, configManager: ConfigManager) {
    this.db = db;
    this.configManager = configManager;
  }

  /**
   * Run hourly micro-calibration across all targets.
   */
  async runHourlyCalibration(): Promise<CalibrationResult[]> {
    const config = this.configManager.getConfig();
    if (!config.learning.hourly_micro_calibration.enabled) {
      return [];
    }

    const results: CalibrationResult[] = [];
    const targets = config.learning.hourly_micro_calibration.targets;

    for (const target of targets) {
      try {
        const result = await this.calibrateTarget(target, config);
        if (result) {
          results.push(result);
        }
      } catch (err) {
        log.error(`Calibration failed for ${target}: ${(err as Error).message}`);
      }
    }

    if (results.length > 0) {
      // Apply calibrated values as config patch
      const patch = this.buildConfigPatch(results, config);
      if (Object.keys(patch).length > 0) {
        this.configManager.applyPatch(
          patch as any,
          'learning',
          `Hourly micro-calibration: ${results.map(r => r.target).join(', ')}`
        );
      }
    }

    this.lastCalibrationAt = nowMs();
    log.info(`Hourly calibration complete: ${results.length} targets updated`);
    return results;
  }

  /**
   * Calibrate a single target using recent execution data.
   */
  private async calibrateTarget(
    target: string,
    config: PumpQuantConfig
  ): Promise<CalibrationResult | null> {
    const now = nowMs();
    const windowMs = 3600_000; // 1 hour of data

    switch (target) {
      case 'slippage': {
        // Calibrate entry/exit slippage from recent orders
        const orders = this.db.getDb().prepare(`
          SELECT realized_slippage_pct, side FROM orders
          WHERE confirmed_at > ? AND realized_slippage_pct IS NOT NULL AND status = 'confirmed'
          ORDER BY confirmed_at DESC LIMIT 50
        `).all(now - windowMs) as any[];

        if (orders.length < 5) return null;

        const buySlippages = orders.filter((o: any) => o.side === 'buy').map((o: any) => o.realized_slippage_pct);
        const sellSlippages = orders.filter((o: any) => o.side === 'sell').map((o: any) => o.realized_slippage_pct);

        const avgBuySlippage = buySlippages.length > 0
          ? buySlippages.reduce((a: number, b: number) => a + b, 0) / buySlippages.length
          : config.friction.default_entry_slippage_pct;

        return {
          target: 'slippage',
          oldValue: config.friction.default_entry_slippage_pct,
          newValue: this.emaUpdate(config.friction.default_entry_slippage_pct, avgBuySlippage, 0.3),
          sampleSize: orders.length,
          timestamp: now,
        };
      }

      case 'landing_risk': {
        // Calibrate from retry/failure data
        const orders = this.db.getDb().prepare(`
          SELECT status, retry_count FROM orders
          WHERE created_at > ? ORDER BY created_at DESC LIMIT 50
        `).all(now - windowMs) as any[];

        if (orders.length < 5) return null;

        const failRate = orders.filter((o: any) => o.status === 'failed').length / orders.length;

        return {
          target: 'landing_risk',
          oldValue: 0, // Tracked in execution features
          newValue: failRate,
          sampleSize: orders.length,
          timestamp: now,
        };
      }

      case 'route_health': {
        // Route health is updated by route-policy module — just log current state
        return null;
      }

      case 'feed_latency': {
        // Track feed latency from raw events
        const events = this.db.getDb().prepare(`
          SELECT timestamp, received_at FROM raw_events
          WHERE received_at > ? ORDER BY received_at DESC LIMIT 100
        `).all(now - windowMs) as any[];

        if (events.length < 10) return null;

        const latencies = events.map((e: any) => e.received_at - e.timestamp).filter((l: number) => l >= 0);
        const avgLatency = latencies.reduce((a: number, b: number) => a + b, 0) / latencies.length;

        return {
          target: 'feed_latency',
          oldValue: 0,
          newValue: avgLatency,
          sampleSize: events.length,
          timestamp: now,
        };
      }

      case 'friction_priors': {
        // Overall friction calibration from realized costs
        return null; // Covered by slippage calibration
      }

      default:
        return null;
    }
  }

  /**
   * Build config patch from calibration results.
   */
  private buildConfigPatch(
    results: CalibrationResult[],
    config: PumpQuantConfig
  ): Record<string, unknown> {
    const patch: Record<string, unknown> = {};

    for (const result of results) {
      if (result.target === 'slippage') {
        patch.friction = {
          ...config.friction,
          default_entry_slippage_pct: Math.max(0.005, Math.min(0.2, result.newValue)),
        };
      }
    }

    return patch;
  }

  /** Exponential moving average update */
  private emaUpdate(oldValue: number, newValue: number, alpha: number): number {
    return alpha * newValue + (1 - alpha) * oldValue;
  }
}
