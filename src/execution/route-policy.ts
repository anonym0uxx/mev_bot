/**
 * @module execution/route-policy
 * Route scoring, promotion/demotion policy per spec section 13.
 *
 * Default: Local. Lightning only when edge justifies fee.
 * Jito only for true multi-transaction atomicity.
 * Maintains route-health priors for scoring.
 */

import { createLogger } from '../utils/logger';
import { nowMs, ageS } from '../utils/time';
import { RouteMode, ExecutionConfig, RouteHealthConfig } from '../types/config';

const log = createLogger('route-policy');

/** Route health statistics */
export interface RouteHealthStats {
  mode: RouteMode;
  /** Recent landing latency average (ms) */
  avgLandingLatencyMs: number;
  /** Recent retry/failure rate [0,1] */
  recentRetryRate: number;
  /** Recent congestion estimate [0,1] */
  recentCongestion: number;
  /** Fee burden as fraction of typical trade */
  feeBurden: number;
  /** Time since last successful execution (s) */
  freshnessS: number;
  /** Last updated timestamp */
  lastUpdatedAt: number;
  /** Composite route score [0,1] */
  routeScore: number;
  /** Route EV adjustment */
  routeEvAdjustment: number;
  /** Number of executions in scoring window */
  sampleCount: number;
}

/** Route execution record for health tracking */
export interface RouteExecutionRecord {
  mode: RouteMode;
  success: boolean;
  landingLatencyMs: number;
  retried: boolean;
  feePaid: number;
  tradeSizeSol: number;
  timestamp: number;
}

export class RoutePolicy {
  private healthStats: Map<RouteMode, RouteHealthStats> = new Map();
  private executionHistory: RouteExecutionRecord[] = [];
  private lastPromotionAt: number = 0;
  private lastDemotionAt: number = 0;
  private config: ExecutionConfig;

  constructor(config: ExecutionConfig) {
    this.config = config;
    this.initHealthStats();
  }

  /** Update config */
  updateConfig(config: ExecutionConfig): void {
    this.config = config;
  }

  /**
   * Select the best route for a trade given current conditions.
   *
   * @param entryEdge - Entry edge value
   * @param opportunityHalfLifeS - Estimated opportunity half-life in seconds
   * @param tradeSizeSol - Trade size
   * @returns Recommended route mode
   */
  selectRoute(
    entryEdge: number,
    opportunityHalfLifeS: number,
    tradeSizeSol: number
  ): RouteMode {
    // If promotion is disabled, always use default
    if (!this.config.route_promotion.enabled) {
      return this.config.default_route_mode;
    }

    const promo = this.config.route_promotion;

    // Check Lightning promotion
    if (
      opportunityHalfLifeS < promo.opportunity_half_life_threshold_s &&
      entryEdge > promo.min_edge_for_lightning &&
      this.isRouteHealthy('lightning')
    ) {
      // Check demotion cooldown
      if (nowMs() - this.lastDemotionAt > promo.demotion_cooldown_s * 1000) {
        log.info(`Route promoted to Lightning: edge=${entryEdge.toFixed(4)}, halfLife=${opportunityHalfLifeS.toFixed(1)}s`);
        this.lastPromotionAt = nowMs();
        return 'lightning';
      }
    }

    // Jito only for atomic multi-tx (not single buys/sells)
    // Not promoted here — only used explicitly

    return this.config.default_route_mode;
  }

  /**
   * Record an execution result for health tracking.
   */
  recordExecution(record: RouteExecutionRecord): void {
    this.executionHistory.push(record);

    // Prune old history (keep last 100)
    if (this.executionHistory.length > 100) {
      this.executionHistory = this.executionHistory.slice(-100);
    }

    // Update health stats
    this.updateHealthStats(record.mode);
  }

  /**
   * Get current health stats for a route mode.
   */
  getHealthStats(mode: RouteMode): RouteHealthStats {
    return this.healthStats.get(mode) || this.defaultHealthStats(mode);
  }

  /**
   * Get all route health stats.
   */
  getAllHealthStats(): Map<RouteMode, RouteHealthStats> {
    return new Map(this.healthStats);
  }

  /**
   * Check if a route is healthy enough for use.
   */
  isRouteHealthy(mode: RouteMode): boolean {
    const stats = this.healthStats.get(mode);
    if (!stats) return false;

    const hc = this.config.route_health;
    return (
      stats.avgLandingLatencyMs < hc.landing_latency_fail_ms &&
      stats.recentRetryRate < hc.retry_rate_fail &&
      stats.recentCongestion < hc.congestion_threshold &&
      stats.freshnessS < hc.freshness_max_s
    );
  }

  /**
   * Demote from a promoted route back to default.
   */
  demote(reason: string): void {
    this.lastDemotionAt = nowMs();
    log.info(`Route demoted back to ${this.config.default_route_mode}: ${reason}`);
  }

  /** Initialize health stats with defaults */
  private initHealthStats(): void {
    for (const mode of ['local', 'lightning', 'jito'] as RouteMode[]) {
      this.healthStats.set(mode, this.defaultHealthStats(mode));
    }
  }

  /** Update health stats from recent execution history */
  private updateHealthStats(mode: RouteMode): void {
    const recentWindow = 60 * 1000; // 60s window
    const now = nowMs();
    const recent = this.executionHistory.filter(
      r => r.mode === mode && r.timestamp > now - recentWindow
    );

    if (recent.length === 0) {
      // Keep existing stats but mark as stale
      const existing = this.healthStats.get(mode) || this.defaultHealthStats(mode);
      existing.freshnessS = ageS(existing.lastUpdatedAt);
      this.healthStats.set(mode, existing);
      return;
    }

    const avgLatency = recent.reduce((sum, r) => sum + r.landingLatencyMs, 0) / recent.length;
    const retryRate = recent.filter(r => r.retried || !r.success).length / recent.length;
    const lastExec = recent[recent.length - 1];

    // Congestion estimate: ratio of slow executions
    const slowThreshold = this.config.route_health.landing_latency_warn_ms;
    const congestion = recent.filter(r => r.landingLatencyMs > slowThreshold).length / recent.length;

    // Fee burden
    const avgFee = recent.reduce((sum, r) => sum + r.feePaid, 0) / recent.length;
    const avgTradeSize = recent.reduce((sum, r) => sum + r.tradeSizeSol, 0) / recent.length;
    const feeBurden = avgTradeSize > 0 ? avgFee / avgTradeSize : 0;

    // Route score
    const hc = this.config.route_health;
    const latencyScore = Math.max(0, 1 - avgLatency / hc.landing_latency_fail_ms);
    const retryScore = Math.max(0, 1 - retryRate / hc.retry_rate_fail);
    const congestionScore = Math.max(0, 1 - congestion);
    const routeScore = 0.3 * latencyScore + 0.3 * retryScore + 0.2 * congestionScore + 0.2;

    // Route EV adjustment
    let routeEvAdjustment = 0;
    if (mode === 'lightning') {
      routeEvAdjustment = 0.01 * routeScore - feeBurden * 0.5;
    }

    const stats: RouteHealthStats = {
      mode,
      avgLandingLatencyMs: avgLatency,
      recentRetryRate: retryRate,
      recentCongestion: congestion,
      feeBurden,
      freshnessS: ageS(lastExec.timestamp),
      lastUpdatedAt: now,
      routeScore: Math.max(0, Math.min(1, routeScore)),
      routeEvAdjustment,
      sampleCount: recent.length,
    };

    this.healthStats.set(mode, stats);

    // Auto-demote if route becomes unhealthy
    if (!this.isRouteHealthy(mode) && mode !== this.config.default_route_mode) {
      this.demote(`${mode} route unhealthy: latency=${avgLatency.toFixed(0)}ms, retry=${(retryRate * 100).toFixed(1)}%`);
    }
  }

  /** Default health stats for a route */
  private defaultHealthStats(mode: RouteMode): RouteHealthStats {
    return {
      mode,
      avgLandingLatencyMs: 0,
      recentRetryRate: 0,
      recentCongestion: 0,
      feeBurden: 0,
      freshnessS: 999,
      lastUpdatedAt: 0,
      routeScore: mode === 'local' ? 0.7 : 0.5, // Local gets higher default
      routeEvAdjustment: 0,
      sampleCount: 0,
    };
  }
}
