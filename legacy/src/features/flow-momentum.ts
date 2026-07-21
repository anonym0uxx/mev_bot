/**
 * @module features/flow-momentum
 * Feature family 1: Flow/momentum computation.
 * Buy notional velocity, trade-count velocity, acceleration, imbalance,
 * avg trade size, size dispersion — all computed over rolling windows.
 */

import { FlowMomentumFeatures, TradeDataPoint } from '../types/features';

/**
 * Compute flow/momentum features from trade data across windows.
 *
 * @param trades - Recent trade data points
 * @param windows - Window sizes in seconds [1, 5, 15, 30]
 * @param now - Current timestamp in ms
 * @param prevVelocities - Previous velocity values for acceleration
 */
export function computeFlowMomentum(
  trades: TradeDataPoint[],
  windows: number[],
  now: number,
  prevVelocities: Map<string, number>
): FlowMomentumFeatures {
  const windowedTrades = new Map<number, TradeDataPoint[]>();

  for (const w of windows) {
    const cutoff = now - w * 1000;
    windowedTrades.set(w, trades.filter(t => t.timestamp >= cutoff));
  }

  // Buy notional velocity: total buy SOL volume / window seconds
  const buyNotionalVelocity = (w: number): number => {
    const wTrades = windowedTrades.get(w) || [];
    const totalBuySol = wTrades
      .filter(t => t.txType === 'buy')
      .reduce((sum, t) => sum + t.solAmount, 0);
    return w > 0 ? totalBuySol / w : 0;
  };

  // Trade count velocity: total trades / window seconds
  const tradeCountVelocity = (w: number): number => {
    const wTrades = windowedTrades.get(w) || [];
    return w > 0 ? wTrades.length / w : 0;
  };

  // Buy velocity acceleration: (current velocity - previous velocity) / window
  const buyVelAccel = (w: number): number => {
    const current = buyNotionalVelocity(w);
    const prev = prevVelocities.get(`buy_velocity_${w}s`) || 0;
    return w > 0 ? (current - prev) / w : 0;
  };

  // Curve progress acceleration: rate of change of bonding curve progress
  const curveProgressAccel = (w: number): number => {
    const wTrades = windowedTrades.get(w) || [];
    if (wTrades.length < 2) return 0;
    const first = wTrades[0];
    const last = wTrades[wTrades.length - 1];
    const timeDiff = (last.timestamp - first.timestamp) / 1000;
    if (timeDiff <= 0) return 0;

    // Progress approximated from vTokensInBondingCurve change
    const initialVTokens = 1_073_000_000;
    const progressFirst = 1 - (first.vTokensInBondingCurve / initialVTokens);
    const progressLast = 1 - (last.vTokensInBondingCurve / initialVTokens);
    return (progressLast - progressFirst) / timeDiff;
  };

  // Buy/sell imbalance: (buy_vol - sell_vol) / (buy_vol + sell_vol)
  const buySellImbalance = (w: number): number => {
    const wTrades = windowedTrades.get(w) || [];
    let buyVol = 0;
    let sellVol = 0;
    for (const t of wTrades) {
      if (t.txType === 'buy') buyVol += t.solAmount;
      else sellVol += t.solAmount;
    }
    const total = buyVol + sellVol;
    return total > 0 ? (buyVol - sellVol) / total : 0;
  };

  // Average trade size (SOL) over window
  const avgTradeSize = (w: number): number => {
    const wTrades = windowedTrades.get(w) || [];
    if (wTrades.length === 0) return 0;
    const totalSol = wTrades.reduce((sum, t) => sum + t.solAmount, 0);
    return totalSol / wTrades.length;
  };

  // Size dispersion: coefficient of variation of trade sizes
  const sizeDispersion = (w: number): number => {
    const wTrades = windowedTrades.get(w) || [];
    if (wTrades.length < 2) return 0;
    const sizes = wTrades.map(t => t.solAmount);
    const mean = sizes.reduce((a, b) => a + b, 0) / sizes.length;
    if (mean === 0) return 0;
    const variance = sizes.reduce((sum, s) => sum + Math.pow(s - mean, 2), 0) / sizes.length;
    return Math.sqrt(variance) / mean; // CV
  };

  return {
    buy_notional_velocity_1s: buyNotionalVelocity(1),
    buy_notional_velocity_5s: buyNotionalVelocity(5),
    buy_notional_velocity_15s: buyNotionalVelocity(15),
    buy_notional_velocity_30s: buyNotionalVelocity(30),
    trade_count_velocity_1s: tradeCountVelocity(1),
    trade_count_velocity_5s: tradeCountVelocity(5),
    trade_count_velocity_15s: tradeCountVelocity(15),
    trade_count_velocity_30s: tradeCountVelocity(30),
    buy_velocity_acceleration_5s: buyVelAccel(5),
    buy_velocity_acceleration_15s: buyVelAccel(15),
    curve_progress_acceleration_5s: curveProgressAccel(5),
    curve_progress_acceleration_15s: curveProgressAccel(15),
    buy_sell_imbalance_5s: buySellImbalance(5),
    buy_sell_imbalance_15s: buySellImbalance(15),
    buy_sell_imbalance_30s: buySellImbalance(30),
    avg_trade_size_5s: avgTradeSize(5),
    avg_trade_size_15s: avgTradeSize(15),
    size_dispersion_5s: sizeDispersion(5),
    size_dispersion_15s: sizeDispersion(15),
  };
}
