/**
 * @module feed/creator-cache
 * LRU cache for creator wallet history with GraphQL API lookup.
 * Budget: 2,100 calls/day (~87/hour). Only queries unseen creators.
 */

import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';

const log = createLogger('creator-cache');

export interface CreatorHistory {
  creator: string;
  totalTokensLaunched: number;
  graduatedCount: number;
  rugCount: number;
  avgTimeToGraduateMs: number;
  lastLaunch: number;
  fetchedAt: number;
}

const BITQUERY_API_URL = 'https://streaming.bitquery.io/graphql';
const DAILY_RESET_MS = 24 * 60 * 60 * 1000;
const MAX_ENTRIES = 10_000;
const DAILY_BUDGET = 2_100;

const CREATOR_HISTORY_QUERY = `
  query CreatorHistory($creator: String!) {
    Solana {
      Instructions(
        where: {
          Instruction: {
            Program: { Address: { is: "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P" } }
            Name: { is: "create" }
          }
          Transaction: { Signer: { is: $creator } }
        }
      ) {
        count
      }
    }
  }
`;

export class CreatorCache {
  private cache: Map<string, CreatorHistory> = new Map();
  private lruOrder: string[] = [];
  private callCount = 0;
  private windowStart = nowMs();
  private apiKey: string;

  constructor(apiKeyEnv: string = 'BITQUERY_API_KEY') {
    this.apiKey = process.env[apiKeyEnv] || process.env.BITQUERY_API_KEY || '';
  }

  /** Get cached creator history, or null if not cached */
  get(creator: string): CreatorHistory | null {
    const entry = this.cache.get(creator);
    if (!entry) return null;

    // Move to front of LRU
    const idx = this.lruOrder.indexOf(creator);
    if (idx > -1) {
      this.lruOrder.splice(idx, 1);
      this.lruOrder.push(creator);
    }
    return entry;
  }

  /** Store creator history in cache */
  set(creator: string, history: CreatorHistory): void {
    if (this.cache.size >= MAX_ENTRIES) {
      this.evictLRU();
    }
    this.cache.set(creator, history);
    const idx = this.lruOrder.indexOf(creator);
    if (idx > -1) this.lruOrder.splice(idx, 1);
    this.lruOrder.push(creator);
  }

  /** Whether we should make an API call for this creator */
  shouldLookup(creator: string): boolean {
    if (!creator) return false;
    if (this.cache.has(creator)) return false;
    if (!this.apiKey) return false;

    // Reset window if 24h elapsed
    if (nowMs() - this.windowStart > DAILY_RESET_MS) {
      this.callCount = 0;
      this.windowStart = nowMs();
    }

    return this.callCount < DAILY_BUDGET;
  }

  /** Fetch creator history from Bitquery GraphQL API */
  async fetchFromApi(creator: string): Promise<CreatorHistory | null> {
    if (!this.shouldLookup(creator)) return null;
    if (!this.apiKey) return null;

    try {
      this.callCount++;
      const response = await fetch(BITQUERY_API_URL, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${this.apiKey}`,
        },
        body: JSON.stringify({
          query: CREATOR_HISTORY_QUERY,
          variables: { creator },
        }),
      });

      if (!response.ok) {
        log.warn(`Creator API error: ${response.status} for ${creator.slice(0, 8)}`);
        return null;
      }

      const data: any = await response.json();
      const count = data?.data?.Solana?.Instructions?.count || 0;

      const history: CreatorHistory = {
        creator,
        totalTokensLaunched: count,
        graduatedCount: 0, // Would need additional query
        rugCount: 0,
        avgTimeToGraduateMs: 0,
        lastLaunch: nowMs(),
        fetchedAt: nowMs(),
      };

      this.set(creator, history);
      log.debug(`Creator ${creator.slice(0, 8)}: ${count} tokens launched (budget: ${this.getRemainingBudget()} left)`);
      return history;
    } catch (err) {
      log.warn(`Creator API fetch failed: ${(err as Error).message}`);
      return null;
    }
  }

  /** Get or fetch creator history (async) */
  async getOrFetch(creator: string): Promise<CreatorHistory | null> {
    const cached = this.get(creator);
    if (cached) return cached;
    return this.fetchFromApi(creator);
  }

  getRemainingBudget(): number {
    if (nowMs() - this.windowStart > DAILY_RESET_MS) return DAILY_BUDGET;
    return Math.max(0, DAILY_BUDGET - this.callCount);
  }

  get size(): number {
    return this.cache.size;
  }

  private evictLRU(): void {
    const oldest = this.lruOrder.shift();
    if (oldest) this.cache.delete(oldest);
  }
}
