/**
 * @module mev/position-manager
 * PositionManager: tracks open paper MEV positions.
 *
 * openPosition()     — record entry vSol, start max-hold timer
 * onSubsequentTrade() — check take_profit / stop_loss / next_buyer_exit
 * closePosition()    — compute PnL, emit 'closed' with PnLRecord
 */

import { EventEmitter } from 'events';
import { LAMPORTS_PER_SOL } from '@solana/web3.js';
import { MevConfig } from '../types/config';
import { TokenTradeEvent } from '../types/events';
import { BackrunOpportunity } from './detector';
import { BondingCurveSimulator } from './bonding-curve-sim';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';

const sim = new BondingCurveSimulator();

const log = createLogger('mev:position-manager');

export interface PnLRecord {
  mint: string;
  entryVSol: number;
  exitVSol: number;
  entryTimestampMs: number;
  exitTimestampMs: number;
  holdMs: number;
  sizeSol: number;
  pnlSol: number;
  pnlPct: number;
  exitReason: ExitReason;
  score: number;
  // Fields for live sell execution (populated from triggerEvent at open)
  bondingCurveKey?: string;
  associatedBondingCurve?: string;
  /** Estimated token balance held (from simulateBuy at entry). 0 in paper mode. */
  tokensHeld?: bigint;
  /** Current virtual reserves at exit (for live sell tx building) */
  exitVSolLamports?: bigint;
  exitVTokens?: bigint;
}

export type ExitReason = 'take_profit' | 'stop_loss' | 'next_buyer' | 'max_hold';

interface OpenPosition {
  mint: string;
  entryVSol: number;
  sizeSol: number;
  entryTimestampMs: number;
  opportunity: BackrunOpportunity;
  holdTimer: NodeJS.Timeout;
  isFirstTradeAfterEntry: boolean;
  triggerSignature: string;
  tradesSeenAfterEntry: number;
  bondingCurveKey: string;
  associatedBondingCurve: string;
  tokensHeld: bigint;
  // Track current vSol/vToken reserves for sell tx building
  currentVSolLamports: bigint;
  currentVTokens: bigint;
}

export declare interface PositionManager {
  on(event: 'closed', listener: (record: PnLRecord) => void): this;
  emit(event: 'closed', record: PnLRecord): boolean;
}

export class PositionManager extends EventEmitter {
  private cfg: MevConfig;
  private positions: Map<string, OpenPosition> = new Map();

  constructor(cfg: MevConfig) {
    super();
    this.cfg = cfg;
  }

  hasPosition(mint: string): boolean {
    return this.positions.has(mint);
  }

  get openCount(): number {
    return this.positions.size;
  }

  openPosition(opp: BackrunOpportunity): void {
    if (this.positions.has(opp.mint)) {
      log.warn(`openPosition: already have position for ${opp.mint.slice(0, 8)} — skipping (conflict_policy=skip)`);
      return;
    }

    const sizeSol = opp.recommendedSizeSol ?? this.cfg.entry_size_sol;
    const holdTimer = setTimeout(() => {
      const pos = this.positions.get(opp.mint);
      if (pos) {
        log.debug(`Max hold timeout: ${opp.mint.slice(0, 8)}`);
        this.closePosition(opp.mint, opp.entryVSol, 'max_hold');
      }
    }, this.cfg.max_hold_ms);
    holdTimer.unref();

    // Simulate buy to estimate tokensHeld and post-entry reserves
    const vSolLamports = BigInt(Math.floor(opp.triggerEvent.vSolInBondingCurve * Number(LAMPORTS_PER_SOL)));
    const vTokens = BigInt(Math.floor(opp.triggerEvent.vTokensInBondingCurve));
    const solInLamports = BigInt(Math.floor(sizeSol * Number(LAMPORTS_PER_SOL)));
    let tokensHeld = 0n;
    let postVSolLamports = vSolLamports;
    let postVTokens = vTokens;
    try {
      const buyResult = sim.simulateBuy(vSolLamports, vTokens, solInLamports, 100n);
      tokensHeld = buyResult.tokensOut;
      postVSolLamports = buyResult.newVSol;
      postVTokens = buyResult.newVTokens;
    } catch (e) {
      log.warn(`simulateBuy failed for ${opp.mint.slice(0, 8)}: ${(e as Error).message}`);
    }

    const bondingCurveKey = opp.triggerEvent.bondingCurveKey ?? '';
    const associatedBondingCurve = (opp.triggerEvent as any).associatedBondingCurve ?? bondingCurveKey;

    const pos: OpenPosition = {
      mint: opp.mint,
      entryVSol: opp.entryVSol,
      sizeSol,
      entryTimestampMs: nowMs(),
      opportunity: opp,
      holdTimer,
      isFirstTradeAfterEntry: true,
      triggerSignature: opp.triggerEvent.signature ?? '',
      tradesSeenAfterEntry: 0,
      bondingCurveKey,
      associatedBondingCurve,
      tokensHeld,
      currentVSolLamports: postVSolLamports,
      currentVTokens: postVTokens,
    };

    this.positions.set(opp.mint, pos);
    log.info(
      `📥 Opened paper position: ${opp.mint.slice(0, 8)} entryVSol=${opp.entryVSol.toFixed(2)} ` +
      `size=${sizeSol} SOL score=${opp.score.toFixed(3)} tokensHeld=${tokensHeld}`
    );
  }

  /**
   * Called for every subsequent trade on a mint we hold.
   * Checks TP / SL / next_buyer_exit conditions.
   */
  onSubsequentTrade(event: TokenTradeEvent): void {
    const pos = this.positions.get(event.mint);
    if (!pos) return;

    // Update current reserves from live event data
    pos.currentVSolLamports = BigInt(Math.floor(event.vSolInBondingCurve * Number(LAMPORTS_PER_SOL)));
    pos.currentVTokens = BigInt(Math.floor(event.vTokensInBondingCurve));

    const currentVSol = event.vSolInBondingCurve;
    const pnlPct = (currentVSol - pos.entryVSol) / pos.entryVSol;

    // Take profit
    if (pnlPct >= this.cfg.take_profit_pct) {
      this.closePosition(event.mint, currentVSol, 'take_profit');
      return;
    }

    // Stop loss
    if (pnlPct <= -this.cfg.stop_loss_pct) {
      this.closePosition(event.mint, currentVSol, 'stop_loss');
      return;
    }

    // Skip the trigger event itself — it arrives in the same loop iteration as entry
    const eventSig = (event as any).signature ?? '';
    if (eventSig && eventSig === pos.triggerSignature) return;

    // Count trades seen after entry
    pos.tradesSeenAfterEntry++;

    // Require at least 2 trades seen AND 500ms hold before any exit
    // This prevents instant same-tick or near-instant closes
    const holdSoFar = nowMs() - pos.entryTimestampMs;
    if (pos.tradesSeenAfterEntry < 2 || holdSoFar < 500) return;

    // next_buyer_exit: exit on the first buy that arrives after entry
    if (this.cfg.next_buyer_exit && event.txType === 'buy') {
      if (pos.isFirstTradeAfterEntry) {
        pos.isFirstTradeAfterEntry = false;
        this.closePosition(event.mint, currentVSol, 'next_buyer');
        return;
      }
    }

    // Mark that we've seen at least one trade after entry
    if (pos.isFirstTradeAfterEntry) {
      pos.isFirstTradeAfterEntry = false;
    }
  }

  private closePosition(mint: string, exitVSol: number, reason: ExitReason): void {
    const pos = this.positions.get(mint);
    if (!pos) return;

    clearTimeout(pos.holdTimer);
    this.positions.delete(mint);

    const exitTimestampMs = nowMs();
    const holdMs = exitTimestampMs - pos.entryTimestampMs;
    const pnlPct = (exitVSol - pos.entryVSol) / pos.entryVSol;
    const pnlSol = pnlPct * pos.sizeSol;

    const record: PnLRecord = {
      mint,
      entryVSol: pos.entryVSol,
      exitVSol,
      entryTimestampMs: pos.entryTimestampMs,
      exitTimestampMs,
      holdMs,
      sizeSol: pos.sizeSol,
      pnlSol,
      pnlPct,
      exitReason: reason,
      score: pos.opportunity.score,
      // Live sell fields
      bondingCurveKey: pos.bondingCurveKey,
      associatedBondingCurve: pos.associatedBondingCurve,
      tokensHeld: pos.tokensHeld,
      exitVSolLamports: pos.currentVSolLamports,
      exitVTokens: pos.currentVTokens,
    };

    const emoji = pnlSol >= 0 ? '✅' : '❌';
    log.info(
      `${emoji} Closed paper position: ${mint.slice(0, 8)} reason=${reason} ` +
      `pnl=${pnlSol >= 0 ? '+' : ''}${pnlSol.toFixed(4)} SOL (${(pnlPct * 100).toFixed(2)}%) ` +
      `hold=${holdMs}ms`
    );

    this.emit('closed', record);
  }

  /**
   * Force-close a single position by mint.
   * Used for external exit signals (e.g. LP removal, rug detection).
   * reason is cast to ExitReason; callers should use 'max_hold' for unrecognised reasons.
   */
  forceClosePosition(mint: string, reason: ExitReason | string): void {
    const pos = this.positions.get(mint);
    if (!pos) return;
    const exitReason: ExitReason = (['take_profit', 'stop_loss', 'next_buyer', 'max_hold'] as string[]).includes(reason)
      ? (reason as ExitReason)
      : 'max_hold';
    this.closePosition(mint, pos.entryVSol, exitReason);
  }

  /** Force-close all open positions (e.g. on shutdown) */
  closeAll(): void {
    for (const [mint, pos] of this.positions) {
      this.closePosition(mint, pos.entryVSol, 'max_hold');
    }
  }

  destroy(): void {
    for (const pos of this.positions.values()) {
      clearTimeout(pos.holdTimer);
    }
    this.positions.clear();
  }
}
