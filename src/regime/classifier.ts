/**
 * @module regime/classifier
 * Regime classification for tokens based on bonding curve progress,
 * migration status, age, and exclusion flags.
 */

import { Regime } from '../types/state';
import { RegimeConfig } from '../types/config';
import { createLogger } from '../utils/logger';
import { ageS } from '../utils/time';

const log = createLogger('regime');

export interface RegimeInput {
  bondingCurveProgress: number;
  migrated: boolean;
  tokenAgeMs: number;
  isMayhem: boolean;
  isTokenizedAgent: boolean;
  createdAt: number;
}

/**
 * Classify a token into a regime based on current state and config thresholds.
 *
 * @param input - Current token data
 * @param config - Regime configuration thresholds
 * @returns The classified regime
 */
export function classifyRegime(input: RegimeInput, config: RegimeConfig): Regime {
  // Exclusion checks first
  if (config.exclude_mayhem && input.isMayhem) {
    return Regime.EXCLUDED;
  }

  if (config.exclude_tokenized_agent && input.isTokenizedAgent) {
    return Regime.EXCLUDED;
  }

  // Age check
  const tokenAge = ageS(input.createdAt);
  if (tokenAge > config.max_token_age_s) {
    return Regime.EXCLUDED;
  }

  // Post-migration check
  if (input.migrated) {
    return Regime.POST_MIGRATION;
  }

  const progress = input.bondingCurveProgress;

  // Graduation boundary takes precedence over late curve
  if (progress >= config.graduation_boundary_start && progress <= config.graduation_boundary_end) {
    return Regime.GRADUATION_BOUNDARY;
  }

  // Curve stages
  if (progress <= config.early_curve_max_progress) {
    return Regime.EARLY_CURVE;
  }

  if (progress <= config.mid_curve_max_progress) {
    return Regime.MID_CURVE;
  }

  if (progress <= config.late_curve_max_progress) {
    return Regime.LATE_CURVE;
  }

  // Should be caught by graduation_boundary but fallthrough
  return Regime.GRADUATION_BOUNDARY;
}

/**
 * Check if a regime is tradeable (not excluded and not post-migration in initial build).
 */
export function isTradeableRegime(regime: Regime): boolean {
  return regime !== Regime.EXCLUDED && regime !== Regime.POST_MIGRATION;
}

/**
 * Compute bonding curve progress from virtual token/sol reserves.
 * Pump.fun bonding curve: starts with ~1B tokens and 30 SOL virtual.
 * Progress = 1 - (vTokensInCurve / initialVirtualTokens)
 *
 * The total supply is ~1,000,000,000 tokens.
 * Initial vTokensInBondingCurve ≈ 1,073,000,000 (with virtual offset).
 * As tokens are bought, vTokensInBondingCurve decreases.
 */
export function computeBondingCurveProgress(
  vTokensInBondingCurve: number,
  initialVirtualTokens: number = 1_073_000_000
): number {
  if (initialVirtualTokens <= 0) return 0;
  const progress = 1 - (vTokensInBondingCurve / initialVirtualTokens);
  return Math.max(0, Math.min(1, progress));
}

/**
 * Detect if a token is a "Mayhem" mode token based on metadata.
 * Mayhem tokens have specific markers in their metadata/name.
 */
export function detectMayhem(name: string, symbol: string, uri: string): boolean {
  const lower = `${name} ${symbol}`.toLowerCase();
  return lower.includes('mayhem') || lower.includes('🔥mayhem');
}

/**
 * Detect if a token is a "Tokenized Agent" based on metadata.
 */
export function detectTokenizedAgent(name: string, symbol: string, uri: string): boolean {
  const lower = `${name} ${symbol}`.toLowerCase();
  return lower.includes('agent') && (lower.includes('ai') || lower.includes('bot'));
}
