/**
 * @module features/multimodal-junk-filter
 * Feature family 6: Secondary multimodal junk filter.
 * ASYNC and NON-BLOCKING. Must never delay fast-lane entry or forced exits.
 * Used for: obvious junk exclusion, tie-breaking, candidate ranking refinement.
 */

import { MultimodalJunkFeatures } from '../types/features';
import { MultimodalJunkFilterConfig } from '../types/config';

/** Context from async metadata fetching */
export interface MultimodalContext {
  ticker: string;
  name: string;
  uri: string;
  /** Whether metadata was fetched successfully */
  metadataFetched: boolean;
  /** Token description from metadata */
  description: string;
  /** Whether logo exists */
  hasLogo: boolean;
  /** Logo URL if exists */
  logoUrl: string;
  /** Logo analysis score [0,1] */
  logoQuality: number;
  /** Comments/replies if available */
  comments: string[];
  /** Social signals if available */
  socialMentions: number;
  fetchedAt: number;
}

/**
 * Compute multimodal junk filter score.
 * Returns stale=true if context unavailable — fast lane still operates.
 */
export function getMultimodalJunkScore(
  ctx: MultimodalContext | null,
  config: MultimodalJunkFilterConfig
): MultimodalJunkFeatures {
  // Default: stale/unavailable — does not affect fast lane
  if (!ctx || !config.enabled) {
    return {
      ticker_clarity: 0.5,
      name_clarity: 0.5,
      logo_presence: 0.5,
      logo_quality: 0.5,
      metadata_spam: 0.5,
      comment_entropy: 0.5,
      social_pickup: 0,
      junk_score: 0.5, // Neutral — no effect
      is_stale: true,
    };
  }

  // Ticker clarity: penalize gibberish tickers
  const tickerClarity = computeTickerClarity(ctx.ticker);

  // Name clarity: penalize spam-like names
  const nameClarity = computeNameClarity(ctx.name);

  // Logo presence: binary — has logo or not
  const logoPresence = ctx.hasLogo ? 1 : 0;

  // Logo quality: from external analysis or heuristic
  const logoQuality = ctx.logoQuality;

  // Metadata spam: detect repetitive/spammy metadata
  const metadataSpam = computeMetadataSpam(ctx.name, ctx.description, ctx.ticker);

  // Comment entropy: diversity of comments (low entropy = spam)
  const commentEntropy = computeCommentEntropy(ctx.comments);

  // Social pickup score: external signal
  const socialPickup = Math.min(1, ctx.socialMentions / 10);

  // Composite junk score [0=junk, 1=clean]
  const junkScore = Math.max(0, Math.min(1,
    config.ticker_clarity_weight * tickerClarity +
    config.name_clarity_weight * nameClarity +
    config.logo_presence_weight * logoPresence +
    config.logo_quality_weight * logoQuality +
    config.metadata_spam_weight * (1 - metadataSpam) +
    config.comment_entropy_weight * commentEntropy
  ));

  return {
    ticker_clarity: tickerClarity,
    name_clarity: nameClarity,
    logo_presence: logoPresence,
    logo_quality: logoQuality,
    metadata_spam: metadataSpam,
    comment_entropy: commentEntropy,
    social_pickup: socialPickup,
    junk_score: junkScore,
    is_stale: false,
  };
}

/**
 * Compute ticker clarity: [0=gibberish, 1=clear].
 * Checks for pronounceability, length, character patterns.
 */
function computeTickerClarity(ticker: string): number {
  if (!ticker || ticker.length === 0) return 0;

  let score = 1.0;

  // Penalize very long tickers
  if (ticker.length > 10) score -= 0.3;
  if (ticker.length > 15) score -= 0.3;

  // Penalize all-numeric
  if (/^\d+$/.test(ticker)) score -= 0.4;

  // Penalize excessive special characters
  const specialChars = ticker.replace(/[a-zA-Z0-9]/g, '').length;
  if (specialChars > 2) score -= 0.3;

  // Penalize random-looking character sequences
  const hasVowels = /[aeiouAEIOU]/.test(ticker);
  if (!hasVowels && ticker.length > 3) score -= 0.2;

  // Reward common patterns (3-5 char uppercase)
  if (/^[A-Z]{3,5}$/.test(ticker)) score += 0.1;

  return Math.max(0, Math.min(1, score));
}

/**
 * Compute name clarity: [0=spam, 1=clear].
 */
function computeNameClarity(name: string): number {
  if (!name || name.length === 0) return 0;

  let score = 1.0;

  // Penalize very long names (spam-like)
  if (name.length > 50) score -= 0.3;
  if (name.length > 100) score -= 0.3;

  // Penalize excessive emojis
  const emojiCount = (name.match(/[\u{1F000}-\u{1FFFF}]/gu) || []).length;
  if (emojiCount > 3) score -= 0.2;

  // Penalize excessive numbers
  const digitCount = (name.match(/\d/g) || []).length;
  if (digitCount > name.length * 0.5) score -= 0.3;

  // Penalize repeated characters (e.g., "AAAAAAA")
  if (/(.)\1{4,}/.test(name)) score -= 0.3;

  // Penalize common spam phrases
  const spamPhrases = ['100x', '1000x', 'moon', 'pump', 'gem', 'next', 'buy now', 'free'];
  const lower = name.toLowerCase();
  const spamHits = spamPhrases.filter(p => lower.includes(p)).length;
  score -= spamHits * 0.1;

  return Math.max(0, Math.min(1, score));
}

/**
 * Compute metadata spam score: [0=clean, 1=spam].
 */
function computeMetadataSpam(name: string, description: string, ticker: string): number {
  const combined = `${name} ${description} ${ticker}`.toLowerCase();

  let spamScore = 0;

  // Check for repetition
  const words = combined.split(/\s+/);
  const uniqueWords = new Set(words);
  if (words.length > 5 && uniqueWords.size < words.length * 0.5) {
    spamScore += 0.3;
  }

  // Check for excessive caps
  const capsRatio = (combined.match(/[A-Z]/g) || []).length / Math.max(1, combined.length);
  if (capsRatio > 0.7) spamScore += 0.2;

  // Check for URL spam
  const urlCount = (combined.match(/https?:\/\//g) || []).length;
  if (urlCount > 2) spamScore += 0.3;

  // Check for contract address spam
  if (/[A-Za-z0-9]{32,44}/.test(combined)) spamScore += 0.2;

  return Math.min(1, spamScore);
}

/**
 * Compute comment entropy: [0=spam/low-diversity, 1=diverse/healthy].
 */
function computeCommentEntropy(comments: string[]): number {
  if (!comments || comments.length === 0) return 0.5; // Unknown

  if (comments.length < 3) return 0.5;

  // Compute diversity: unique comments / total
  const uniqueComments = new Set(comments.map(c => c.toLowerCase().trim()));
  const diversity = uniqueComments.size / comments.length;

  // Average comment length diversity
  const lengths = comments.map(c => c.length);
  const avgLen = lengths.reduce((a, b) => a + b, 0) / lengths.length;
  const lenVariance = lengths.reduce((sum, l) => sum + Math.pow(l - avgLen, 2), 0) / lengths.length;
  const lenDiversity = Math.min(1, Math.sqrt(lenVariance) / 50);

  return Math.max(0, Math.min(1, 0.6 * diversity + 0.4 * lenDiversity));
}

/**
 * Async metadata fetcher — runs in background, updates MultimodalContext.
 * Non-blocking: returns immediately with a Promise.
 */
export async function fetchTokenMetadata(
  mint: string,
  uri: string,
  ticker: string,
  name: string
): Promise<MultimodalContext> {
  const ctx: MultimodalContext = {
    ticker,
    name,
    uri,
    metadataFetched: false,
    description: '',
    hasLogo: false,
    logoUrl: '',
    logoQuality: 0,
    comments: [],
    socialMentions: 0,
    fetchedAt: Date.now(),
  };

  try {
    if (!uri) return ctx;

    // Fetch metadata JSON from URI (typically IPFS or Arweave)
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 5000);

    const fetch = (await import('node-fetch')).default;
    const response = await fetch(uri, { signal: controller.signal as any });
    clearTimeout(timeout);

    if (response.ok) {
      const metadata = await response.json() as any;
      ctx.metadataFetched = true;
      ctx.description = metadata.description || '';
      ctx.hasLogo = !!(metadata.image || metadata.logo);
      ctx.logoUrl = metadata.image || metadata.logo || '';
      // Logo quality: basic heuristic — presence implies some quality
      ctx.logoQuality = ctx.hasLogo ? 0.6 : 0;
    }
  } catch {
    // Non-blocking: failure is acceptable
  }

  return ctx;
}
