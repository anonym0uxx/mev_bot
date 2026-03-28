/**
 * @module strategies/SandwichDetector
 * Evaluates ShredStream pre-confirmation trades for sandwich viability.
 *
 * Sandwich logic:
 *   1. Victim detected via ShredStream buying V SOL into curve with S vSol
 *   2. We buy P SOL first (front-run, 0.26% price impact at P=0.10, S=39)
 *   3. Victim's buy pushes curve further
 *   4. We sell our tokens into the post-victim curve
 *   Net: sell_proceeds - our_buy - fees - tip > MIN_PROFIT
 *
 * Key safety: bundle atomicity means if victim reverts, our buy never lands.
 * Victim revert risk: <2% (our 0.26% impact << 10-15% Pump.fun default slippage)
 */

import { curveStateCache } from '../state/CurveStateCache';
import { BondingCurveSimulator } from '../mev/bonding-curve-sim';
import { MevConfig } from '../types/config';
// ShredStreamPumpTrade is defined in Phase 1 (ShredStream build). Interface mirrored here for pre-compilation.
export interface ShredStreamPumpTrade {
  signature: string;
  slot: number;
  tokenMint: string;
  bondingCurveKey: string;
  solAmount: number;
  isBuy: boolean;
  traderWallet: string;
  detectedAt: number;
  source: 'shredstream';
  victimTxBytes?: Buffer;
}
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';

const log = createLogger('SandwichDetector');

/** Minimum net profit in SOL to emit a signal */
const MIN_NET_PROFIT_SOL = 0.00050; // 500,000 lamports

/** Estimated fees drag for buy + sell txs (priority fees, compute units) */
const FEES_DRAG_LAMPORTS = 1_000_000n; // 0.001 SOL

export interface SandwichSignal {
  victimSignature: string;
  victimTxBytes: Buffer;
  mint: string;
  bondingCurveKey: string;
  victimSolAmount: number;
  ourPositionSol: number;
  ourPositionLamports: bigint;
  estimatedGrossProfitSol: number;
  estimatedNetProfitSol: number;
  tipLamports: number;
  curveVSolLamports: bigint;
  curveVTokens: bigint;
  tokensWeReceive: bigint;
  newCurveAfterOurBuy: { vSol: bigint; vTokens: bigint };
  newCurveAfterVictim: { vSol: bigint; vTokens: bigint };
  ourSellProceedsLamports: bigint;
  detectedAt: number;
}

export interface SandwichStats {
  evaluatedCount: number;
  signalsEmitted: number;
  skippedNoCache: number;
  skippedBelowThreshold: number;
  skippedUnprofitable: number;
}

export class SandwichDetector {
  private cfg: MevConfig;
  private sim: BondingCurveSimulator;

  // Stats tracking
  private evaluatedCount = 0;
  private signalsEmitted = 0;
  private skippedNoCache = 0;
  private skippedBelowThreshold = 0;
  private skippedUnprofitable = 0;

  constructor(cfg: MevConfig, sim: BondingCurveSimulator) {
    this.cfg = cfg;
    this.sim = sim;
  }

  /**
   * Evaluate a ShredStream trade for sandwich viability.
   * Returns a SandwichSignal if profitable, null otherwise.
   */
  evaluate(trade: ShredStreamPumpTrade, currentSlot: number): SandwichSignal | null {
    this.evaluatedCount++;

    // ── Step 1: Basic eligibility ──────────────────────────────────────
    if (!trade.isBuy || !trade.victimTxBytes) {
      log.debug(
        `Skip ${trade.signature.slice(0, 8)}: isBuy=${trade.isBuy}, hasTxBytes=${!!trade.victimTxBytes}`,
      );
      return null;
    }

    // ── Step 2: Minimum trigger threshold ──────────────────────────────
    const minTriggerSol = this.cfg.sandwich_min_trigger_sol ?? 0.75;
    if (trade.solAmount < minTriggerSol) {
      this.skippedBelowThreshold++;
      log.debug(
        `Skip ${trade.signature.slice(0, 8)}: ${trade.solAmount} SOL < ${minTriggerSol} SOL trigger`,
      );
      return null;
    }

    // ── Step 3: Get cached curve state ─────────────────────────────────
    const curve = curveStateCache.get(trade.tokenMint);
    if (!curve) {
      this.skippedNoCache++;
      log.debug(`Skip ${trade.signature.slice(0, 8)}: no cached curve for ${trade.tokenMint.slice(0, 8)}`);
      return null;
    }

    // ── Step 4: Staleness check ────────────────────────────────────────
    const maxStalenessSlots = this.cfg.sandwich_max_staleness_slots ?? 5;
    if (curveStateCache.isStale(trade.tokenMint, currentSlot, maxStalenessSlots)) {
      this.skippedNoCache++; // counted as cache miss for staleness
      log.debug(
        `Skip ${trade.signature.slice(0, 8)}: stale curve (slot delta ${currentSlot - curve.lastSlot} > ${maxStalenessSlots})`,
      );
      return null;
    }

    // ── Step 5: Determine position size and tip from tiers ─────────────
    let ourPositionSol: number;
    let tipLamports: number;

    if (trade.solAmount < 1.5) {
      // Tier 1: small victim buy (0.75 – 1.5 SOL)
      ourPositionSol = this.cfg.sandwich_position_size_sol ?? 0.10;
      tipLamports = this.cfg.sandwich_tip_lamports ?? 200_000;
    } else if (trade.solAmount < 3.0) {
      // Tier 2: medium victim buy (1.5 – 3.0 SOL)
      ourPositionSol = 0.15;
      tipLamports = this.cfg.sandwich_tip_lamports ?? 200_000;
    } else {
      // Tier 3: large victim buy (>= 3.0 SOL)
      ourPositionSol = 0.20;
      tipLamports = this.cfg.sandwich_tip_large_lamports ?? 500_000;
    }

    // ── Step 6: Simulate our front-run buy ─────────────────────────────
    const ourBuyLamports = BigInt(Math.floor(ourPositionSol * 1_000_000_000));

    const step1 = this.sim.simulateBuy(
      curve.vSolLamports,
      curve.vTokens,
      ourBuyLamports,
      100n, // 1% slippage for our buy (tight)
    );

    // ── Step 7: Simulate victim's buy on post-our-buy curve ────────────
    const victimLamports = BigInt(Math.floor(trade.solAmount * 1_000_000_000));

    const step2 = this.sim.simulateBuy(
      step1.newVSol,
      step1.newVTokens,
      victimLamports,
      5000n, // 50% slippage tolerance for victim sim (Pump.fun default is high)
    );

    // ── Step 8: Simulate our sell on post-victim curve ─────────────────
    const step3 = this.sim.simulateSell(
      step2.newVSol,
      step2.newVTokens,
      step1.tokensOut, // sell exactly the tokens we bought in step 1
    );

    // ── Step 9: Compute profit ─────────────────────────────────────────
    const grossProfitLamports = step3.solOut - ourBuyLamports;
    const netProfitLamports = grossProfitLamports - FEES_DRAG_LAMPORTS - BigInt(tipLamports);
    const netProfitSol = Number(netProfitLamports) / 1_000_000_000;
    const grossProfitSol = Number(grossProfitLamports) / 1_000_000_000;

    // ── Step 10: Profitability gate ────────────────────────────────────
    if (netProfitSol < MIN_NET_PROFIT_SOL) {
      this.skippedUnprofitable++;
      log.debug(
        `Skip ${trade.signature.slice(0, 8)}: net profit ${netProfitSol.toFixed(6)} SOL < ${MIN_NET_PROFIT_SOL} SOL minimum`,
      );
      return null;
    }

    // ── Step 11: Emit signal ───────────────────────────────────────────
    this.signalsEmitted++;

    const signal: SandwichSignal = {
      victimSignature: trade.signature,
      victimTxBytes: trade.victimTxBytes,
      mint: trade.tokenMint,
      bondingCurveKey: trade.bondingCurveKey,
      victimSolAmount: trade.solAmount,
      ourPositionSol,
      ourPositionLamports: ourBuyLamports,
      estimatedGrossProfitSol: grossProfitSol,
      estimatedNetProfitSol: netProfitSol,
      tipLamports,
      curveVSolLamports: curve.vSolLamports,
      curveVTokens: curve.vTokens,
      tokensWeReceive: step1.tokensOut,
      newCurveAfterOurBuy: {
        vSol: step1.newVSol,
        vTokens: step1.newVTokens,
      },
      newCurveAfterVictim: {
        vSol: step2.newVSol,
        vTokens: step2.newVTokens,
      },
      ourSellProceedsLamports: step3.solOut,
      detectedAt: nowMs(),
    };

    log.info(
      `🥪 Sandwich signal: ${trade.tokenMint.slice(0, 8)} | victim ${trade.solAmount} SOL | ` +
      `our ${ourPositionSol} SOL | net +${netProfitSol.toFixed(6)} SOL | tip ${tipLamports} lamports`,
    );

    return signal;
  }

  /**
   * Return current evaluation statistics.
   */
  getStats(): SandwichStats {
    return {
      evaluatedCount: this.evaluatedCount,
      signalsEmitted: this.signalsEmitted,
      skippedNoCache: this.skippedNoCache,
      skippedBelowThreshold: this.skippedBelowThreshold,
      skippedUnprofitable: this.skippedUnprofitable,
    };
  }
}
