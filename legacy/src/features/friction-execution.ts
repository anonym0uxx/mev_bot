/**
 * @module features/friction-execution
 * Feature family 4: Friction/execution features.
 * Entry/exit slippage, route mode, priority-fee burden, landing-risk,
 * retry/failure rate, execution freshness, route score, route_ev_adjustment,
 * route-health prior, latency-budget utilization.
 */

import { FrictionExecutionFeatures } from '../types/features';
import { RouteHealthConfig } from '../types/config';

/** Context for friction computation */
export interface FrictionContext {
  expectedEntrySlippage: number;
  expectedExitSlippage: number;
  routeMode: string;
  priorityFeeSol: number;
  landingRisk: number;
  retryFailureRate: number;
  executionFreshnessS: number;
  latencyBudgetMs: number;
  actualLatencyMs: number;
  routeHealthLandingMs: number;
  routeHealthRetryRate: number;
  routeHealthCongestion: number;
}

/**
 * Compute friction/execution features from current context and route health config.
 */
export function computeFrictionExecution(
  ctx: FrictionContext,
  healthConfig: RouteHealthConfig
): FrictionExecutionFeatures {
  // Route score: [0,1] composite health of current execution route
  // Higher = better execution quality
  const landingScore = computeLandingScore(ctx.routeHealthLandingMs, healthConfig);
  const retryScore = computeRetryScore(ctx.routeHealthRetryRate, healthConfig);
  const congestionScore = 1 - Math.min(1, ctx.routeHealthCongestion / healthConfig.congestion_threshold);
  const freshnessScore = computeFreshnessScore(ctx.executionFreshnessS, healthConfig);

  const routeScore = Math.max(0, Math.min(1,
    0.30 * landingScore +
    0.25 * retryScore +
    0.25 * congestionScore +
    0.20 * freshnessScore
  ));

  // Route EV adjustment: cost/benefit adjustment based on route choice
  // Lightning: higher fee but faster landing → less opportunity cost
  // Local: lower fee but higher slippage risk
  let routeEvAdjustment = 0;
  if (ctx.routeMode === 'lightning') {
    // Lightning saves opportunity cost but adds fee
    const opportunitySaved = 0.01; // Estimated 1% better fill from speed
    const extraFee = 0.005; // PumpPortal lightning premium
    routeEvAdjustment = opportunitySaved - extraFee;
  } else if (ctx.routeMode === 'jito') {
    routeEvAdjustment = -0.002; // Jito bundle tip cost, only for multi-tx atomicity
  }

  // Route health prior: overall execution health indicator [0,1]
  const routeHealthPrior = routeScore;

  // Priority fee burden: fee as fraction of typical trade size
  const typicalTradeSol = 0.05; // Approximate for normalization
  const priorityFeeBurden = typicalTradeSol > 0
    ? ctx.priorityFeeSol / typicalTradeSol
    : 0;

  // Landing risk estimate: probability of tx not landing
  const landingRiskEstimate = Math.min(1, Math.max(0,
    0.4 * ctx.retryFailureRate +
    0.3 * (ctx.routeHealthLandingMs / healthConfig.landing_latency_fail_ms) +
    0.3 * ctx.routeHealthCongestion
  ));

  // Latency budget utilization: what fraction of budget is consumed
  const latencyBudgetUtilization = ctx.latencyBudgetMs > 0
    ? Math.min(1, ctx.actualLatencyMs / ctx.latencyBudgetMs)
    : 0;

  return {
    expected_entry_slippage: ctx.expectedEntrySlippage,
    expected_exit_slippage: ctx.expectedExitSlippage,
    route_mode: ctx.routeMode,
    priority_fee_burden: priorityFeeBurden,
    landing_risk_estimate: landingRiskEstimate,
    retry_failure_rate: ctx.retryFailureRate,
    execution_freshness_s: ctx.executionFreshnessS,
    route_score: routeScore,
    route_ev_adjustment: routeEvAdjustment,
    route_health_prior: routeHealthPrior,
    latency_budget_utilization: latencyBudgetUtilization,
  };
}

function computeLandingScore(landingMs: number, config: RouteHealthConfig): number {
  if (landingMs <= 0) return 1;
  if (landingMs >= config.landing_latency_fail_ms) return 0;
  if (landingMs >= config.landing_latency_warn_ms) {
    return 1 - (landingMs - config.landing_latency_warn_ms) /
      (config.landing_latency_fail_ms - config.landing_latency_warn_ms);
  }
  return 1;
}

function computeRetryScore(retryRate: number, config: RouteHealthConfig): number {
  if (retryRate >= config.retry_rate_fail) return 0;
  if (retryRate >= config.retry_rate_warn) {
    return 1 - (retryRate - config.retry_rate_warn) /
      (config.retry_rate_fail - config.retry_rate_warn);
  }
  return 1;
}

function computeFreshnessScore(freshnessS: number, config: RouteHealthConfig): number {
  if (freshnessS >= config.freshness_max_s) return 0;
  return 1 - (freshnessS / config.freshness_max_s);
}
