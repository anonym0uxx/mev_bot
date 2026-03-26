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
  /** Unix ms when this token was created on-chain. Used for maturity-aware signal gating. */
  tokenCreatedAt?: number;
}

/**
 * Compute manipulation/distribution features.
 */
export function computeManipulationDistribution(
  ctx: ManipulationContext,
  config: ManipulationConfig
): ManipulationDistributionFeatures {
  const { trades, creator, walletBalances, uniqueBuyers, now, tokenCreatedAt } = ctx;
  const tokenAgeSec = tokenCreatedAt ? (now - tokenCreatedAt) / 1000 : 999;
  const lifetimeBuyers = uniqueBuyers.size;

  // 1. Creator sell detection — with lookback window + minimum size threshold.
  //    Ignores dust sells (< 0.05 SOL) and sells older than 5 min (deployment artifacts).
  //    Rationale: Pump.fun creators routinely sell tiny amounts within seconds of launch
  //    to recoup deployment gas or set aside team allocation. These are NOT rug signals.
  const creatorSell = detectCreatorSell(trades, creator, now);

  // 2. Same-size prints detection — with minimum trade size filter.
  //    Ignores micro trades (< 0.05 SOL) to avoid false-positives from min-buy clustering.
  const sameSizePrintCount = detectSameSizePrints(
    trades, config.same_size_print_window_s, config.same_size_tolerance_pct, now
  );

  // 3. Price up / breadth flat divergence
  const priceBreadthDivergence = detectPriceBreadthDivergence(trades, uniqueBuyers, now);

  // 4. Concentration worsening — skip if < 25 wallet entries or token age < 180s.
  //    Early tokens naturally have high concentration (one whale = 80% of 5 holders).
  //    The indexOf value-equality bug is also fixed: use walletBalances keys directly.
  const concentrationWorsening = detectConcentrationWorsening(trades, walletBalances, tokenAgeSec);

  // 5. Cluster correlation — with 100ms bucket (vs old 500ms) + dynamic denominator.
  //    Organic FOMO spreads across 100-500ms; real sybil farms operate in <50ms lockstep.
  //    Suppress entirely during the first 60s (launch rush is always high-concurrency).
  const clusterCorrelation = detectClusterCorrelation(trades, now, tokenAgeSec, lifetimeBuyers);

  // 6. Suspicious burst behavior
  const suspiciousBurst = detectSuspiciousBurst(trades, now);

  // 7. Slippage shock — gated on token maturity.
  //    Pump.fun bonding curve produces 10-40% price impact per trade by design on new tokens.
  //    Skip entirely if lifetime buyers < 20 or token age < 120s.
  const slippageShock = detectSlippageShock(trades, uniqueBuyers, now, tokenAgeSec, lifetimeBuyers);

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

/**
 * Detect if creator has made a meaningful sell.
 * Ignores:
 *  - Sells older than 5 minutes (deployment/gas-recoup artifacts)
 *  - Sells smaller than 0.05 SOL (dust / team allocation micro-sales)
 * Rationale: Pump.fun creators routinely sell a dust amount in the first 1-3 seconds
 * to recoup launch costs. This is NOT a rug signal. Real rugs involve the creator
 * dumping a material percentage of supply (typically 0.1+ SOL equivalent).
 */
function detectCreatorSell(
  trades: TradeDataPoint[],
  creator: string,
  now: number,
  lookbackMs = 300_000,       // 5 minute window — ignores old history
  minSellSolAmount = 0.05     // ignore dust sells below this threshold
): boolean {
  const cutoff = now - lookbackMs;
  return trades.some(
    t =>
      t.traderPublicKey === creator &&
      t.txType === 'sell' &&
      t.timestamp >= cutoff &&
      (t.solAmount ?? 0) >= minSellSolAmount
  );
}

/**
 * Detect repeated same-size trades (wash trading indicator).
 * Counts trades within window that have nearly identical SOL amounts.
 *
 * Fixes:
 * - Ignores micro trades < 0.05 SOL: min-buy on Pump.fun is often 0.001-0.005 SOL.
 *   Hundreds of organic buyers at the minimum would falsely cluster as "same size prints".
 * - Uses symmetric reference (average of pair) for tolerance, not sizes[i] alone.
 *   The old approach was asymmetric: A vs B used A's tolerance; B vs A used B's tolerance.
 *   This caused grouping to depend on iteration order.
 */
function detectSameSizePrints(
  trades: TradeDataPoint[],
  windowS: number,
  tolerancePct: number,
  now: number,
  minAbsoluteSolSize = 0.05   // ignore micro trades — they're likely min-buys, not wash trades
): number {
  const cutoff = now - windowS * 1000;
  // Filter: recent AND above minimum trade size
  const recentTrades = trades.filter(
    t => t.timestamp >= cutoff && (t.solAmount ?? 0) >= minAbsoluteSolSize
  );
  if (recentTrades.length < 2) return 0;

  const sizes = recentTrades.map(t => t.solAmount);
  let maxGroupSize = 0;

  for (let i = 0; i < sizes.length; i++) {
    let groupSize = 1;
    for (let j = i + 1; j < sizes.length; j++) {
      const diff = Math.abs(sizes[i] - sizes[j]);
      // Use average of the pair as reference — symmetric, stable across iteration order
      const ref = (sizes[i] + sizes[j]) / 2;
      const threshold = ref * tolerancePct;
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
 *
 * Fixes:
 * - Raised holder count guard from 10 → 25: young tokens naturally have high concentration.
 * - Added tokenAgeSec gate: skip if token < 180s old.
 * - Fixed indexOf value-equality bug: old code used sortedBalances.indexOf(balance) which
 *   does float equality comparison — two wallets with identical balances both map to rank 0.
 *   Fix: sort wallet entries by balance (addr, balance) pairs and rank by address, not value.
 * - Raised top5Share threshold from 0.5 to 0.7: on early tokens, one whale owning 70% is normal.
 */
function detectConcentrationWorsening(
  trades: TradeDataPoint[],
  walletBalances: Map<string, number>,
  tokenAgeSec: number
): number {
  // Too young to judge — early concentration is structural, not adversarial
  if (tokenAgeSec < 180) return 0;

  // Build sorted entries as (address, balance) pairs — preserves identity across equal balances
  const entries = Array.from(walletBalances.entries())
    .filter(([_, b]) => b > 0)
    .sort((a, b) => b[1] - a[1]);

  // Need enough distinct holders to meaningfully detect concentration changes
  if (entries.length < 25) return 0;

  const totalSupply = entries.reduce((s, [_, b]) => s + b, 0);
  if (totalSupply <= 0) return 0;

  const top5Addresses = new Set(entries.slice(0, 5).map(([addr]) => addr));
  const top5Share = entries.slice(0, 5).reduce((s, [_, b]) => s + b, 0) / totalSupply;

  // Only flag if concentration is extreme — 70%+ in top 5
  if (top5Share < 0.7) return 0;

  // Recent large buys by existing top-5 holders indicate active re-accumulation
  // (correctly uses address identity, not balance value — fixes the indexOf bug)
  const recentBigBuys = trades.filter(t =>
    t.txType === 'buy' &&
    top5Addresses.has(t.traderPublicKey) &&
    (t.solAmount ?? 0) > 0.1
  );

  if (recentBigBuys.length === 0) return 0;

  // Scale by how extreme the concentration is above 70%
  const worseningSignal = (top5Share - 0.7) / 0.3; // 0 at 70%, 1 at 100%
  return Math.min(1, worseningSignal);
}

/**
 * Detect cluster correlation: multiple wallets trading in tight temporal correlation.
 * Indicates coordinated group activity (sybil farms, bot clusters).
 *
 * Fixes:
 * - Reduced bucket size from 500ms → 100ms: organic FOMO spreads across 100-500ms due to
 *   network and human latency. Real sybil farms submit transactions in near-lockstep (<50ms).
 *   100ms buckets surgically target the latter while ignoring the former.
 * - Dynamic denominator scaled to lifetime buyer count: the old fixed denominator of 10
 *   fired at 7 wallets regardless of how many total buyers existed. On a token with 50
 *   buyers, 7 in 100ms is suspicious. On a token with 500 buyers during a viral moment, it's noise.
 * - Suppressed entirely during the first 60 seconds: launch rushes are always high-concurrency.
 *   Applying this filter at token age < 60s blocks every trending launch.
 * - Minimum 5 wallets in bucket to evaluate: prevents single-trade buckets from scoring.
 */
function detectClusterCorrelation(
  trades: TradeDataPoint[],
  now: number,
  tokenAgeSec: number,
  lifetimeBuyers: number
): number {
  // Suppress entirely during launch rush — organic FOMO is indistinguishable from bot clusters
  if (tokenAgeSec < 60) return 0;

  const window5s = trades.filter(t => t.timestamp >= now - 5000);
  if (window5s.length < 3) return 0;

  // 100ms buckets — catches sybil lockstep (<50ms spread) without penalizing organic crowd buys
  const buckets = new Map<number, TradeDataPoint[]>();
  for (const t of window5s) {
    const bucket = Math.floor(t.timestamp / 100);
    const existing = buckets.get(bucket) || [];
    existing.push(t);
    buckets.set(bucket, existing);
  }

  // Dynamic denominator: scale with how many buyers the token already has
  // At 10 lifetime buyers, denominator=10. At 50 buyers, denominator=20. Caps at 30.
  const denominator = Math.min(30, Math.max(10, Math.floor(lifetimeBuyers * 0.4)));

  let maxWalletsInBucket = 0;
  for (const [_, bucketTrades] of buckets) {
    const uniqueWallets = new Set(bucketTrades.map(t => t.traderPublicKey));
    // Require minimum 5 wallets in a bucket to score — prevents single-trade noise
    if (uniqueWallets.size >= 5) {
      maxWalletsInBucket = Math.max(maxWalletsInBucket, uniqueWallets.size);
    }
  }

  return Math.min(1, maxWalletsInBucket / denominator);
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
 * Detect slippage shock: sudden large price move without healthy buyer breadth.
 * Uses marketCapSol changes as proxy for slippage impact.
 *
 * Fixes:
 * - Gated on token maturity: skip entirely if lifetime buyers < 20 OR token age < 120s.
 *   Pump.fun bonding curve produces 10-40% price impact per early trade BY DESIGN.
 *   Applying this filter to a 3-buyer token fires on every legitimate launch.
 * - Actually uses the passed-in uniqueBuyers parameter (previously a dead argument).
 *   Old code recomputed breadth from only the 5s window, ignoring all-time breadth context.
 * - Raised price change threshold from 0.1 (10%) to 0.25 (25%) for mature tokens.
 *   10% is within normal bonding curve mechanics; 25% is a genuine shock on a liquid token.
 * - Dynamic breadth requirement: scales with lifetime buyer count, not hardcoded at 5.
 */
function detectSlippageShock(
  trades: TradeDataPoint[],
  uniqueBuyers: Set<string>,
  now: number,
  tokenAgeSec: number,
  lifetimeBuyers: number
): number {
  // Gate on maturity: too young or too few buyers → bonding curve mechanics, not manipulation
  if (tokenAgeSec < 120 || lifetimeBuyers < 20) return 0;

  const window5s = trades.filter(t => t.timestamp >= now - 5000);
  if (window5s.length < 3) return 0;

  const priceChanges: number[] = [];
  for (let i = 1; i < window5s.length; i++) {
    const prev = window5s[i - 1].marketCapSol;
    const curr = window5s[i].marketCapSol;
    if (prev > 0) {
      priceChanges.push(Math.abs(curr - prev) / prev);
    }
  }

  const maxPriceChange = Math.max(0, ...priceChanges);

  // Use lifetime breadth (passed-in uniqueBuyers) — not just the 5s window.
  // Dynamic breadth threshold: scale with token maturity (min 5, up to 15% of lifetime buyers)
  const minBreadth = Math.max(5, Math.floor(lifetimeBuyers * 0.15));
  const uniqueIn5s = new Set(window5s.map(t => t.traderPublicKey));
  const breadthOk = uniqueIn5s.size >= minBreadth || uniqueBuyers.size >= minBreadth * 2;

  // Shock: large price move (>25% on mature token) with genuinely low participation
  if (maxPriceChange > 0.25 && !breadthOk) {
    return Math.min(1, maxPriceChange / 0.5);
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
