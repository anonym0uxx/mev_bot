/**
 * @module mev/backrun-engine
 * BackrunEngine: orchestrates the full momentum-backrun MEV pipeline.
 *
 *  BackrunDetector  →  PositionManager  →  PaperTradeLogger  →  MevStatsReporter
 *
 * Guards:
 *   - max_concurrent_positions
 *   - daily_loss_cap_sol  (resets at midnight UTC)
 *
 * start() / stop() lifecycle.
 */

import { PumpPortalClient } from '../feed/pump-portal';
import { MevConfig } from '../types/config';
import { TokenTradeEvent } from '../types/events';
import { BackrunDetector, BackrunOpportunity } from './detector';
import { PositionManager } from './position-manager';
import { PaperTradeLogger } from './paper-trade-logger';
import { JitoFailureHandler } from './jito-failure-handler';
import { MevStatsReporter } from './stats-reporter';
import { QualifiedMintCache } from './signal-bridge';
import { EntryRandomizer } from './entry-randomizer';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';

const log = createLogger('mev:backrun-engine');

export class BackrunEngine {
  private cfg: MevConfig;
  private feed: PumpPortalClient;

  private detector: BackrunDetector;
  private posManager: PositionManager;
  private tradeLogger: PaperTradeLogger;
  private jitoHandler: JitoFailureHandler;
  private statsReporter: MevStatsReporter;
  private qualifiedMints: QualifiedMintCache | undefined;
  private randomizer: EntryRandomizer;

  private running = false;

  // Daily loss tracking — reset at midnight UTC
  private dailyLossSol = 0;
  private dailyLossResetDay = -1; // UTC day of month

  // Bound listener refs for clean removal
  private onTokenTrade: ((event: TokenTradeEvent) => void) | null = null;
  private onMintTrade: ((event: TokenTradeEvent) => void) | null = null;

  constructor(cfg: MevConfig, feed: PumpPortalClient, qualifiedMints?: QualifiedMintCache) {
    this.cfg = cfg;
    this.feed = feed;
    this.qualifiedMints = qualifiedMints;

    this.detector = new BackrunDetector(cfg);
    this.posManager = new PositionManager(cfg);
    this.tradeLogger = new PaperTradeLogger(cfg.log_file);
    this.jitoHandler = new JitoFailureHandler();
    this.statsReporter = new MevStatsReporter(this.tradeLogger);
    this.randomizer = new EntryRandomizer(cfg);
  }

  start(): void {
    if (this.running) {
      log.warn('BackrunEngine already running');
      return;
    }
    this.running = true;

    log.info(
      `BackrunEngine started [paper_mode=${this.cfg.paper_mode}] ` +
      `maxPositions=${this.cfg.max_concurrent_positions} ` +
      `dailyCap=${this.cfg.daily_loss_cap_sol} SOL`
    );

    // Wire detector opportunities → position manager
    this.detector.on('opportunity', (opp: BackrunOpportunity) => {
      this.handleOpportunity(opp);
    });

    // Wire position closures → logger + stats
    this.posManager.on('closed', (record) => {
      // Track daily loss
      if (record.pnlSol < 0) {
        this.checkAndResetDailyLoss();
        this.dailyLossSol += Math.abs(record.pnlSol);
      }
      this.tradeLogger.record(record);
      this.statsReporter.onTrade();
    });

    // Listen to all tokenTrade events for scoring
    this.onTokenTrade = (event: TokenTradeEvent) => {
      if (!this.running) return;
      // Check existing positions BEFORE feeding to detector (so trigger event doesn't
      // immediately close a position opened in the same tick by the opportunity handler)
      const hadOpenPosition = this.posManager.hasPosition(event.mint);
      this.detector.onTrade(event);
      // Only route to position manager if position was already open before this event
      // (prevents the trigger trade from immediately closing the position it just opened)
      if (hadOpenPosition) {
        this.posManager.onSubsequentTrade(event);
      }
    };
    this.feed.on('tokenTrade', this.onTokenTrade);

    // Register stats reporter SIGINT handler
    this.statsReporter.registerSigint();

    log.info('BackrunEngine: listening on tokenTrade events');
  }

  stop(): void {
    if (!this.running) return;
    this.running = false;

    log.info('BackrunEngine stopping...');

    // Remove feed listener
    if (this.onTokenTrade) {
      this.feed.off('tokenTrade', this.onTokenTrade);
      this.onTokenTrade = null;
    }

    // Force-close any open positions
    this.posManager.closeAll();

    // Print final stats
    this.statsReporter.logSummary();
    this.statsReporter.destroy();

    // Destroy subsystems
    this.detector.destroy();
    this.posManager.destroy();

    log.info('BackrunEngine stopped');
  }

  private handleOpportunity(opp: BackrunOpportunity): void {
    if (!this.running) return;

    // Guard: max concurrent positions
    if (this.posManager.openCount >= this.cfg.max_concurrent_positions) {
      log.debug(
        `Skipping opportunity ${opp.mint.slice(0, 8)}: max_concurrent_positions reached ` +
        `(${this.posManager.openCount}/${this.cfg.max_concurrent_positions})`
      );
      return;
    }

    // Guard: already have a position for this mint
    if (this.posManager.hasPosition(opp.mint)) {
      if (this.cfg.conflict_policy === 'skip') {
        log.debug(`Skipping opportunity ${opp.mint.slice(0, 8)}: conflict_policy=skip`);
        return;
      }
    }

    // Guard: daily loss cap
    this.checkAndResetDailyLoss();
    if (this.dailyLossSol >= this.cfg.daily_loss_cap_sol) {
      log.warn(
        `Daily loss cap reached: ${this.dailyLossSol.toFixed(4)} SOL >= ` +
        `${this.cfg.daily_loss_cap_sol} SOL — no new MEV positions today`
      );
      return;
    }

    // Guard: Jito circuit breaker (paper mode: log-only, but still honour pauses)
    if (this.jitoHandler.isPaused()) {
      log.debug(`Skipping opportunity ${opp.mint.slice(0, 8)}: Jito circuit breaker active`);
      return;
    }

    // Guard: scalper pre-qualification (signal bridge)
    if (this.qualifiedMints && !this.qualifiedMints.has(opp.mint)) {
      log.debug(`[paper] skipped ${opp.mint.slice(0, 8)} — not in scalper hot-list`);
      return;
    }

    log.info(
      `🎯 Backrun opportunity: ${opp.mint.slice(0, 8)} score=${opp.score.toFixed(3)} ` +
      `vSol=${opp.entryVSol.toFixed(2)} buyers=${opp.uniqueBuyerCount} [paper]`
    );

    // Apply anti-fingerprinting: randomize entry size and delay
    const { delayMs, sizeSol } = this.randomizer.randomize();
    opp.recommendedSizeSol = sizeSol; // override with randomized size
    if (delayMs > 0) {
      setTimeout(() => this.posManager.openPosition(opp), delayMs);
    } else {
      this.posManager.openPosition(opp);
    }
  }

  private checkAndResetDailyLoss(): void {
    const utcDay = new Date().getUTCDate();
    if (utcDay !== this.dailyLossResetDay) {
      if (this.dailyLossResetDay !== -1) {
        log.info(`Daily loss counter reset (previous day loss: ${this.dailyLossSol.toFixed(4)} SOL)`);
      }
      this.dailyLossSol = 0;
      this.dailyLossResetDay = utcDay;
    }
  }
}
