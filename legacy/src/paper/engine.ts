/**
 * @module paper/engine
 * Paper trading engine: runs on live feed with synthetic fills.
 * Uses IDENTICAL decision logic as live mode.
 * All synthetic fills and decisions are persisted.
 */

import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';

const log = createLogger('paper');

/**
 * Paper trading mode indicator.
 * When active, the execution adapter uses synthetic fills.
 * All other logic (features, probabilities, entry/exit engines) runs identically.
 */
export function isPaperMode(): boolean {
  return process.env.PAPER_MODE === 'true';
}

/**
 * Synthetic fill generator for paper mode.
 * Simulates realistic fills based on current market state.
 */
export interface SyntheticFill {
  mint: string;
  side: 'buy' | 'sell';
  requestedSol: number;
  filledSol: number;
  filledTokens: number;
  fillPrice: number;
  slippagePct: number;
  latencyMs: number;
  timestamp: number;
}

/**
 * Generate a synthetic buy fill based on bonding curve state.
 */
export function generateSyntheticBuyFill(
  mint: string,
  solAmount: number,
  vTokensInCurve: number,
  vSolInCurve: number,
  expectedSlippagePct: number
): SyntheticFill {
  // Bonding curve: xy = k
  const k = vTokensInCurve * vSolInCurve;
  const newVSol = vSolInCurve + solAmount;
  const newVTokens = k / newVSol;
  const tokensReceived = vTokensInCurve - newVTokens;

  // Apply slippage
  const actualTokens = tokensReceived * (1 - expectedSlippagePct);
  const fillPrice = solAmount / actualTokens;

  // Simulate latency (200-800ms)
  const latencyMs = 200 + Math.random() * 600;

  return {
    mint,
    side: 'buy',
    requestedSol: solAmount,
    filledSol: solAmount,
    filledTokens: actualTokens,
    fillPrice,
    slippagePct: expectedSlippagePct,
    latencyMs,
    timestamp: nowMs(),
  };
}

/**
 * Generate a synthetic sell fill based on bonding curve state.
 */
export function generateSyntheticSellFill(
  mint: string,
  tokenAmount: number,
  vTokensInCurve: number,
  vSolInCurve: number,
  expectedSlippagePct: number
): SyntheticFill {
  // Selling tokens back to curve
  const k = vTokensInCurve * vSolInCurve;
  const newVTokens = vTokensInCurve + tokenAmount;
  const newVSol = k / newVTokens;
  const solReceived = vSolInCurve - newVSol;

  // Apply slippage
  const actualSol = solReceived * (1 - expectedSlippagePct);
  const fillPrice = actualSol / tokenAmount;

  const latencyMs = 200 + Math.random() * 600;

  return {
    mint,
    side: 'sell',
    requestedSol: solReceived,
    filledSol: actualSol,
    filledTokens: tokenAmount,
    fillPrice,
    slippagePct: expectedSlippagePct,
    latencyMs,
    timestamp: nowMs(),
  };
}
