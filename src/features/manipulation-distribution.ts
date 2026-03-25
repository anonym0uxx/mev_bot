/**
 * @module features/manipulation-distribution
 * Feature family 5: Manipulation/distribution detection.
 * Creator sell, same-size prints, price-breadth divergence,
 * concentration worsening, cluster correlation, suspicious burst,
 * slippage shock, distribution signatures.
 */

import { ManipulationDistributionFeatures, TradeDataPoint } from '../types/features';
import { ManipulationConfig } from '../types/config';

/** Context for manipulation detection */
export interface ManipulationContext {
  trades: TradeDataPoint[];
  creator: string;
  walletBalances: Map<string, number>;
  uniqueBuyers: Set<string>;
  windows: number[];
  now: number;
}

/**
 * Compute manipulation/distribution features.
 */
export function computeManipulationDistribution(
  ctx: ManipulationContext,
  config: ManipulationConfig
): ManipulationDistributionFeatures {
  const { trades, creator, walletBalances, uniqueBuyers, now } = ctx;

  // 1. Creator sell detection
  const creatorSell = detectCreatorSell(trades, creator);

  // 2. Same-size prints detection
  const sameSizePrintCount = detectSameSizePrints(
    trades, config.same_size_print_window_s, config.same_size_tolerance_pct, now
  );

  // 3. Price up / breadth flat divergence
  const priceBreadthDivergence = detectPriceBreadthDivergence(trades, uniqueBuyers, now);

  // 4. Concentration worsening
  const concentrationWorsening = detectConcentrationWorsening(trades, walletBalances);

  // 5. Cluster correlation
  const clusterCorrelation = detectClusterCorrelation(trades, now);

  // 6. Suspicious burst behavior
  const suspiciousBurst = detectSuspiciousBurst(trades, now);

  // 7. Slippage shock without healthy breadth
  const slippageShock = detectSlippageShock(trades, uniqueBuyers, now);

  // 8. Distribution event signatures
  const distributionSignatures = detectDistributionSignatures(trades, walletBalances, creator);

  // Hard shock: any single critical condition met
  const hardShock =
    (config.creator_sell_instant_exit && creatorSell) ||
    sameSizePrintCount >= config.same_size_print_min_count ||
    priceBreadthDivergence >= config.price_breadth_divergence_threshold ||
    concentrationWorsening >= config.concentration_worsening_threshold ||
    clusterCorrelation >= config.cluster_correlation_threshold ||
    slippageShock >= config.slippage_shock_threshold;

  // Continuous manipulation penalty [0,1]
  const weights = config.continuous_penalty_weights;
  const rawPenalty =
    (creatorSell ? weights.creator_sell : 0) +
    weights.same_size_prints * Math.min(1, sameSizePrintCount / config.same_size_print_min_count) +
    weights.price_breadth_divergence * Math.min(1, priceBreadthDivergence / config.price_breadth_divergence_threshold) +
    weights.concentration_worsening * Math.min(1, concentrationWorsening / config.concentration_worsening_threshold) +
    weights.cluster_correlation * Math.min(1, clusterCorrelation / config.cluster_correlation_threshold) +
    weights.suspicious_burst * Math.min(1, suspiciousBurst / config.suspicious_burst_threshold) +
    weights.slippage_shock * Math.min(1, slippageShock / config.slippage_shock_threshold);

  const maxPossiblePenalty =
    weights.creator_sell + weights.same_size_prints + weights.price_breadth_divergence +
    weights.concentration_worsening + weights.cluster_correlation +
    weights.suspicious_burst + weights.slippage_shock;

  const manipulationPenalty = maxPossiblePenalty > 0
    ? Math.min(1, Math.max(0, rawPenalty / maxPossiblePenalty))
    : 0;

  return {
    creator_sell: creatorSell,
    same_size_print_count: sameSizePrintCount,
    price_breadth_divergence: priceBreadthDivergence,
    concentration_worsening: concentrationWorsening,
    cluster_correlation: clusterCorrelation,
    suspicious_burst: suspiciousBurst,
    slippage_shock: slippageShock,
    distribution_signatures: distributionSignatures,
    manipulation_penalty: manipulationPenalty,
    hard_shock: hardShock,
  };
}

/** Detect if creator has sold tokens */
function detectCreatorSell(trades: TradeDataPoint[], creator: string): boolean {
  return trades.some(t => t.traderPublicKey === creator && t.txType === 'sell');
}

/**
 * Detect repeated same-size trades (wash trading indicator).
 * Counts trades within window that have nearly identical SOL amounts.
 */
function detectSameSizePrints(
  trades: TradeDataPoint[],
  windowS: number,
  tolerancePct: number,
  now: number
): number {
  const cutoff = now - windowS * 1000;
  const recentTrades = trades.filter(t => t.timestamp >= cutoff);
  if (recentTrades.length < 2) return 0;

  // Group by similar sizes
  let maxGroupSize = 0;
  const sizes = recentTrades.map(t => t.solAmount);

  for (let i = 0; i < sizes.length; i++) {
    let groupSize = 1;
    for (let j = i + 1; j < sizes.length; j++) {
      const diff = Math.abs(sizes[i] - sizes[j]);
      const threshold = sizes[i] * tolerancePct;
      if (diff <= threshold) {
        groupSize++;
      }
    }
    maxGroupSize = Math.max(maxGroupSize, groupSize);
  }

  return maxGroupSize;
}

/**
 * Detect price going up while breadth stays flat (artificial pump).
 * Returns divergence score [0,1].
 */
function detectPriceBreadthDivergence(
  trades: TradeDataPoint[],
  uniqueBuyers: Set<string>,
  now: number
): number {
  const window15s = trades.filter(t => t.timestamp >= now - 15000);
  if (window15s.length < 3) return 0;

  // Price change
  const firstMarketCap = window15s[0].marketCapSol;
  const lastMarketCap = window15s[window15s.length - 1].marketCapSol;
  const priceChange = firstMarketCap > 0 ? (lastMarketCap - firstMarketCap) / firstMarketCap : 0;

  // Breadth change: new unique buyers in last 15s
  const uniqueInWindow = new Set(
    window15s.filter(t => t.txType === 'buy').map(t => t.traderPublicKey)
  );
  const breadthGrowth = uniqueBuyers.size > 0
    ? uniqueInWindow.size / Math.max(10, uniqueBuyers.size)
    : 0;

  // Divergence: price going up significantly but breadth barely growing
  if (priceChange > 0.05 && breadthGrowth < 0.02) {
    return Math.min(1, priceChange / 0.1);
  }
  return 0;
}

/**
 * Detect concentration worsening: top holders gaining share.
 * Uses recent trades to see if concentration is increasing.
 */
function detectConcentrationWorsening(
  trades: TradeDataPoint[],
  walletBalances: Map<string, number>
): number {
  const sortedBalances = Array.from(walletBalances.values())
    .filter(b => b > 0)
    .sort((a, b) => b - a);

  // Need enough holders to meaningfully detect concentration changes
  // With < 10 holders, high concentration is expected for new tokens
  if (sortedBalances.length < 10) return 0;

  const totalSupply = sortedBalances.reduce((a, b) => a + b, 0);
  if (totalSupply <= 0) return 0;

  const top5Share = sortedBalances.slice(0, 5).reduce((a, b) => a + b, 0) / totalSupply;

  // Only flag if top5 share is extreme AND growing via recent large buys
  if (top5Share < 0.5) return 0; // Healthy concentration, no concern

  // Recent large buys by existing top holders indicate active accumulation
  const recentBigBuys = trades.filter(t => {
    if (t.txType !== 'buy') return false;
    const balance = walletBalances.get(t.traderPublicKey) || 0;
    // Only count if this wallet is already a top-5 holder AND buying more
    const rank = sortedBalances.indexOf(balance);
    return rank >= 0 && rank < 5 && t.solAmount > 0.1;
  });

  if (recentBigBuys.length === 0) return 0;

  // Scale by how extreme the concentration is above 50%
  const worseningSignal = (top5Share - 0.5) * 2; // 0 at 50%, 1 at 100%
  return Math.min(1, worseningSignal);
}

/**
 * Detect cluster correlation: multiple wallets trading in tight temporal correlation.
 * Indicates coordinated group activity.
 */
function detectClusterCorrelation(trades: TradeDataPoint[], now: number): number {
  const window5s = trades.filter(t => t.timestamp >= now - 5000);
  if (window5s.length < 3) return 0;

  // Group trades into 500ms buckets
  const buckets = new Map<number, TradeDataPoint[]>();
  for (const t of window5s) {
    const bucket = Math.floor(t.timestamp / 500);
    const existing = buckets.get(bucket) || [];
    existing.push(t);
    buckets.set(bucket, existing);
  }

  // Find max unique wallets in a single bucket
  let maxWalletsInBucket = 0;
  for (const [_, bucketTrades] of buckets) {
    const uniqueWallets = new Set(bucketTrades.map(t => t.traderPublicKey));
    maxWalletsInBucket = Math.max(maxWalletsInBucket, uniqueWallets.size);
  }

  // High correlation if many unique wallets trade in the same 500ms window
  return Math.min(1, maxWalletsInBucket / 10);
}

/**
 * Detect suspicious burst: sudden spike in activity with low breadth.
 */
function detectSuspiciousBurst(trades: TradeDataPoint[], now: number): number {
  const window1s = trades.filter(t => t.timestamp >= now - 1000);
  const window5s = trades.filter(t => t.timestamp >= now - 5000);

  if (window5s.length === 0) return 0;

  const rate1s = window1s.length;
  const rate5s = window5s.length / 5;

  // Burst: 1s rate much higher than 5s average
  if (rate5s > 0 && rate1s > rate5s * 3) {
    const uniqueIn1s = new Set(window1s.map(t => t.traderPublicKey));
    const breadthRatio = uniqueIn1s.size / Math.max(1, rate1s);
    // Low breadth in burst = suspicious
    if (breadthRatio < 0.5) {
      return Math.min(1, (rate1s / rate5s - 1) / 5);
    }
  }
  return 0;
}

/**
 * Detect slippage shock: sudden increase in slippage without healthy breadth.
 * Uses marketCapSol changes as proxy for slippage.
 */
function detectSlippageShock(
  trades: TradeDataPoint[],
  uniqueBuyers: Set<string>,
  now: number
): number {
  const window5s = trades.filter(t => t.timestamp >= now - 5000);
  if (window5s.length < 3) return 0;

  // Look for large price moves with few unique participants
  const priceChanges: number[] = [];
  for (let i = 1; i < window5s.length; i++) {
    const prev = window5s[i - 1].marketCapSol;
    const curr = window5s[i].marketCapSol;
    if (prev > 0) {
      priceChanges.push(Math.abs(curr - prev) / prev);
    }
  }

  const maxPriceChange = Math.max(0, ...priceChanges);
  const uniqueIn5s = new Set(window5s.map(t => t.traderPublicKey));
  const breadthOk = uniqueIn5s.size >= 5;

  // Shock: large price move with low breadth
  if (maxPriceChange > 0.1 && !breadthOk) {
    return Math.min(1, maxPriceChange / 0.2);
  }
  return 0;
}

/**
 * Detect distribution event signatures: coordinated selling patterns.
 */
function detectDistributionSignatures(
  trades: TradeDataPoint[],
  walletBalances: Map<string, number>,
  creator: string
): number {
  // Count sells from large holders
  const sortedBalances = Array.from(walletBalances.entries())
    .filter(([_, b]) => b > 0)
    .sort((a, b) => b[1] - a[1]);

  const topHolders = new Set(sortedBalances.slice(0, 20).map(([addr]) => addr));
  const recentSells = trades.filter(t =>
    t.txType === 'sell' && topHolders.has(t.traderPublicKey) && t.traderPublicKey !== creator
  );

  // Multiple top holders selling = distribution signal
  const uniqueSellers = new Set(recentSells.map(t => t.traderPublicKey));
  return Math.min(1, uniqueSellers.size / 5);
}
