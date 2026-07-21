/**
 * @module feed/bitquery
 * Bitquery GraphQL client for enrichment queries.
 * Used for: holder data, creator data, OHLCV, top traders, first-100 buyers.
 * Deep-lane only — must never block the fast lane.
 */

import fetch from 'node-fetch';
import { createLogger } from '../utils/logger';
import { HolderData, CreatorData, CreatorTokenHistory, OHLCVCandle } from '../types/events';

const log = createLogger('bitquery');

const DEFAULT_API_URL = 'https://streaming.bitquery.io/graphql';
const REQUEST_TIMEOUT_MS = 10000;

export class BitqueryClient {
  private apiUrl: string;
  private apiKey: string;

  constructor(apiKey?: string, apiUrl?: string) {
    this.apiKey = apiKey || process.env.BITQUERY_API_KEY || '';
    this.apiUrl = apiUrl || process.env.BITQUERY_API_URL || DEFAULT_API_URL;
  }

  /** Execute a GraphQL query against Bitquery */
  private async query<T>(gql: string, variables: Record<string, unknown> = {}): Promise<T> {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

    try {
      const response = await fetch(this.apiUrl, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${this.apiKey}`,
        },
        body: JSON.stringify({ query: gql, variables }),
        signal: controller.signal as any,
      });

      if (!response.ok) {
        throw new Error(`Bitquery HTTP ${response.status}: ${response.statusText}`);
      }

      const result = await response.json() as any;
      if (result.errors) {
        throw new Error(`Bitquery GraphQL errors: ${JSON.stringify(result.errors)}`);
      }

      return result.data as T;
    } finally {
      clearTimeout(timeout);
    }
  }

  /**
   * Get top holders for a token mint.
   */
  async getTopHolders(mint: string, limit: number = 20): Promise<HolderData[]> {
    const gql = `
      query TopHolders($mint: String!, $limit: Int!) {
        Solana {
          BalanceUpdates(
            where: {
              BalanceUpdate: { Currency: { MintAddress: { is: $mint } } }
            }
            orderBy: { descending: BalanceUpdate_Amount }
            limit: { count: $limit }
          ) {
            BalanceUpdate {
              Account { Address }
              Amount
              Currency { MintAddress }
            }
          }
        }
      }
    `;

    try {
      const data = await this.query<any>(gql, { mint, limit });
      const updates = data?.Solana?.BalanceUpdates || [];

      return updates.map((u: any) => ({
        address: u.BalanceUpdate?.Account?.Address || '',
        balance: parseFloat(u.BalanceUpdate?.Amount || '0'),
        percentage: 0, // Computed client-side from total supply
        isCreator: false, // Resolved against creator address
        firstBuyTimestamp: 0,
      }));
    } catch (err) {
      log.error(`Failed to get holders for ${mint}: ${(err as Error).message}`);
      return [];
    }
  }

  /**
   * Get creator history: tokens created, rug history, etc.
   */
  async getCreatorHistory(creatorAddress: string): Promise<CreatorData> {
    const gql = `
      query CreatorHistory($creator: String!) {
        Solana {
          DEXTradeByTokens(
            where: {
              Transaction: { Signer: { is: $creator } }
              Trade: { Dex: { ProtocolFamily: { is: "Pump.fun" } } }
            }
            orderBy: { descending: Block_Time }
            limit: { count: 50 }
          ) {
            Trade {
              Currency { MintAddress Name Symbol }
              Side { Type }
              Amount
              Price
            }
            Block { Time }
          }
        }
      }
    `;

    try {
      const data = await this.query<any>(gql, { creator: creatorAddress });
      const trades = data?.Solana?.DEXTradeByTokens || [];

      // Group by mint
      const mintMap = new Map<string, CreatorTokenHistory>();
      for (const t of trades) {
        const mintAddr = t.Trade?.Currency?.MintAddress || '';
        if (!mintMap.has(mintAddr)) {
          mintMap.set(mintAddr, {
            mint: mintAddr,
            createdAt: new Date(t.Block?.Time || 0).getTime(),
            soldAll: false,
            holdDurationS: 0,
            maxMarketCapSol: 0,
          });
        }
        const existing = mintMap.get(mintAddr)!;
        if (t.Trade?.Side?.Type === 'sell') {
          existing.soldAll = true;
        }
      }

      const history = Array.from(mintMap.values());
      const totalCreated = history.length;
      const totalRugged = history.filter(h => h.soldAll).length;
      const avgHoldTime = history.length > 0
        ? history.reduce((sum, h) => sum + h.holdDurationS, 0) / history.length
        : 0;

      return {
        address: creatorAddress,
        totalCreated,
        totalRugged,
        avgHoldTime,
        recentActivity: history.slice(0, 20),
      };
    } catch (err) {
      log.error(`Failed to get creator history for ${creatorAddress}: ${(err as Error).message}`);
      return {
        address: creatorAddress,
        totalCreated: 0,
        totalRugged: 0,
        avgHoldTime: 0,
        recentActivity: [],
      };
    }
  }

  /**
   * Get OHLCV candles for a token.
   */
  async getOHLCV(mint: string, intervalMinutes: number = 1, limit: number = 30): Promise<OHLCVCandle[]> {
    const gql = `
      query OHLCV($mint: String!, $limit: Int!) {
        Solana {
          DEXTradeByTokens(
            where: {
              Trade: {
                Currency: { MintAddress: { is: $mint } }
                Dex: { ProtocolFamily: { is: "Pump.fun" } }
              }
            }
            orderBy: { descending: Block_Time }
            limit: { count: $limit }
          ) {
            Trade {
              open: PriceInUSD(minimum: Block_Number)
              high: PriceInUSD(maximum: Trade_Price)
              low: PriceInUSD(minimum: Trade_Price)
              close: PriceInUSD(maximum: Block_Number)
              Amount
            }
            Block { Time }
          }
        }
      }
    `;

    try {
      const data = await this.query<any>(gql, { mint, limit });
      const items = data?.Solana?.DEXTradeByTokens || [];

      return items.map((item: any) => ({
        open: parseFloat(item.Trade?.open || '0'),
        high: parseFloat(item.Trade?.high || '0'),
        low: parseFloat(item.Trade?.low || '0'),
        close: parseFloat(item.Trade?.close || '0'),
        volume: parseFloat(item.Trade?.Amount || '0'),
        timestamp: new Date(item.Block?.Time || 0).getTime(),
      }));
    } catch (err) {
      log.error(`Failed to get OHLCV for ${mint}: ${(err as Error).message}`);
      return [];
    }
  }

  /**
   * Get first N buyers of a token.
   */
  async getFirstBuyers(mint: string, limit: number = 100): Promise<HolderData[]> {
    const gql = `
      query FirstBuyers($mint: String!, $limit: Int!) {
        Solana {
          DEXTradeByTokens(
            where: {
              Trade: {
                Currency: { MintAddress: { is: $mint } }
                Side: { Type: { is: "buy" } }
                Dex: { ProtocolFamily: { is: "Pump.fun" } }
              }
            }
            orderBy: { ascending: Block_Time }
            limit: { count: $limit }
          ) {
            Trade {
              Account { Address }
              Amount
              Price
            }
            Block { Time }
          }
        }
      }
    `;

    try {
      const data = await this.query<any>(gql, { mint, limit });
      const trades = data?.Solana?.DEXTradeByTokens || [];

      const seen = new Set<string>();
      const buyers: HolderData[] = [];

      for (const t of trades) {
        const addr = t.Trade?.Account?.Address || '';
        if (addr && !seen.has(addr)) {
          seen.add(addr);
          buyers.push({
            address: addr,
            balance: parseFloat(t.Trade?.Amount || '0'),
            percentage: 0,
            isCreator: false,
            firstBuyTimestamp: new Date(t.Block?.Time || 0).getTime(),
          });
        }
      }

      return buyers;
    } catch (err) {
      log.error(`Failed to get first buyers for ${mint}: ${(err as Error).message}`);
      return [];
    }
  }

  /**
   * Check if first-N buyers still hold tokens.
   */
  async checkFirstBuyerPersistence(mint: string, buyerAddresses: string[]): Promise<Map<string, boolean>> {
    const result = new Map<string, boolean>();

    // Query current balances for each buyer
    const holders = await this.getTopHolders(mint, 500);
    const holderSet = new Set(holders.filter(h => h.balance > 0).map(h => h.address));

    for (const addr of buyerAddresses) {
      result.set(addr, holderSet.has(addr));
    }

    return result;
  }

  /**
   * Get dev/creator token holdings for a specific mint.
   */
  async getDevHoldings(mint: string, devAddress: string): Promise<{ balance: number; percentage: number }> {
    try {
      const holders = await this.getTopHolders(mint, 100);
      const totalBalance = holders.reduce((sum, h) => sum + h.balance, 0);
      const devHolder = holders.find(h => h.address === devAddress);

      if (!devHolder || totalBalance === 0) {
        return { balance: 0, percentage: 0 };
      }

      return {
        balance: devHolder.balance,
        percentage: devHolder.balance / totalBalance,
      };
    } catch (err) {
      log.error(`Failed to get dev holdings: ${(err as Error).message}`);
      return { balance: 0, percentage: 0 };
    }
  }

  /**
   * Get top traders by volume for a token.
   */
  async getTopTraders(mint: string, limit: number = 20): Promise<{ address: string; volume: number; trades: number }[]> {
    const gql = `
      query TopTraders($mint: String!, $limit: Int!) {
        Solana {
          DEXTradeByTokens(
            where: {
              Trade: {
                Currency: { MintAddress: { is: $mint } }
                Dex: { ProtocolFamily: { is: "Pump.fun" } }
              }
            }
            orderBy: { descending: Trade_Amount }
            limit: { count: $limit }
          ) {
            Trade {
              Account { Address }
              Amount
            }
            count
          }
        }
      }
    `;

    try {
      const data = await this.query<any>(gql, { mint, limit });
      const items = data?.Solana?.DEXTradeByTokens || [];

      return items.map((item: any) => ({
        address: item.Trade?.Account?.Address || '',
        volume: parseFloat(item.Trade?.Amount || '0'),
        trades: item.count || 1,
      }));
    } catch (err) {
      log.error(`Failed to get top traders for ${mint}: ${(err as Error).message}`);
      return [];
    }
  }
}
