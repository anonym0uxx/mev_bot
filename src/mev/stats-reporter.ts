/**
 * @module mev/stats-reporter
 * MevStatsReporter: logs a summary every N trades or on SIGINT.
 */

import { PaperTradeLogger } from './paper-trade-logger';
import { createLogger } from '../utils/logger';

const log = createLogger('mev:stats-reporter');

const REPORT_EVERY_N_TRADES = 100;

export class MevStatsReporter {
  private logger: PaperTradeLogger;
  private lastReportedTotal = 0;
  private sigintHandler: (() => void) | null = null;

  constructor(logger: PaperTradeLogger) {
    this.logger = logger;
  }

  /** Call after each trade is recorded. Reports every REPORT_EVERY_N_TRADES. */
  onTrade(): void {
    const summary = this.logger.getSummary();
    if (summary.totalTrades > 0 && summary.totalTrades % REPORT_EVERY_N_TRADES === 0) {
      if (summary.totalTrades !== this.lastReportedTotal) {
        this.lastReportedTotal = summary.totalTrades;
        this.logSummary();
      }
    }
  }

  /** Register SIGINT handler to print stats before process exit. */
  registerSigint(): void {
    this.sigintHandler = () => {
      this.logSummary();
    };
    process.on('SIGINT', this.sigintHandler);
  }

  /** Deregister SIGINT handler (call on clean shutdown). */
  deregisterSigint(): void {
    if (this.sigintHandler) {
      process.off('SIGINT', this.sigintHandler);
      this.sigintHandler = null;
    }
  }

  logSummary(): void {
    const s = this.logger.getSummary();
    log.info(
      `📊 MEV Paper Stats | trades=${s.totalTrades} wins=${s.wins} losses=${s.losses} ` +
      `winRate=${(s.winRate * 100).toFixed(1)}% totalPnl=${s.totalPnlSol >= 0 ? '+' : ''}${s.totalPnlSol.toFixed(4)} SOL ` +
      `avgPnl=${s.avgPnlSol >= 0 ? '+' : ''}${s.avgPnlSol.toFixed(4)} SOL avgHold=${s.avgHoldMs.toFixed(0)}ms ` +
      `maxWin=${s.maxWinSol.toFixed(4)} SOL maxLoss=${s.maxLossSol.toFixed(4)} SOL`
    );
  }

  destroy(): void {
    this.deregisterSigint();
  }
}
