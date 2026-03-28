/**
 * @module mev/paper-trade-logger
 * PaperTradeLogger: appends JSONL records for every closed paper trade
 * and maintains in-memory statistics.
 */

import * as fs from 'fs';
import * as path from 'path';
import { PnLRecord } from './position-manager';
import { createLogger } from '../utils/logger';

const log = createLogger('mev:paper-trade-logger');

export interface TradeSummary {
  totalTrades: number;
  wins: number;
  losses: number;
  winRate: number;
  totalPnlSol: number;
  avgPnlSol: number;
  avgHoldMs: number;
  maxWinSol: number;
  maxLossSol: number;
}

export class PaperTradeLogger {
  private logFile: string;
  private totalTrades = 0;
  private wins = 0;
  private losses = 0;
  private totalPnl = 0;
  private totalHoldMs = 0;
  private maxWinSol = 0;
  private maxLossSol = 0;

  constructor(logFile: string) {
    this.logFile = logFile;
    // Ensure parent directory exists
    const dir = path.dirname(logFile);
    if (dir && dir !== '.') {
      fs.mkdirSync(dir, { recursive: true });
    }
  }

  record(trade: PnLRecord): void {
    this.totalTrades++;
    if (trade.pnlSol >= 0) {
      this.wins++;
      if (trade.pnlSol > this.maxWinSol) this.maxWinSol = trade.pnlSol;
    } else {
      this.losses++;
      if (trade.pnlSol < this.maxLossSol) this.maxLossSol = trade.pnlSol;
    }
    this.totalPnl += trade.pnlSol;
    this.totalHoldMs += trade.holdMs;

    // Append to JSONL log
    try {
      // Serialize BigInt fields as strings to avoid JSON.stringify TypeError
      // Explicitly list fields — avoids nested BigInt in trade.opportunity.triggerEvent
      const serializable = {
        mint: trade.mint,
        entryVSol: trade.entryVSol,
        exitVSol: trade.exitVSol,
        entryTimestampMs: trade.entryTimestampMs,
        exitTimestampMs: trade.exitTimestampMs,
        holdMs: trade.holdMs,
        sizeSol: trade.sizeSol,
        pnlSol: trade.pnlSol,
        pnlPct: trade.pnlPct,
        exitReason: trade.exitReason,
        score: trade.score,
        bondingCurveKey: trade.bondingCurveKey,
        tokensHeld: trade.tokensHeld?.toString(),
        exitVSolLamports: trade.exitVSolLamports?.toString(),
        exitVTokens: trade.exitVTokens?.toString(),
        // ML training context
        triggerBuySol: trade.triggerBuySol,
        triggerBuyerCount: trade.triggerBuyerCount,
        triggerHourUtc: trade.triggerHourUtc,
        curvePct: trade.curvePct,
        uniqueBuyerCount: trade.uniqueBuyerCount,
        // MFE/MAE — essential for training (captures best/worst unrealised P&L during hold)
        mfeSol: trade.mfeSol,
        maeSol: trade.maeSol,
        // Score model v2 components — for calibration and future model training
        scoreComponents: trade.scoreComponents,
        adversarialConcentration: trade.adversarialConcentration,
        // Pre-trigger gate signals — for gate validation and tuning
        preTriggerBuys1s: trade.preTriggerBuys1s,
        preTriggerBuys2s: trade.preTriggerBuys2s,
        preTriggerBuys5s: trade.preTriggerBuys5s,
        preTriggerGapMs: trade.preTriggerGapMs,
        preTriggerVSolDelta3s: trade.preTriggerVSolDelta3s,
        preTriggerVolume5s: trade.preTriggerVolume5s,
        // Sell flow signals (new: creator sell + net flow)
        preTriggerSellCount5s: trade.preTriggerSellCount5s,
        preTriggerSellVolume5s: trade.preTriggerSellVolume5s,
        preTriggerNetFlowRatio5s: trade.preTriggerNetFlowRatio5s,
        creatorSellDetected: trade.creatorSellDetected,
        // Sizing context
        todMultiplier: trade.todMultiplier,
        // Trigger source tracking (Helius fast lane)
        triggerSource: (trade as any).triggerSource,
        heliusLeadMs: (trade as any).heliusLeadMs,
        recordedAt: Date.now(),
      };
      const line = JSON.stringify(serializable) + '\n';
      fs.appendFileSync(this.logFile, line, 'utf8');
    } catch (err) {
      log.warn(`Failed to write paper trade log: ${(err as Error).message}`);
    }
  }

  getSummary(): TradeSummary {
    return {
      totalTrades: this.totalTrades,
      wins: this.wins,
      losses: this.losses,
      winRate: this.totalTrades > 0 ? this.wins / this.totalTrades : 0,
      totalPnlSol: this.totalPnl,
      avgPnlSol: this.totalTrades > 0 ? this.totalPnl / this.totalTrades : 0,
      avgHoldMs: this.totalTrades > 0 ? this.totalHoldMs / this.totalTrades : 0,
      maxWinSol: this.maxWinSol,
      maxLossSol: this.maxLossSol,
    };
  }
}
