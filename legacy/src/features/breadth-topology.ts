/**
 * @module features/breadth-topology
 * Feature family 2: Breadth/topology computation.
 * Unique buyer growth, repeat-wallet ratio, fresh-wallet ratio, non-dev participation,
 * first-100 persistence, concentration metrics, breadth score.
 */

import { BreadthTopologyFeatures, TradeDataPoint } from '../types/features';

/**
 * Compute breadth/topology features.
 *
 * @param trades - Recent trade data points
 * @param allUniqueBuyers - All unique buyer addresses ever seen
 * @param walletBalances - Current wallet → token balance map
 * @param creator - Creator wallet address
 * @param windows - Window sizes in seconds
 * @param now - Current timestamp in ms
 */
export function computeBreadthTopology(
  trades: TradeDataPoint[],
  allUniqueBuyers: Set<string>,
  walletBalances: Map<string, number>,
  creator: string,
  windows: number[],
  now: number
): BreadthTopologyFeatures {
  // Unique buyer growth over windows
  const uniqueBuyersInWindow = (w: number): number => {
    const cutoff = now - w * 1000;
    const recentBuyers = new Set<string>();
    for (const t of trades) {
      if (t.timestamp >= cutoff && t.txType === 'buy') {
        recentBuyers.add(t.traderPublicKey);
      }
    }
    return recentBuyers.size;
  };

  const ub5s = uniqueBuyersInWindow(5);
  const ub15s = uniqueBuyersInWindow(15);

  // Unique buyer growth rate: new unique buyers / window
  const uniqueBuyersGrowth5s = 5 > 0 ? ub5s / 5 : 0;
  const uniqueBuyersGrowth15s = 15 > 0 ? ub15s / 15 : 0;

  // Repeat wallet ratio: wallets that traded more than once / total unique
  const walletTradeCounts = new Map<string, number>();
  for (const t of trades) {
    const current = walletTradeCounts.get(t.traderPublicKey) || 0;
    walletTradeCounts.set(t.traderPublicKey, current + 1);
  }
  const totalUniqueTraders = walletTradeCounts.size;
  const repeatWallets = Array.from(walletTradeCounts.values()).filter(c => c > 1).length;
  const repeatWalletRatio = totalUniqueTraders > 0 ? repeatWallets / totalUniqueTraders : 0;

  // Fresh wallet ratio: wallets with only 1 trade / total unique
  const freshWallets = Array.from(walletTradeCounts.values()).filter(c => c === 1).length;
  const freshWalletRatio = totalUniqueTraders > 0 ? freshWallets / totalUniqueTraders : 0;

  // Non-dev participation: fraction of buys not from creator
  const totalBuys = trades.filter(t => t.txType === 'buy').length;
  const devBuys = trades.filter(t => t.txType === 'buy' && t.traderPublicKey === creator).length;
  const nonDevParticipation = totalBuys > 0 ? (totalBuys - devBuys) / totalBuys : 0;

  // First-100 buyer persistence: fraction of first 100 buyers still holding
  // This is populated by deep-lane enrichment, use approximation from known balances
  const sortedBuyers = Array.from(allUniqueBuyers).slice(0, 100);
  let holdingCount = 0;
  for (const buyer of sortedBuyers) {
    const balance = walletBalances.get(buyer) || 0;
    if (balance > 0) holdingCount++;
  }
  const first100Persistence = sortedBuyers.length > 0 ? holdingCount / sortedBuyers.length : 0;

  // Concentration: top-N holders by balance / total supply
  const balanceEntries = Array.from(walletBalances.entries())
    .filter(([_, bal]) => bal > 0)
    .sort((a, b) => b[1] - a[1]);

  const totalSupplyHeld = balanceEntries.reduce((sum, [_, bal]) => sum + bal, 0);

  const topNConcentration = (n: number): number => {
    if (totalSupplyHeld <= 0 || balanceEntries.length === 0) return 0;
    const topN = balanceEntries.slice(0, n);
    const topSum = topN.reduce((sum, [_, bal]) => sum + bal, 0);
    return topSum / totalSupplyHeld;
  };

  const top10Concentration = topNConcentration(10);
  const top20Concentration = topNConcentration(20);

  // Breadth score: composite [0,1] — higher is better breadth
  // Weighted combination of normalized metrics
  const breadthScore = Math.min(1, Math.max(0,
    0.25 * Math.min(1, allUniqueBuyers.size / 50) +  // Normalized buyer count
    0.20 * nonDevParticipation +                       // Non-dev activity
    0.15 * first100Persistence +                       // Early buyer retention
    0.15 * (1 - top10Concentration) +                  // Low concentration
    0.15 * freshWalletRatio +                          // Diverse fresh wallets
    0.10 * (1 - repeatWalletRatio)                     // Not dominated by repeat traders
  ));

  return {
    unique_buyers_growth_5s: uniqueBuyersGrowth5s,
    unique_buyers_growth_15s: uniqueBuyersGrowth15s,
    unique_buyers_total: allUniqueBuyers.size,
    repeat_wallet_ratio: repeatWalletRatio,
    fresh_wallet_ratio: freshWalletRatio,
    non_dev_participation: nonDevParticipation,
    first_100_persistence: first100Persistence,
    top_10_concentration: top10Concentration,
    top_20_concentration: top20Concentration,
    breadth_score: breadthScore,
  };
}
