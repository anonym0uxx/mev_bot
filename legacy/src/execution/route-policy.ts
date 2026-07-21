/**
 * @module execution/route-policy
 * MEV-aware route scoring, promotion/demotion per spec section 13.
 *
 * Route classes:
 *   LOCAL     — default Solana public RPC or PumpPortal API
 *   LIGHTNING — PumpPortal Lightning (higher priority fee, faster landing)
 *   PRIVATE   — Jito-protected private submission (MEV-shielded)
 *   BUNDLE    — Jito bundle (true multi-tx atomicity only)
 *
 * Execution policy:
 * - Route selection maximizes expected net value after fees, latency, failure risk
 * - LOCAL remains default where appropriate
 * - LIGHTNING promotion only when opportunity half-life < threshold AND extra fee justified
 * - PRIVATE used where private landing materially improves exit quality
 * - BUNDLE only for true atomic/multi-step cases, never blindly
 * - Route-specific health priors measured and used in scoring
 * - Forced exits NEVER wait for deep-lane enrichment or supervisory reasoning
 */

import { createLogger } from '../utils/logger';
import { nowMs, ageS } from '../utils/time';
import { RouteMode, ExecutionConfig } from '../types/config';

const log = createLogger('route-policy');

/** Route health statistics */
export interface RouteHealthStats {
  mode: RouteMode;
  avgLandingLatencyMs: number;
  avgConfirmLatencyMs: number;
  recentFailureRate: number;
  recentRetryRate: number;
  recentCongestion: number;
  feeBurden: number;
  freshnessS: number;
  lastUpdatedAt: number;
  routeScore: number;
  routeEvAdjustment: number;
  sampleCount: number;
  /** Realized slippage vs expected slippage ratio (1.0 = exactly as expected) */
  slippageAccuracy: number;
}

/** Route execution record for health tracking */
export interface RouteExecutionRecord {
  mode: RouteMode;
  success: boolean;
  landingLatencyMs: number;
  confirmLatencyMs?: number;
  retried: boolean;
  feePaid: number;
  tradeSizeSol: number;
  expectedSlippagePct?: number;
  realizedSlippagePct?: number;
  timestamp: number;
  isForcedExit?: boolean;
}

/** Route selection context passed by the caller */
export interface RouteSelectionContext {
  entryEdge: number;
  opportunityHalfLifeS: number;
  tradeSizeSol: number;
  side: 'buy' | 'sell';
  isForcedExit: boolean;
  currentSlippagePct: number;
  /** Time pressure: 0 = no pressure, 1 = extreme */
  timePressure: number;
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

  updateConfig(config: ExecutionConfig): void {
    this.config = config;
  }

  /**
   * Select the best route for a trade given current conditions.
   * Returns the route that maximizes expected net value after all costs.
   */
  selectRoute(ctx: RouteSelectionContext): RouteMode {
    // Forced exits: use fastest healthy route, never wait
    if (ctx.isForcedExit) {
      return this.selectForcedExitRoute(ctx);
    }

    // If route promotion disabled, always default
    if (!this.config.route_promotion.enabled) {
      return this.config.default_route_mode;
    }

    // Score all available routes
    const candidates = this.scoreAllRoutes(ctx);

    // Pick highest EV route
    if (candidates.length === 0) return this.config.default_route_mode;
    candidates.sort((a, b) => b.expectedNetEv - a.expectedNetEv);
    const best = candidates[0];

    if (best.mode !== this.config.default_route_mode) {
      log.info(`Route selected: ${best.mode} (EV=${best.expectedNetEv.toFixed(6)}) over ${this.config.default_route_mode}`);
      this.lastPromotionAt = nowMs();
    }

    return best.mode;
  }

  /**
   * Select route for forced exit — prioritize speed and reliability.
   */
  private selectForcedExitRoute(ctx: RouteSelectionContext): RouteMode {
    // If private route is enabled and exit slippage is high, use private
    if (this.config.private_route?.enabled &&
        ctx.currentSlippagePct > (this.config.private_route.exit_slippage_trigger_pct || 0.10)) {
      if (this.isRouteHealthy('private')) {
        log.info(`Forced exit using PRIVATE route (slippage=${(ctx.currentSlippagePct * 100).toFixed(1)}%)`);
        return 'private';
      }
    }

    // If lightning is healthy and fast, use it for forced exits
    if (this.isRouteHealthy('lightning')) {
      const lightningStats = this.healthStats.get('lightning');
      const localStats = this.healthStats.get('local');
      if (lightningStats && localStats &&
          lightningStats.avgLandingLatencyMs < localStats.avgLandingLatencyMs * 0.7) {
        return 'lightning';
      }
    }

    return this.config.default_route_mode;
  }

  /**
   * Score all available routes by expected net value.
   */
  private scoreAllRoutes(ctx: RouteSelectionContext): Array<{ mode: RouteMode; expectedNetEv: number }> {
    const routes: Array<{ mode: RouteMode; expectedNetEv: number }> = [];
    const promo = this.config.route_promotion;

    // LOCAL — always available
    routes.push({
      mode: 'local',
      expectedNetEv: this.computeRouteEV('local', ctx),
    });

    // LIGHTNING — only when conditions met
    if (ctx.opportunityHalfLifeS < promo.opportunity_half_life_threshold_s &&
        ctx.entryEdge > promo.min_edge_for_lightning &&
        this.isRouteHealthy('lightning') &&
        (nowMs() - this.lastDemotionAt > promo.demotion_cooldown_s * 1000)) {
      routes.push({
        mode: 'lightning',
        expectedNetEv: this.computeRouteEV('lightning', ctx),
      });
    }

    // PRIVATE — for exits with high slippage, or buys under MEV pressure
    if (this.config.private_route?.enabled && this.isRouteHealthy('private')) {
      const minEdge = this.config.private_route.min_edge_for_private;
      if (ctx.entryEdge > minEdge || ctx.side === 'sell') {
        routes.push({
          mode: 'private',
          expectedNetEv: this.computeRouteEV('private', ctx),
        });
      }
    }

    // BUNDLE — only for explicit atomic cases (not scored here in normal flow)
    // Bundles are never auto-promoted; they require explicit use

    return routes;
  }

  /**
   * Compute expected net value for a route given context.
   * EV = expected_gross - route_fees - expected_slippage_adjustment - latency_cost - failure_risk_cost
   */
  private computeRouteEV(mode: RouteMode, ctx: RouteSelectionContext): number {
    const stats = this.healthStats.get(mode) || this.defaultHealthStats(mode);
    const expectedGross = ctx.entryEdge * ctx.tradeSizeSol;

    // Fee cost
    let feeCost = 0;
    if (mode === 'lightning') {
      feeCost = this.config.default_priority_fee_sol * 2; // Higher priority
    } else if (mode === 'private') {
      const tipLamports = this.config.private_route?.jito_tip_lamports || 10000;
      feeCost = tipLamports / 1e9; // Convert lamports to SOL
    }

    // Slippage adjustment: private route may reduce sandwiching
    let slippageAdj = 0;
    if (mode === 'private') {
      // Private landing reduces MEV extraction by ~50-80%
      slippageAdj = ctx.currentSlippagePct * ctx.tradeSizeSol * 0.5;
    }

    // Latency cost: faster routes preserve more edge
    const latencyDecay = stats.avgLandingLatencyMs / 1000 * 0.001 * ctx.tradeSizeSol;

    // Failure risk cost
    const failureCost = stats.recentFailureRate * ctx.tradeSizeSol * 0.05;

    return expectedGross - feeCost + slippageAdj - latencyDecay - failureCost;
  }

  /**
   * Record an execution result for health tracking.
   */
  recordExecution(record: RouteExecutionRecord): void {
    this.executionHistory.push(record);

    // Prune old history (keep last 200)
    if (this.executionHistory.length > 200) {
      this.executionHistory = this.executionHistory.slice(-200);
    }

    this.updateHealthStats(record.mode);
  }

  getHealthStats(mode: RouteMode): RouteHealthStats {
    return this.healthStats.get(mode) || this.defaultHealthStats(mode);
  }

  getAllHealthStats(): Map<RouteMode, RouteHealthStats> {
    return new Map(this.healthStats);
  }

  isRouteHealthy(mode: RouteMode): boolean {
    const stats = this.healthStats.get(mode);
    if (!stats || stats.sampleCount === 0) {
      // No data — local is assumed healthy, others are not
      return mode === 'local';
    }

    const hc = this.config.route_health;
    return (
      stats.avgLandingLatencyMs < hc.landing_latency_fail_ms &&
      stats.recentFailureRate < hc.retry_rate_fail &&
      stats.recentCongestion < hc.congestion_threshold &&
      stats.freshnessS < hc.freshness_max_s
    );
  }

  demote(reason: string): void {
    this.lastDemotionAt = nowMs();
    log.info(`Route demoted back to ${this.config.default_route_mode}: ${reason}`);
  }

  // ====== HEALTH STATS ======

  private initHealthStats(): void {
    for (const mode of ['local', 'lightning', 'private', 'jito'] as RouteMode[]) {
      this.healthStats.set(mode, this.defaultHealthStats(mode));
    }
  }

  private updateHealthStats(mode: RouteMode): void {
    const windowMs = 120_000; // 2 min window
    const now = nowMs();
    const recent = this.executionHistory.filter(
      r => r.mode === mode && r.timestamp > now - windowMs
    );

    if (recent.length === 0) {
      const existing = this.healthStats.get(mode) || this.defaultHealthStats(mode);
      existing.freshnessS = ageS(existing.lastUpdatedAt);
      this.healthStats.set(mode, existing);
      return;
    }

    const avgLanding = recent.reduce((s, r) => s + r.landingLatencyMs, 0) / recent.length;
    const avgConfirm = recent.filter(r => r.confirmLatencyMs != null)
      .reduce((s, r) => s + (r.confirmLatencyMs || 0), 0) /
      Math.max(1, recent.filter(r => r.confirmLatencyMs != null).length);

    const failureRate = recent.filter(r => !r.success).length / recent.length;
    const retryRate = recent.filter(r => r.retried).length / recent.length;

    const slowThreshold = this.config.route_health.landing_latency_warn_ms;
    const congestion = recent.filter(r => r.landingLatencyMs > slowThreshold).length / recent.length;

    const avgFee = recent.reduce((s, r) => s + r.feePaid, 0) / recent.length;
    const avgSize = recent.reduce((s, r) => s + r.tradeSizeSol, 0) / recent.length;
    const feeBurden = avgSize > 0 ? avgFee / avgSize : 0;

    // Slippage accuracy (realized vs expected)
    const slippageRecords = recent.filter(r =>
      r.expectedSlippagePct != null && r.realizedSlippagePct != null && r.expectedSlippagePct > 0
    );
    const slippageAccuracy = slippageRecords.length > 0
      ? slippageRecords.reduce((s, r) => s + (r.realizedSlippagePct! / r.expectedSlippagePct!), 0) / slippageRecords.length
      : 1.0;

    // Composite score
    const hc = this.config.route_health;
    const latencyScore = Math.max(0, 1 - avgLanding / hc.landing_latency_fail_ms);
    const reliabilityScore = Math.max(0, 1 - failureRate / hc.retry_rate_fail);
    const congestionScore = Math.max(0, 1 - congestion);
    const slippageScore = Math.max(0, 1 - Math.abs(slippageAccuracy - 1));

    const routeScore = 0.25 * latencyScore + 0.25 * reliabilityScore +
                       0.20 * congestionScore + 0.15 * slippageScore + 0.15;

    // Route EV adjustment
    let routeEvAdjustment = 0;
    if (mode === 'lightning') {
      routeEvAdjustment = 0.01 * routeScore - feeBurden * 0.5;
    } else if (mode === 'private') {
      // Private routes reduce MEV extraction
      routeEvAdjustment = 0.005 * routeScore + 0.002; // Small positive for MEV protection
    }

    const stats: RouteHealthStats = {
      mode,
      avgLandingLatencyMs: avgLanding,
      avgConfirmLatencyMs: avgConfirm,
      recentFailureRate: failureRate,
      recentRetryRate: retryRate,
      recentCongestion: congestion,
      feeBurden,
      freshnessS: 0,
      lastUpdatedAt: now,
      routeScore: Math.max(0, Math.min(1, routeScore)),
      routeEvAdjustment,
      sampleCount: recent.length,
      slippageAccuracy,
    };

    this.healthStats.set(mode, stats);

    // Auto-demote if non-default route becomes unhealthy
    if (!this.isRouteHealthy(mode) && mode !== this.config.default_route_mode) {
      this.demote(`${mode} unhealthy: latency=${avgLanding.toFixed(0)}ms fail=${(failureRate * 100).toFixed(1)}%`);
    }
  }

  private defaultHealthStats(mode: RouteMode): RouteHealthStats {
    return {
      mode,
      avgLandingLatencyMs: 0,
      avgConfirmLatencyMs: 0,
      recentFailureRate: 0,
      recentRetryRate: 0,
      recentCongestion: 0,
      feeBurden: 0,
      freshnessS: 999,
      lastUpdatedAt: 0,
      routeScore: mode === 'local' ? 0.7 : 0.5,
      routeEvAdjustment: 0,
      sampleCount: 0,
      slippageAccuracy: 1.0,
    };
  }
}
