/**
 * @module features/bonding-curve-dynamics
 * Capital efficiency and bonding curve dynamics — primary graduation predictor per arXiv:2602.14860.
 *
 * v2 additions:
 *  - initial_capital_efficiency: CE from first 10-30 trades (paper's preferred early signal)
 *  - accumulation_shape: front-loaded vs linear vs accelerating (CE history halves comparison)
 *  - initial_burst_impact: mean trade impact ratio of first 10 trades (decisive early entrants)
 *  - high_impact_fraction: fraction of all trades with >5% curve impact (whale presence)
 *  - max_impact_ratio_normalized: largest single trade impact (conviction signal)
 *  - organic_diversity_score: CV of early trade impacts (0=bot-uniform, 1=organic)
 */

import { TradeDataPoint } from '../types/features';

export interface BondingCurveDynamicsFeatures {
  capital_efficiency_raw: number;
  capital_efficiency_normalized: number;
  window_capital_efficiency: number;
  efficiency_trend: number;
  curve_fill_rate_sol_per_min: number;
  curve_fill_rate_normalized: number;
  large_trade_fraction: number;
  median_trade_size_sol: number;
  median_trade_size_normalized: number;
  initial_capital_efficiency: number;
  accumulation_shape: number;
  initial_burst_impact: number;
  high_impact_fraction: number;
  max_impact_ratio_normalized: number;
  organic_diversity_score: number;
  bcd_score: number;
}

export interface BondingCurveDynamicsContext {
  vSolInBondingCurve: number;
  totalSwapCount: number;
  vSolAtFirstTrade: number;
  windowVSolAccumulated: number;
  windowSwapCount: number;
  windowDurationMs: number;
  largeTradeCount: number;
  capitalEfficiencyHistory: Array<{ timestamp: number; value: number }>;
  trades: TradeDataPoint[];
  // v2 fields
  firstTradesVSolSnapshot: number[];
  firstTradesCount: number;
  firstNTradeImpacts: number[];
  highImpactTradeCount: number;
  maxImpactRatio: number;
}

function clamp(x: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, x));
}

function arrayMean(xs: number[]): number {
  if (xs.length === 0) return 0;
  return xs.reduce((a, b) => a + b, 0) / xs.length;
}

function arrayStd(xs: number[], mu: number): number {
  if (xs.length < 2) return 0;
  const variance = xs.reduce((a, x) => a + (x - mu) ** 2, 0) / xs.length;
  return Math.sqrt(variance);
}

export function computeBondingCurveDynamics(ctx: BondingCurveDynamicsContext): BondingCurveDynamicsFeatures {
  const {
    vSolInBondingCurve, totalSwapCount, vSolAtFirstTrade,
    windowVSolAccumulated, windowSwapCount, windowDurationMs,
    largeTradeCount, capitalEfficiencyHistory, trades,
    firstTradesVSolSnapshot, firstTradesCount,
    firstNTradeImpacts, highImpactTradeCount, maxImpactRatio,
  } = ctx;

  // 1. Lifetime capital efficiency
  const capital_efficiency_raw = totalSwapCount > 0 ? vSolInBondingCurve / totalSwapCount : 0;
  const CE_SCALE = 0.10;
  const capital_efficiency_normalized = clamp(capital_efficiency_raw / CE_SCALE, 0, 1);

  // 2. Window efficiency
  const window_capital_efficiency = windowSwapCount > 0 ? windowVSolAccumulated / windowSwapCount : 0;

  // 3. Efficiency trend (linear regression over last 5 history points)
  let efficiency_trend = 0.5;
  if (capitalEfficiencyHistory.length >= 3) {
    const recent = capitalEfficiencyHistory.slice(-5);
    const n = recent.length;
    const tMean = recent.reduce((s, p) => s + p.timestamp, 0) / n;
    const vMean = recent.reduce((s, p) => s + p.value, 0) / n;
    const cov = recent.reduce((s, p) => s + (p.timestamp - tMean) * (p.value - vMean), 0) / n;
    const varT = recent.reduce((s, p) => s + (p.timestamp - tMean) ** 2, 0) / n;
    const slope = varT > 0 ? cov / varT : 0;
    const slopePerMin = slope * 60000;
    efficiency_trend = clamp(slopePerMin / 0.05 + 0.5, 0, 1);
  }

  // 4. Curve fill rate
  const windowDurationMin = windowDurationMs / 60000;
  const curve_fill_rate_sol_per_min = windowDurationMin > 0 ? windowVSolAccumulated / windowDurationMin : 0;
  const curve_fill_rate_normalized = clamp(curve_fill_rate_sol_per_min / 10.0, 0, 1);

  // 5. Large trade fraction
  const large_trade_fraction = totalSwapCount > 0 ? clamp(largeTradeCount / totalSwapCount, 0, 1) : 0;

  // 6. Median trade size (approximated as mean of recent buys)
  const recentBuys = trades.filter(t => t.txType === 'buy').slice(-20);
  const median_trade_size_sol = arrayMean(recentBuys.map(t => t.solAmount));
  const median_trade_size_normalized = clamp(median_trade_size_sol / 0.05, 0, 1);

  // 7. NEW: Initial capital efficiency (first 10-30 trades)
  let initial_capital_efficiency: number;
  if (firstTradesCount < 5) {
    initial_capital_efficiency = 0.5; // insufficient data — neutral
  } else {
    const vSolAtTrade10 = firstTradesVSolSnapshot[9] ?? firstTradesVSolSnapshot[firstTradesCount - 1] ?? 0;
    const initial_ce_10_raw = firstTradesCount >= 10 ? (vSolAtTrade10 - vSolAtFirstTrade) / 10 : 0;
    const initial_ce_10_norm = clamp(initial_ce_10_raw / 0.10, 0, 1);

    const vSolAtTrade30 = firstTradesVSolSnapshot[29] ?? firstTradesVSolSnapshot[firstTradesCount - 1] ?? 0;
    const initial_ce_30_raw = (vSolAtTrade30 - vSolAtFirstTrade) / Math.max(firstTradesCount, 1);
    const initial_ce_30_norm = clamp(initial_ce_30_raw / 0.10, 0, 1);

    // Weight 10-trade window higher — more selective signal
    initial_capital_efficiency = 0.6 * initial_ce_10_norm + 0.4 * initial_ce_30_norm;
  }

  // 8. NEW: Accumulation shape (CE history first half vs second half)
  let accumulation_shape = 0.5; // neutral default
  if (capitalEfficiencyHistory.length >= 4) {
    const hist = capitalEfficiencyHistory;
    const mid = Math.floor(hist.length / 2);
    const earlyMeanCE = arrayMean(hist.slice(0, mid).map(h => h.value));
    const lateMeanCE = arrayMean(hist.slice(mid).map(h => h.value));
    // lateCE > earlyCE → accelerating (> 0.5); lateCE < earlyCE → front-loaded (< 0.5)
    const ratio = (lateMeanCE + 0.001) / (earlyMeanCE + 0.001);
    accumulation_shape = clamp(ratio / 2.0, 0, 1);
  }

  // 9. NEW: Initial burst impact (mean impact ratio of first 10 trades)
  const first10Impacts = firstNTradeImpacts.slice(0, 10);
  const initial_burst_impact = first10Impacts.length > 0
    ? clamp(arrayMean(first10Impacts) / 0.10, 0, 1)
    : 0;

  // 10. NEW: High impact fraction
  const high_impact_fraction = clamp(highImpactTradeCount / Math.max(totalSwapCount, 1), 0, 1);

  // 11. NEW: Max impact ratio normalized
  const max_impact_ratio_normalized = clamp(maxImpactRatio / 0.20, 0, 1);

  // 12. NEW: Organic diversity score (CV of firstNTradeImpacts)
  let organic_diversity_score = 0.5; // neutral when insufficient data
  if (firstNTradeImpacts.length >= 2) {
    const impactMean = arrayMean(firstNTradeImpacts);
    const impactStd = arrayStd(firstNTradeImpacts, impactMean);
    const cv = impactMean > 0.001 ? impactStd / impactMean : 2.0; // high CV default when mean near 0
    organic_diversity_score = clamp(cv / 2.0, 0, 1);
  }

  // 13. Updated composite BCD score
  const bcd_score = clamp(
    0.25 * capital_efficiency_normalized +
    0.20 * initial_capital_efficiency +
    0.15 * efficiency_trend +
    0.12 * accumulation_shape +
    0.10 * initial_burst_impact +
    0.08 * curve_fill_rate_normalized +
    0.05 * high_impact_fraction +
    0.03 * organic_diversity_score +
    0.02 * large_trade_fraction,
    0, 1
  );

  return {
    capital_efficiency_raw,
    capital_efficiency_normalized,
    window_capital_efficiency,
    efficiency_trend,
    curve_fill_rate_sol_per_min,
    curve_fill_rate_normalized,
    large_trade_fraction,
    median_trade_size_sol,
    median_trade_size_normalized,
    initial_capital_efficiency,
    accumulation_shape,
    initial_burst_impact,
    high_impact_fraction,
    max_impact_ratio_normalized,
    organic_diversity_score,
    bcd_score,
  };
}
