/**
 * @module feed/social-cache
 * Async pre-fetch of pump.fun token social/metadata signals.
 * Zero API key required. Uses frontend-api-v3.pump.fun (public, unauthenticated).
 *
 * Fetched on token discovery (non-blocking). Read at entry decision time from cache.
 * No fetch = neutral defaults (never blocks entry).
 *
 * Signals provided:
 *   - has_twitter / has_telegram / has_website (social presence)
 *   - reply_count (community engagement)
 *   - ath_market_cap_sol (all-time high; if current << ATH → fading token)
 *   - is_banned / nsfw (instant disqualifiers)
 *   - last_trade_age_s (staleness of last trade)
 */

import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';

const log = createLogger('social-cache');

const PUMP_API = 'https://frontend-api-v3.pump.fun/coins';
const CACHE_TTL_MS = 5 * 60 * 1000;  // 5 min TTL — token metadata rarely changes
const MAX_ENTRIES = 5_000;
const FETCH_TIMEOUT_MS = 2_000;       // 2s max — never blocks entry decision

export interface SocialMetadata {
  mint: string;
  has_twitter: boolean;
  has_telegram: boolean;
  has_website: boolean;
  reply_count: number;
  ath_market_cap_sol: number;
  current_market_cap_sol: number;
  is_banned: boolean;
  nsfw: boolean;
  last_trade_age_s: number;   // seconds since last trade at fetch time
  social_score: number;        // composite 0-1: (has_twitter*0.4 + has_telegram*0.3 + has_website*0.2 + reply_score*0.1)
  fetchedAt: number;
  fetchOk: boolean;
}

const NEUTRAL: Omit<SocialMetadata, 'mint' | 'fetchedAt' | 'fetchOk'> = {
  has_twitter: false,
  has_telegram: false,
  has_website: false,
  reply_count: 0,
  ath_market_cap_sol: 0,
  current_market_cap_sol: 0,
  is_banned: false,
  nsfw: false,
  last_trade_age_s: 0,
  social_score: 0.5, // neutral — unknown, don't penalize
};

export class SocialCache {
  private cache: Map<string, SocialMetadata> = new Map();
  private pending: Set<string> = new Set();
  private lruOrder: string[] = [];

  /** Get cached social metadata, or null if not available */
  get(mint: string): SocialMetadata | null {
    const entry = this.cache.get(mint);
    if (!entry) return null;
    if (nowMs() - entry.fetchedAt > CACHE_TTL_MS) {
      this.cache.delete(mint);
      return null;
    }
    return entry;
  }

  /** Get cached entry or neutral defaults (never returns null — safe for use in entry engine) */
  getOrNeutral(mint: string): SocialMetadata {
    return this.get(mint) ?? { ...NEUTRAL, mint, fetchedAt: 0, fetchOk: false };
  }

  /** Trigger async pre-fetch. Non-blocking. Deduplicated. */
  prefetch(mint: string): void {
    if (this.cache.has(mint) || this.pending.has(mint)) return;
    this.pending.add(mint);
    this.fetchFromApi(mint).catch(() => {}).finally(() => this.pending.delete(mint));
  }

  private async fetchFromApi(mint: string): Promise<void> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);

    try {
      const res = await fetch(`${PUMP_API}/${mint}`, {
        signal: controller.signal,
        headers: { 'User-Agent': 'pump-quant/1.0' },
      });
      clearTimeout(timer);

      if (!res.ok) {
        this.setNeutral(mint);
        return;
      }

      const d = await res.json() as Record<string, unknown>;

      const replyCount = Number(d.reply_count ?? 0);
      const replyScore = Math.min(replyCount / 20, 1.0); // cap at 20 replies = max score
      const hasTwitter = Boolean(d.twitter && String(d.twitter).length > 0);
      const hasTelegram = Boolean(d.telegram && String(d.telegram).length > 0);
      const hasWebsite = Boolean(d.website && String(d.website).length > 0);
      const isBanned = Boolean(d.is_banned);
      const nsfw = Boolean(d.nsfw);
      const athMcap = Number(d.ath_market_cap ?? 0);
      const currentMcap = Number(d.market_cap ?? 0);
      const lastTradeTs = Number(d.last_trade_timestamp ?? 0);
      const lastTradeAgeS = lastTradeTs > 0 ? (nowMs() - lastTradeTs) / 1000 : 0;

      const socialScore =
        (hasTwitter ? 0.4 : 0) +
        (hasTelegram ? 0.3 : 0) +
        (hasWebsite ? 0.2 : 0) +
        replyScore * 0.1;

      const entry: SocialMetadata = {
        mint,
        has_twitter: hasTwitter,
        has_telegram: hasTelegram,
        has_website: hasWebsite,
        reply_count: replyCount,
        ath_market_cap_sol: athMcap,
        current_market_cap_sol: currentMcap,
        is_banned: isBanned,
        nsfw,
        last_trade_age_s: lastTradeAgeS,
        social_score: socialScore,
        fetchedAt: nowMs(),
        fetchOk: true,
      };

      this.evictIfNeeded();
      this.cache.set(mint, entry);
      this.lruOrder.push(mint);

      log.debug(`Social fetch ${mint.slice(0, 8)}: twitter=${hasTwitter} tg=${hasTelegram} web=${hasWebsite} replies=${replyCount} score=${socialScore.toFixed(2)}`);
    } catch {
      clearTimeout(timer);
      this.setNeutral(mint);
    }
  }

  private setNeutral(mint: string): void {
    const entry: SocialMetadata = { ...NEUTRAL, mint, fetchedAt: nowMs(), fetchOk: false };
    this.evictIfNeeded();
    this.cache.set(mint, entry);
    this.lruOrder.push(mint);
  }

  private evictIfNeeded(): void {
    while (this.lruOrder.length >= MAX_ENTRIES) {
      const oldest = this.lruOrder.shift();
      if (oldest) this.cache.delete(oldest);
    }
  }

  get size(): number { return this.cache.size; }
}
