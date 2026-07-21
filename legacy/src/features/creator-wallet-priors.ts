/**
 * @module features/creator-wallet-priors
 * Feature family 3: Creator/qualified wallet priors.
 * CAPPED PRIOR ONLY — never a standalone trigger.
 * Negative evidence carries stronger penalty than positive carries boost.
 */

import { CreatorWalletPriorFeatures } from '../types/features';
import { QualifiedWalletPriorConfig } from '../types/config';

/** Context data from deep-lane enrichment */
export interface CreatorWalletContext {
  /** Creator history */
  creatorTotalCreated: number;
  creatorTotalRugged: number;
  creatorAvgHoldTime: number;
  creatorSoldFlag: boolean;
  creatorCurrentHoldings: number;
  creatorPreviousHoldings: number;

  /** Qualified wallet activity */
  qualifiedWalletsBuying: number;
  totalQualifiedWalletsKnown: number;

  /** Top trader activity */
  topTradersBuying: number;
  totalTopTradersKnown: number;

  /** First-100 buyer data */
  first100StillHolding: number;
  first100Total: number;

  /** Wallet dispersion quality */
  walletGiniCoefficient: number;

  /** Distribution behavior observations */
  distributionEventsDetected: number;
}

/**
 * Compute creator/qualified wallet prior features.
 * Returns capped composite prior: positive limited to max_positive_boost,
 * negative limited to max_negative_penalty (stronger).
 */
export function computeCreatorWalletPriors(
  ctx: CreatorWalletContext | null,
  walletBalances: Map<string, number>,
  uniqueBuyers: Set<string>,
  creator: string,
  config: QualifiedWalletPriorConfig
): CreatorWalletPriorFeatures {
  // Default features when no enrichment data available
  if (!ctx || !config.enabled) {
    return {
      creator_history_score: 0,
      creator_sell_flag: false,
      creator_holdings_trend: 0,
      qualified_wallet_score: 0,
      top_trader_score: 0,
      first_100_persistence_contribution: 0,
      dispersion_quality_score: 0,
      distribution_penalty: 0,
      composite_prior: 0,
    };
  }

  // Creator history score: [0,1] based on rug rate and creation count
  const rugRate = ctx.creatorTotalCreated > 0
    ? ctx.creatorTotalRugged / ctx.creatorTotalCreated
    : 0;
  // More creates + low rug rate = higher score; high rug rate = negative
  const creatorHistoryScore = ctx.creatorTotalCreated > 0
    ? Math.max(0, Math.min(1, (1 - rugRate) * Math.min(1, ctx.creatorTotalCreated / 10)))
    : 0.5; // Unknown creator gets neutral score

  // Creator sell flag
  const creatorSellFlag = ctx.creatorSoldFlag;

  // Creator holdings trend: positive if increasing, negative if decreasing
  const creatorHoldingsTrend = ctx.creatorPreviousHoldings > 0
    ? (ctx.creatorCurrentHoldings - ctx.creatorPreviousHoldings) / ctx.creatorPreviousHoldings
    : 0;

  // Qualified wallet participation score
  const qualifiedWalletScore = ctx.totalQualifiedWalletsKnown > 0
    ? Math.min(1, ctx.qualifiedWalletsBuying / ctx.totalQualifiedWalletsKnown)
    : 0;

  // Top trader participation score
  const topTraderScore = ctx.totalTopTradersKnown > 0
    ? Math.min(1, ctx.topTradersBuying / ctx.totalTopTradersKnown)
    : 0;

  // First-100 persistence contribution
  const first100PersistenceContribution = ctx.first100Total > 0
    ? ctx.first100StillHolding / ctx.first100Total
    : 0;

  // Dispersion quality: 1 - gini (lower gini = better distribution)
  const dispersionQualityScore = Math.max(0, 1 - ctx.walletGiniCoefficient);

  // Distribution penalty: based on detected distribution events
  const distributionPenalty = Math.min(1, ctx.distributionEventsDetected * 0.25);

  // Compute composite prior with capping
  // Positive contributions
  const positiveSignal =
    config.creator_history_weight * creatorHistoryScore +
    config.qualified_wallet_weight * qualifiedWalletScore +
    config.top_trader_weight * topTraderScore +
    config.first100_persistence_weight * first100PersistenceContribution +
    config.dispersion_quality_weight * dispersionQualityScore;

  // Negative contributions (stronger weight per spec)
  const negativeSignal =
    config.distribution_penalty_weight * distributionPenalty +
    (creatorSellFlag ? 0.5 : 0) +   // Strong negative for creator sell
    (rugRate > 0.5 ? 0.3 * rugRate : 0); // Penalty for high rug rate

  // Raw composite: positive minus negative
  const rawComposite = positiveSignal - negativeSignal;

  // Cap the composite prior
  let compositePrior: number;
  if (rawComposite >= 0) {
    compositePrior = Math.min(rawComposite, config.max_positive_boost);
  } else {
    compositePrior = Math.max(rawComposite, -config.max_negative_penalty);
  }

  return {
    creator_history_score: creatorHistoryScore,
    creator_sell_flag: creatorSellFlag,
    creator_holdings_trend: creatorHoldingsTrend,
    qualified_wallet_score: qualifiedWalletScore,
    top_trader_score: topTraderScore,
    first_100_persistence_contribution: first100PersistenceContribution,
    dispersion_quality_score: dispersionQualityScore,
    distribution_penalty: distributionPenalty,
    composite_prior: compositePrior,
  };
}
