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
    const line = JSON.stringify({
      ...trade,
      recordedAt: Date.now(),
    }) + '\n';

    try {
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
