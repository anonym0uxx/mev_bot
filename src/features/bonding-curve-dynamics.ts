/**
 * @module features/bonding-curve-dynamics
 * Capital efficiency and bonding curve dynamics — the primary graduation predictor
 * per arXiv:2602.14860. Measures SOL accumulation quality, not just quantity.
 *
 * Key insight: vSolInBondingCurve / totalSwapCount (capital efficiency) is the #1 predictor
 * of graduation. A token accumulating SOL with few decisive trades (high CE) vastly
 * outperforms bot-washed tokens that require thousands of swaps to accumulate the same SOL.
 */

import { TradeDataPoint } from '../types/features';

export interface BondingCurveDynamicsFeatures {
  // Primary signal: vSol accumulated per swap (higher = fewer decisive trades = better)
  capital_efficiency_raw: number;        // vSolInBondingCurve / totalSwapCount
  capital_efficiency_normalized: number; // clamp(raw / CE_SCALE, 0, 1) → [0,1] higher=better

  // Window efficiency: recent accumulation quality
  window_capital_efficiency: number;     // windowVSolAccumulated / windowSwapCount

  // Trend: is efficiency improving (more SOL/trade over time) or degrading?
  efficiency_trend: number;              // [0,1] where 1=strongly improving

  // Fill rate: SOL per minute of curve fill (absolute velocity)
  curve_fill_rate_sol_per_min: number;   // raw
  curve_fill_rate_normalized: number;    // clamp(raw / 10.0, 0, 1)

  // Large trade presence: fraction of trades >= 0.10 SOL
  large_trade_fraction: number;          // largeTradeCount / totalSwapCount, [0,1]

  // Median trade size (approximated as mean for simplicity)
  median_trade_size_sol: number;         // approximated from recent trades
  median_trade_size_normalized: number;  // clamp(value / 0.05, 0, 1)

  // Composite bonding curve dynamics score [0,1]
  bcd_score: number;
}

export interface BondingCurveDynamicsContext {
  vSolInBondingCurve: number;        // current value from latest trade
  totalSwapCount: number;            // lifetime swap count
  vSolAtFirstTrade: number;          // vSol when first trade observed
  windowVSolAccumulated: number;     // SOL accumulated in observation window
  windowSwapCount: number;           // swaps in observation window
  windowDurationMs: number;          // duration of observation window in ms
  largeTradeCount: number;           // cumulative large trade count
  capitalEfficiencyHistory: Array<{ timestamp: number; value: number }>; // for trend
  trades: TradeDataPoint[];          // recent trades (for median approx)
}

export function computeBondingCurveDynamics(ctx: BondingCurveDynamicsContext): BondingCurveDynamicsFeatures {
  const {
    vSolInBondingCurve, totalSwapCount,
    windowVSolAccumulated, windowSwapCount, windowDurationMs,
    largeTradeCount, capitalEfficiencyHistory, trades,
  } = ctx;

  // --- Capital Efficiency (primary signal) ---
  // raw CE = vSol / swaps. A graduating token with 10 swaps and 1 SOL has CE=0.1 (good).
  // A bot-washed token with 1000 swaps and 1 SOL has CE=0.001 (bad).
  // HIGHER raw CE = better quality accumulation.
  const capital_efficiency_raw = totalSwapCount > 0
    ? vSolInBondingCurve / totalSwapCount
    : 0;

  // CE_SCALE = 0.10 means "0.1 SOL per swap = maximum score"
  const CE_SCALE = 0.10;
  const capital_efficiency_normalized = Math.min(1, Math.max(0, capital_efficiency_raw / CE_SCALE));

  // --- Window Efficiency ---
  const window_capital_efficiency = windowSwapCount > 0
    ? windowVSolAccumulated / windowSwapCount
    : 0;

  // --- Efficiency Trend (linear regression over last 5 history points) ---
  let efficiency_trend = 0.5; // neutral default
  if (capitalEfficiencyHistory.length >= 3) {
    const recent = capitalEfficiencyHistory.slice(-5);
    const n = recent.length;
    const tMean = recent.reduce((s, p) => s + p.timestamp, 0) / n;
    const vMean = recent.reduce((s, p) => s + p.value, 0) / n;
    const cov = recent.reduce((s, p) => s + (p.timestamp - tMean) * (p.value - vMean), 0) / n;
    const varT = recent.reduce((s, p) => s + Math.pow(p.timestamp - tMean, 2), 0) / n;
    const slope = varT > 0 ? cov / varT : 0;
    // slope is CE change per ms. slopePerMin = CE change per minute.
    // +0.05 CE/min improvement → normalized = 1.0 (excellent trend)
    const slopePerMin = slope * 60000;
    efficiency_trend = Math.min(1, Math.max(0, slopePerMin / 0.05 + 0.5));
  }

  // --- Curve Fill Rate ---
  const windowDurationMin = windowDurationMs / 60000;
  const curve_fill_rate_sol_per_min = windowDurationMin > 0
    ? windowVSolAccumulated / windowDurationMin
    : 0;
  const FILL_RATE_SCALE = 10.0; // 10 SOL/min = max score
  const curve_fill_rate_normalized = Math.min(1, Math.max(0, curve_fill_rate_sol_per_min / FILL_RATE_SCALE));

  // --- Large Trade Fraction ---
  const large_trade_fraction = totalSwapCount > 0
    ? Math.min(1, largeTradeCount / totalSwapCount)
    : 0;

  // --- Median Trade Size (approximate as mean of recent buys) ---
  const recentBuys = trades.filter(t => t.txType === 'buy').slice(-20);
  const median_trade_size_sol = recentBuys.length > 0
    ? recentBuys.reduce((s, t) => s + t.solAmount, 0) / recentBuys.length
    : 0;
  const median_trade_size_normalized = Math.min(1, Math.max(0, median_trade_size_sol / 0.05));

  // --- Composite BCD Score ---
  // Weights: capital_efficiency dominates (40%), trend secondary (25%), fill rate (20%),
  // large_trade (10%), median_size (5%).
  const bcd_score = Math.min(1, Math.max(0,
    0.40 * capital_efficiency_normalized +
    0.25 * efficiency_trend +
    0.20 * curve_fill_rate_normalized +
    0.10 * large_trade_fraction +
    0.05 * median_trade_size_normalized,
  ));

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
    bcd_score,
  };
}
