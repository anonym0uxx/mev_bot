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
import { PositionManager, PnLRecord } from './position-manager';
import { PaperTradeLogger } from './paper-trade-logger';
import { JitoFailureHandler } from './jito-failure-handler';
import { MevStatsReporter } from './stats-reporter';
import { QualifiedMintCache } from './signal-bridge';
import { PoolDepthCache } from './pool-depth-cache';
import { EntryRandomizer } from './entry-randomizer';
import { JitoBundleBuilder } from './jito-bundle-builder';
import { SellExecutor } from './sell-executor';
import { EventEmitter } from 'events';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { AlertSystem } from '../alerts/system';

const log = createLogger('mev:backrun-engine');

export class BackrunEngine extends EventEmitter {
  private cfg: MevConfig;
  private feed: PumpPortalClient;

  private detector: BackrunDetector;
  private posManager: PositionManager;
  private tradeLogger: PaperTradeLogger;
  private jitoHandler: JitoFailureHandler;
  private statsReporter: MevStatsReporter;
  private qualifiedMints: QualifiedMintCache | undefined;
  private poolDepthCache: PoolDepthCache | undefined;
  private randomizer: EntryRandomizer;
  private jitoBundleBuilder: JitoBundleBuilder;
  private sellExecutor: SellExecutor;

  private running = false;

  // Consecutive stop circuit breaker
  private consecutiveStops = 0;
  private stopPauseUntilMs = 0;

  // Daily loss tracking — reset at midnight UTC
  private dailyLossSol = 0;
  private dailyLossResetDay = -1; // UTC day of month

  /** Effective daily loss cap: paper vs live mode-specific, falls back to daily_loss_cap_sol */
  private get effectiveDailyLossCap(): number {
    if (this.cfg.paper_mode) {
      return this.cfg.paper_daily_loss_cap_sol ?? this.cfg.daily_loss_cap_sol;
    }
    return this.cfg.live_daily_loss_cap_sol ?? this.cfg.daily_loss_cap_sol;
  }

  // Bound listener refs for clean removal
  private onTokenTrade: ((event: TokenTradeEvent) => void) | null = null;
  private onMintTrade: ((event: TokenTradeEvent) => void) | null = null;

  constructor(cfg: MevConfig, feed: PumpPortalClient, qualifiedMints?: QualifiedMintCache, poolDepthCache?: PoolDepthCache) {
    super();
    this.cfg = cfg;
    this.feed = feed;
    this.qualifiedMints = qualifiedMints;
    this.poolDepthCache = poolDepthCache;

    this.detector = new BackrunDetector(cfg);
    this.posManager = new PositionManager(cfg);
    this.tradeLogger = new PaperTradeLogger(cfg.log_file);
    this.jitoHandler = new JitoFailureHandler();
    this.statsReporter = new MevStatsReporter(this.tradeLogger);
    this.randomizer = new EntryRandomizer(cfg);
    this.jitoBundleBuilder = new JitoBundleBuilder(cfg);
    this.sellExecutor = new SellExecutor(cfg);
  }

  start(): void {
    if (this.running) {
      log.warn('BackrunEngine already running');
      return;
    }
    this.running = true;

    // Wire direct close callback for guaranteed alert delivery
    // (EventEmitter listeners can be silently interrupted; this callback cannot)
    this.posManager.onCloseCallback = (record) => {
      const listenerCount = this.listenerCount('trade');
      log.info(`[mev:trade-emit] listeners=${listenerCount} mint=${record.mint.slice(0,8)} pnl=${record.pnlSol.toFixed(4)}`);
      this.emit('trade', record);
    };

    log.info(
      `BackrunEngine started [paper_mode=${this.cfg.paper_mode}] ` +
      `maxPositions=${this.cfg.max_concurrent_positions} ` +
      `dailyCap=${this.effectiveDailyLossCap} SOL [${this.cfg.paper_mode ? 'paper' : 'live'}]`
    );

    // Wire detector opportunities → position manager
    this.detector.on('opportunity', (opp: BackrunOpportunity) => {
      this.handleOpportunity(opp);
    });

    // Wire position closures → execution layer + logger + stats + alerts
    this.posManager.on('closed', (record: PnLRecord) => {
      // Track daily loss
      if (record.pnlSol < 0) {
        this.checkAndResetDailyLoss();
        // Only count actual losses against the daily cap (negative PnL trades only)
        if (record.pnlSol < 0) {
          this.dailyLossSol += Math.abs(record.pnlSol);
        }
      }

      // Consecutive stop circuit breaker — reset on win, increment on loss
      if (record.exitReason === 'stop_loss') {
        this.consecutiveStops++;
        const pauseCount = this.cfg.consecutive_stop_pause_count ?? 3;
        const pauseMs = this.cfg.consecutive_stop_pause_ms ?? 180_000;
        if (this.consecutiveStops >= pauseCount) {
          this.stopPauseUntilMs = Date.now() + pauseMs;
          log.warn(`[circuit-breaker] ${this.consecutiveStops} consecutive stops — pausing new entries for ${pauseMs/1000}s`);
          this.consecutiveStops = 0;
        }
      } else if (record.exitReason === 'take_profit' || record.exitReason === 'next_buyer' || record.exitReason === 'intra_hold_trail') {
        this.consecutiveStops = 0;
      }

      // Execute sell (paper: logs simulation; live: submits via Helius staked RPC)
      this.sellExecutor.executeSell(record).catch((err: Error) => {
        log.warn(`SellExecutor error for ${record.mint.slice(0, 8)}: ${err.message}`);
      });

      this.tradeLogger.record(record);
      this.statsReporter.onTrade();
      // NOTE: trade alert is fired via posManager.onCloseCallback (set in start())
      // to guarantee delivery even if the EventEmitter chain is interrupted
    });

    // Listen to all tokenTrade events for scoring
    this.onTokenTrade = (event: TokenTradeEvent) => {
      if (!this.running) return;
      // Skip AMM (post-graduation Raydium) trades — MEV engine targets pre-graduation bonding curve only.
      // AMM trades have vSol=0 which fail Gate 3 anyway, but they still pollute detector trade history
      // and waste CPU cycles on score computation for tokens that have already graduated.
      if ((event as any).isAmm) return;
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

    // Clear direct callback before closing positions
    this.posManager.onCloseCallback = null;

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
    if (this.dailyLossSol >= this.effectiveDailyLossCap) {
      log.warn(
        `Daily loss cap reached: ${this.dailyLossSol.toFixed(4)} SOL >= ` +
        `${this.effectiveDailyLossCap} SOL [${this.cfg.paper_mode ? 'paper' : 'live'}] — no new MEV positions today`
      );
      return;
    }

    // Guard: Jito circuit breaker (paper mode: log-only, but still honour pauses)
    if (this.jitoHandler.isPaused()) {
      log.debug(`Skipping opportunity ${opp.mint.slice(0, 8)}: Jito circuit breaker active`);
      return;
    }

    // Guard: consecutive stop circuit breaker
    if (Date.now() < this.stopPauseUntilMs) {
      const remainingSec = ((this.stopPauseUntilMs - Date.now()) / 1000).toFixed(0);
      log.debug(`Skipping ${opp.mint.slice(0,8)}: consecutive stop pause active (${remainingSec}s remaining)`);
      return;
    }

    // Guard: scalper pre-qualification (signal bridge)
    // Skipped when use_scalper_prequalification=false — MEV runs independently
    if (this.cfg.use_scalper_prequalification !== false && this.qualifiedMints && !this.qualifiedMints.has(opp.mint)) {
      log.debug(`[paper] skipped ${opp.mint.slice(0, 8)} — not in scalper hot-list`);
      return;
    }

    // Guard: time-of-day gate (config-driven)
    if (this.cfg.tod_config?.blocked_hours_utc) {
      const hourUtcNow = new Date().getUTCHours();
      if (this.cfg.tod_config.blocked_hours_utc.includes(hourUtcNow)) {
        log.debug(`[gate:tod] Skipping ${opp.mint.slice(0,8)}: UTC hour ${hourUtcNow} is blocked`);
        return;
      }
    }

    // Guard: pool depth gate for graduated tokens
    // If this mint has migrated to Raydium but the pool is too shallow, skip entry.
    if (this.poolDepthCache?.hasMigrated(opp.mint)) {
      const minDepth = this.cfg.min_raydium_depth_sol ?? 5;
      if (!this.poolDepthCache.isDeep(opp.mint, minDepth)) {
        log.debug(`[backrun] Skipping ${opp.mint.slice(0, 8)}: migrated but pool depth too shallow`);
        return;
      }
    }

    log.info(
      `🎯 Backrun opportunity: ${opp.mint.slice(0, 8)} score=${opp.score.toFixed(3)} ` +
      `vSol=${opp.entryVSol.toFixed(2)} buyers=${opp.uniqueBuyerCount} [paper]`
    );

    // Tiered sizing by trigger buy size (quant-validated from 1,300-trade dataset)
    const triggerSol = opp.triggerEvent.solAmount;
    let dynamicBase = this.cfg.entry_size_sol;

    const sizeTiers = this.cfg.size_tiers;
    if (sizeTiers && sizeTiers.length > 0) {
      for (const tier of sizeTiers) {
        if (triggerSol <= tier.trigger_max_sol) {
          dynamicBase = tier.size_sol;
          break;
        }
      }
    } else {
      // Legacy fallback: curve + time-of-day sizing
      const curvePct = opp.entryVSol / 85 * 100;
      const hourUtc = new Date().getUTCHours();
      const isOffPeak = hourUtc >= 4 && hourUtc <= 9;
      const isOptimalCurve = curvePct >= 45 && curvePct < 55;
      const isGoodCurve = curvePct >= 38 && curvePct < 65;
      const isHighScore = opp.score >= 0.55 && opp.score < 0.70;

      if (isOptimalCurve && isOffPeak && isHighScore) {
        dynamicBase = this.cfg.max_entry_size_sol;
      } else if (isOptimalCurve || (isOffPeak && isHighScore)) {
        dynamicBase = this.cfg.entry_size_sol;
      } else if (isGoodCurve && isHighScore) {
        dynamicBase = this.cfg.entry_size_sol * 0.75;
      } else if (opp.score > 0.70) {
        dynamicBase = this.cfg.entry_size_sol * 0.50;
      } else {
        dynamicBase = this.cfg.entry_size_sol * 0.60;
      }
    }

    // Time-of-day position sizing (config-driven)
    const hourUtc = new Date().getUTCHours();
    let todMultiplier = 1.0;
    if (this.cfg.tod_config?.boosted_hours_utc?.includes(hourUtc)) {
      todMultiplier = 1.25;
    } else if (this.cfg.tod_config?.reduced_hours_utc?.includes(hourUtc)) {
      todMultiplier = 0.75;
    }
    dynamicBase = Math.min(dynamicBase * todMultiplier, this.cfg.max_entry_size_sol);
    log.debug(`[sizing] ToD multiplier=${todMultiplier} hour=${hourUtc}UTC base=${dynamicBase.toFixed(4)}`);

    // Skip zero-size tiers
    if (dynamicBase <= 0) {
      log.debug(`Skipping ${opp.mint.slice(0,8)}: tier size=0 for trigger=${triggerSol.toFixed(3)} SOL`);
      return;
    }

    const variance = this.cfg.size_variance_pct ?? 0.20;
    const low = dynamicBase * (1 - variance);
    const high = dynamicBase * (1 + variance);
    const { delayMs } = this.randomizer.randomize();
    const sizeSol = parseFloat((Math.random() * (high - low) + low).toFixed(4));
    opp.recommendedSizeSol = sizeSol;
    opp.todMultiplier = todMultiplier;

    log.debug(
      `Tiered sizing: trigger=${triggerSol.toFixed(3)} SOL → base=${dynamicBase.toFixed(3)} → final=${sizeSol.toFixed(4)} SOL`
    );

    const openAndBundle = () => {
      // Guard: engine may have stopped during the jitter delay — abort if so
      if (!this.running) return;
      this.posManager.openPosition(opp);

      // Emit entry event for external listeners (e.g. daemon alert system)
      this.emit('entry', { mint: opp.mint, sizeSol, score: opp.score, vSol: opp.entryVSol, paper: this.cfg.paper_mode });

      // Fire-and-forget Jito bundle (paper: logs simulation; live: submits bundle)
      this.jitoBundleBuilder.buildBundle({
        mint: opp.mint,
        sizeSol,
        tipLamports: this.cfg.jito_tip_lamports,
        paperMode: this.cfg.paper_mode,
        bondingCurve: opp.triggerEvent.bondingCurveKey,
        associatedBondingCurve: (opp.triggerEvent as any).associatedBondingCurve ?? opp.triggerEvent.bondingCurveKey,
        vSolLamports: BigInt(Math.floor(opp.triggerEvent.vSolInBondingCurve * 1e9)),
        vTokens: BigInt(Math.floor(opp.triggerEvent.vTokensInBondingCurve)),
        // buyerKeypair: not set here — live mode uses wallet-rotator separately
      }).catch((err: Error) => {
        log.warn(`JitoBundleBuilder error for ${opp.mint.slice(0, 8)}: ${err.message}`);
      });
    };

    if (delayMs > 0) {
      setTimeout(openAndBundle, delayMs);
    } else {
      openAndBundle();
    }
  }

  /**
   * Notify the detector that a creator sold tokens for this mint.
   * Called by daemon when corecast emits 'creatorSell' events.
   * The detector marks the mint and rejects future triggers for 30s.
   */
  onCreatorSell(mint: string): void {
    this.detector.onCreatorSell(mint);
  }

  /** Returns true if an open position exists for the given mint. */
  hasOpenPosition(mint: string): boolean {
    return this.posManager.hasPosition(mint);
  }

  /**
   * Force-close an open position for the given mint.
   * Used by the daemon for LP removal / external exit signals.
   */
  forceExit(mint: string, reason: string): void {
    if (!this.posManager.hasPosition(mint)) return;
    log.warn(`[backrun] forceExit triggered for ${mint.slice(0, 8)} reason=${reason}`);
    // Use max_hold exit path by closing via closeAll-equivalent with a targeted close
    // PositionManager doesn't expose a public closePosition directly, so we use a
    // private-access workaround via the event cycle: emit a synthetic sell at entry price
    // to trigger stop_loss path would be wrong — instead we expose a targeted close.
    // PositionManager.closeAll() closes everything — too broad.
    // We call posManager directly using the public interface.
    this.posManager.forceClosePosition(mint, reason as any);
  }

  /** Set or update the pool depth cache (can be injected post-construction). */
  setPoolDepthCache(cache: PoolDepthCache): void {
    this.poolDepthCache = cache;
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
