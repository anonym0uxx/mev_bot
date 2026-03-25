/**
 * @module daemon/index
 * Strategy daemon entry point: boots all subsystems.
 * Owns: market intake, rolling features, candidate state, entry/exit EV layers, risk state.
 *
 * This is the main process — all latency-sensitive decisions happen here.
 * Opus is NOT in the hot trading path.
 */

import dotenv from 'dotenv';
dotenv.config();

// Global safety net — log and continue, never crash silently
process.on('uncaughtException', (err) => {
  console.error(`[UNCAUGHT EXCEPTION] ${err.message}\n${err.stack}`);
});
process.on('unhandledRejection', (reason) => {
  console.error(`[UNHANDLED REJECTION] ${reason}`);
});

import { v4 as uuidv4 } from 'uuid';
import { createLogger } from '../utils/logger';
import { nowMs, ageS } from '../utils/time';
import { getConfigManager, getConfig, getConfigVersion } from '../config/loader';
import { getDatabase, PumpQuantDB } from '../persistence/database';
import { PumpPortalClient } from '../feed/pump-portal';
import { CoreCastClient } from '../feed/corecast';
import { BitqueryClient } from '../feed/bitquery';
import { classifyRegime, computeBondingCurveProgress, isTradeableRegime, detectMayhem, detectTokenizedAgent } from '../regime/classifier';
import { FeatureEngine } from '../features/engine';
import { fetchTokenMetadata } from '../features/multimodal-junk-filter';
import { TokenStateMachine } from '../state/machine';
import { computeProbabilities } from '../probability/layer';
import { evaluateEntry, computePositionSizing } from '../entry/engine';
import { evaluateExit } from '../exit/engine';
import { assessManipulationRisk } from '../manipulation/model';
import { ExecutionAdapter } from '../execution/adapter';
import { HealthMonitor, SystemHealth } from '../health/monitor';
import { AlertSystem } from '../alerts/system';
import { LearningLedger } from '../learning/ledger';
import { LearningJobScheduler } from '../learning/jobs';
import { startApiServer, DaemonContext } from './api';
import { isPaperMode } from '../paper/engine';
import { PumpQuantConfig, RouteMode } from '../types/config';
import {
  TokenState, Regime, CandidatePacket, AnalysisTier,
} from '../types/state';
import {
  NewTokenEvent, TokenTradeEvent, MigrationEvent,
} from '../types/events';
import { TradeIntent, Position, PositionStatus, OrderStatus } from '../types/trade';
import { TradeDataPoint, FeatureSnapshot } from '../types/features';

const log = createLogger('daemon');

class StrategyDaemon {
  private db: PumpQuantDB;
  private configManager = getConfigManager();
  private feed: PumpPortalClient;
  private corecast: CoreCastClient | null = null;
  private bitquery: BitqueryClient;
  private featureEngine: FeatureEngine;
  private stateMachine: TokenStateMachine;
  private executionAdapter: ExecutionAdapter;
  private healthMonitor: HealthMonitor;
  private alertSystem: AlertSystem;
  private learningLedger: LearningLedger;
  private learningJobs: LearningJobScheduler;
  private healthCheckInterval: NodeJS.Timeout | null = null;
  private analysisInterval: NodeJS.Timeout | null = null;
  private strategyProfile: string = 'default';
  private pendingExecutions: Set<string> = new Set();
  private forcedExitCooldowns: Map<string, number> = new Map();
  private lastExecutionAttempt: number = 0;
  private executionCooldownMs: number = 5000; // 5s between execution attempts
  private analysisLocks: Set<string> = new Set(); // Prevent concurrent analysis of same mint
  // Circuit breaker state
  private consecutiveLosses: number = 0;
  private circuitBreakerPauseUntil: number = 0;
  private entryEdgeMultiplier: number = 1.0; // Increased on L1, reset on win

  constructor() {
    // Load config
    const configPath = process.env.CONFIG_PATH || 'config/default.json';
    const config = this.configManager.loadFromFile(configPath);
    log.info(`Config loaded: v${this.configManager.getVersion()}`);

    // Initialize database
    this.db = getDatabase();

    // Persist initial config version
    this.db.insertConfigVersion({
      version: this.configManager.getVersion(),
      config,
      timestamp: nowMs(),
      source: 'file',
      description: `Initial load from ${configPath}`,
    });

    // Initialize subsystems
    const paper = isPaperMode();
    this.feed = new PumpPortalClient();
    // CoreCast: primary fast-lane when enabled
    if (config.corecast?.enabled) {
      this.corecast = new CoreCastClient(config.corecast);
      log.info('CoreCast enabled — primary fast-lane feed');
    }
    this.bitquery = new BitqueryClient();
    this.featureEngine = new FeatureEngine(config);
    this.stateMachine = new TokenStateMachine(this.db, config);
    this.executionAdapter = new ExecutionAdapter(this.db, config, paper);
    this.healthMonitor = new HealthMonitor(this.db, config);
    this.alertSystem = new AlertSystem(config);
    this.learningLedger = new LearningLedger(this.db);
    this.learningJobs = new LearningJobScheduler(this.db, this.configManager);

    log.info(`Daemon initialized (paper=${paper})`);
  }

  /** Start the daemon */
  async start(): Promise<void> {
    log.info('Starting strategy daemon...');

    // Wire up feed event handlers
    this.setupFeedHandlers();

    // Wire up CoreCast handlers if enabled (primary fast-lane)
    if (this.corecast) {
      this.setupCoreCastHandlers();
    }

    // Wire up state machine event handlers
    this.setupStateMachineHandlers();

    // Connect to CoreCast first (primary), then PumpPortal (execution + fallback)
    if (this.corecast) {
      try {
        await this.corecast.connect();
        this.healthMonitor.recordUpdate('market_feed');
        log.info('CoreCast primary feed connected');
      } catch (err) {
        log.warn(`CoreCast failed, falling back to PumpPortal: ${(err as Error).message}`);
      }
    }

    // Connect to PumpPortal (always needed for execution; also fallback market data)
    try {
      await this.feed.connect();
      if (!this.corecast?.connected) {
        this.healthMonitor.recordUpdate('market_feed');
        log.info('PumpPortal active as primary feed (CoreCast unavailable)');
      } else {
        log.info('PumpPortal connected (execution + supplemental context)');
      }
    } catch (err) {
      log.error(`Failed to connect to PumpPortal: ${(err as Error).message}`);
      if (!this.corecast?.connected) {
        this.healthMonitor.pause('All feeds failed');
      }
    }

    // Start health check loop
    const config = this.configManager.getConfig();
    this.healthCheckInterval = setInterval(() => {
      this.runHealthCheck();
    }, config.health.check_interval_s * 1000);

    // Start analysis loop (1s interval for fast-lane processing)
    this.analysisInterval = setInterval(() => {
      this.runAnalysisLoop();
    }, 1000);

    // Start learning jobs
    this.learningJobs.start();

    // Start HTTP API
    const apiPort = parseInt(process.env.DAEMON_PORT || '9420');
    const apiHost = process.env.DAEMON_HOST || '127.0.0.1';
    startApiServer(this.buildApiContext(), apiPort, apiHost);

    log.info('Strategy daemon started');
  }

  /** Stop the daemon */
  async stop(): Promise<void> {
    log.info('Stopping strategy daemon...');
    if (this.corecast) this.corecast.disconnect();
    this.feed.disconnect();
    this.learningJobs.stop();
    if (this.healthCheckInterval) clearInterval(this.healthCheckInterval);
    if (this.analysisInterval) clearInterval(this.analysisInterval);
    this.db.close();
    log.info('Strategy daemon stopped');
  }

  // ====== FEED HANDLERS ======

  private setupFeedHandlers(): void {
    // New token creation — only process from PumpPortal when CoreCast is not primary
    this.feed.on('newToken', (event: NewTokenEvent) => {
      if (this.corecast?.connected) return; // CoreCast is primary
      this.handleNewToken(event);
    });

    // Token trade — only process from PumpPortal when CoreCast is not primary
    let tradeCount = 0;
    this.feed.on('tokenTrade', (event: TokenTradeEvent) => {
      if (this.corecast?.connected) return; // CoreCast is primary
      tradeCount++;
      if (tradeCount % 100 === 1) {
        log.info(`PumpPortal trades (fallback): ${tradeCount} (latest: ${event.mint.slice(0,8)} ${event.txType} ${event.solAmount?.toFixed(4)} SOL)`);
      }
      this.handleTokenTrade(event);
    });

    // Migration — only from PumpPortal when CoreCast is not primary
    this.feed.on('migration', (event: MigrationEvent) => {
      if (this.corecast?.connected) return;
      this.handleMigration(event);
    });

    // Connection events
    this.feed.on('connected', () => {
      this.healthMonitor.recordUpdate('market_feed');
    });

    this.feed.on('disconnected', (reason: string) => {
      this.alertSystem.emitStaleFeed('market_feed', 0);
    });

    // Prevent unhandled 'error' event from crashing the process
    this.feed.on('error', (err: Error) => {
      log.warn(`Feed error (non-fatal, will reconnect): ${err.message}`);
    });
  }

  // ====== CORECAST HANDLERS (PRIMARY FAST-LANE) ======

  private setupCoreCastHandlers(): void {
    if (!this.corecast) return;

    // CoreCast new token → same handler as PumpPortal
    this.corecast.on('newToken', (event: NewTokenEvent) => {
      this.handleNewToken(event);
    });

    // CoreCast trade → same handler, but marks source
    let ccTradeCount = 0;
    this.corecast.on('tokenTrade', (event: TokenTradeEvent) => {
      ccTradeCount++;
      if (ccTradeCount % 100 === 1) {
        log.info(`CoreCast trades: ${ccTradeCount} (latest: ${event.mint.slice(0,8)} ${event.txType})`);
      }
      this.handleTokenTrade(event);
    });

    // CoreCast migration
    this.corecast.on('migration', (event: MigrationEvent) => {
      this.handleMigration(event);
    });

    this.corecast.on('connected', () => {
      this.healthMonitor.recordUpdate('market_feed');
      log.info('CoreCast fast-lane connected');
    });

    this.corecast.on('disconnected', (reason: string) => {
      log.warn(`CoreCast disconnected: ${reason}`);
      // PumpPortal will pick up as fallback
      if (this.feed.connected) {
        log.info('PumpPortal active as fallback feed');
      } else {
        this.alertSystem.emitStaleFeed('market_feed', 0);
      }
    });

    this.corecast.on('error', (err: Error) => {
      log.warn(`CoreCast error (non-fatal): ${err.message}`);
    });
  }

  /** Handle new token creation (Tier 0: discovery + instant exclusions) */
  private handleNewToken(event: NewTokenEvent): void {
    const config = this.configManager.getConfig();
    const now = nowMs();

    // Persist raw event
    this.db.insertRawEvent({
      type: 'new_token',
      data: event as any,
      timestamp: event.timestamp || now,
      received_at: now,
    });

    // Detect exclusions
    const isMayhem = detectMayhem(event.name, event.symbol, event.uri);
    const isTokenizedAgent = detectTokenizedAgent(event.name, event.symbol, event.uri);

    const bondingCurveProgress = computeBondingCurveProgress(event.vTokensInBondingCurve);

    // Classify regime
    const regime = classifyRegime({
      bondingCurveProgress,
      migrated: false,
      tokenAgeMs: 0,
      isMayhem,
      isTokenizedAgent,
      createdAt: now,
    }, config.regime);

    // Initialize in state machine
    const packet = this.stateMachine.initToken(
      event.mint, event.symbol, event.name,
      event.traderPublicKey, event.bondingCurveKey,
      event.uri, regime,
      event.vTokensInBondingCurve, event.vSolInBondingCurve,
      event.marketCapSol, bondingCurveProgress
    );

    // If excluded, ban immediately
    if (regime === Regime.EXCLUDED) {
      this.stateMachine.transitionToBan(event.mint, 'Excluded regime on creation');
      return;
    }

    // Initialize feature tracking
    this.featureEngine.initToken(event.mint, event.traderPublicKey);

    // Subscribe to token trades on both feeds
    this.feed.subscribeTokenTrades([event.mint]);
    if (this.corecast) this.corecast.watchMints([event.mint]);

    // Start async metadata fetch (non-blocking)
    fetchTokenMetadata(event.mint, event.uri, event.symbol, event.name)
      .then(ctx => {
        this.featureEngine.setMultimodalContext(event.mint, ctx);
      })
      .catch(() => { /* Non-blocking — failure acceptable */ });

    // Transition to WATCH after brief observation
    this.stateMachine.transitionToWatch(event.mint, 'New token discovered');

    this.healthMonitor.recordUpdate('market_feed');
  }

  /** Handle token trade event (Tier 1: live incremental scoring) */
  private handleTokenTrade(event: TokenTradeEvent): void {
    const now = nowMs();
    // Only process trades for tokens in our state machine
    const packet = this.stateMachine.getPacket(event.mint);
    if (!packet) return;

    // Persist raw event
    this.db.insertRawEvent({
      type: 'token_trade',
      data: event as any,
      timestamp: event.timestamp || now,
      received_at: now,
    });

    // Add trade to feature engine
    const tradePoint: TradeDataPoint = {
      timestamp: event.timestamp || now,
      txType: event.txType,
      solAmount: event.solAmount,
      tokenAmount: event.tokenAmount,
      traderPublicKey: event.traderPublicKey,
      vTokensInBondingCurve: event.vTokensInBondingCurve,
      vSolInBondingCurve: event.vSolInBondingCurve,
      marketCapSol: event.marketCapSol,
      newTokenBalance: event.newTokenBalance,
    };

    this.featureEngine.addTrade(event.mint, tradePoint);

    // Update market data in state machine
    const bondingCurveProgress = computeBondingCurveProgress(event.vTokensInBondingCurve);
    this.stateMachine.updateMarketData(
      event.mint,
      event.vTokensInBondingCurve,
      event.vSolInBondingCurve,
      event.marketCapSol,
      bondingCurveProgress
    );

    // Update position if we hold this token
    this.updatePositionMarketData(event.mint, event);

    this.healthMonitor.recordUpdate('market_feed');
  }

  /** Handle migration event */
  private handleMigration(event: MigrationEvent): void {
    const now = nowMs();

    this.db.insertRawEvent({
      type: 'migration',
      data: event as any,
      timestamp: event.timestamp || now,
      received_at: now,
    });

    // Ban migrated tokens (post-migration excluded in initial build)
    this.stateMachine.transitionToBan(event.mint, 'Token migrated — post-migration excluded');
    this.featureEngine.removeToken(event.mint);
    this.feed.unsubscribeTokenTrades([event.mint]);
    if (this.corecast) this.corecast.unwatchMints([event.mint]);
  }

  // ====== STATE MACHINE HANDLERS ======

  private setupStateMachineHandlers(): void {
    this.stateMachine.on('stateChange', (mint: string, from: TokenState, to: TokenState, reason: string) => {
      log.debug(`State: ${mint} ${from} → ${to}: ${reason}`);
    });

    this.stateMachine.on('ban', (mint: string, reason: string) => {
      // Cleanup resources for banned tokens
      this.featureEngine.removeToken(mint);
      this.feed.unsubscribeTokenTrades([mint]);
      if (this.corecast) this.corecast.unwatchMints([mint]);

      // Force exit if we have a position
      const position = this.db.getPositionByMint(mint);
      if (position && (position.status === 'open' || position.status === 'reducing')) {
        this.executeForcedExit(mint, `BAN: ${reason}`);
      }
    });
  }

  // ====== ANALYSIS LOOP ======

  /** Run analysis on all active tokens (1s cadence) */
  private runAnalysisLoop(): void {
    const config = this.configManager.getConfig();

    // Always record subsystem liveness — friction/datastore/execution use default
    // estimates when no trade data exists yet (cold-start bootstrap)
    this.healthMonitor.recordUpdate('friction_estimate');
    this.healthMonitor.recordUpdate('datastore');
    this.healthMonitor.recordUpdate('execution_adapter');

    const health = this.healthMonitor.check();

    // If paused by operator, still run analysis for state transitions but skip entry
    const activePackets = this.stateMachine.getActivePackets();

    for (const packet of activePackets) {
      try {
        this.analyzeToken(packet, config, health);
      } catch (err) {
        log.error(`Analysis error for ${packet.mint}: ${(err as Error).message}`);
      }
    }

    this.healthMonitor.recordUpdate('probability_layer');
  }

  /** Analyze a single token: compute features, probabilities, entry/exit decisions */
  private analyzeToken(packet: CandidatePacket, config: PumpQuantConfig, health: SystemHealth): void {
    const mint = packet.mint;

    // Prevent concurrent analysis of the same mint (dedup across event-driven + interval ticks)
    if (this.analysisLocks.has(mint)) return;
    this.analysisLocks.add(mint);
    // Release lock after analysis completes (sync code, but protects against re-entry)
    try {
      this._analyzeTokenInner(packet, config, health);
    } finally {
      this.analysisLocks.delete(mint);
    }
  }

  private _analyzeTokenInner(packet: CandidatePacket, config: PumpQuantConfig, health: SystemHealth): void {
    const mint = packet.mint;

    // Compute features
    const features = this.featureEngine.computeFeatures(mint);
    if (!features) return;

    // Re-classify regime
    const regime = classifyRegime({
      bondingCurveProgress: packet.bonding_curve_progress,
      migrated: false,
      tokenAgeMs: nowMs() - packet.created_at,
      isMayhem: false,
      isTokenizedAgent: false,
      createdAt: packet.created_at,
    }, config.regime);

    // Check for regime changes → ban if now excluded
    if (!isTradeableRegime(regime)) {
      this.stateMachine.transitionToBan(mint, `Regime changed to ${regime}`);
      return;
    }

    // Compute probabilities
    const probabilities = computeProbabilities(features, regime, config);

    // Manipulation check
    const manipAssessment = assessManipulationRisk(
      features.manipulation_distribution, config.manipulation
    );
    const tokenAgeSec = ageS(packet.created_at);

    // For LONG positions: any hard shock → forced exit immediately regardless of age
    if (manipAssessment.hardShock && packet.state === TokenState.LONG) {
      if (!this.pendingExecutions.has(mint) && !this.forcedExitCooldowns.has(mint)) {
        this.pendingExecutions.add(mint);
        this.executeForcedExit(mint, `Manipulation shock: ${manipAssessment.hardShockReason}`)
          .catch(() => {
            // On failure, set cooldown to prevent retry spam (5s between retries)
            this.forcedExitCooldowns.set(mint, Date.now() + 5000);
            setTimeout(() => this.forcedExitCooldowns.delete(mint), 5000);
          })
          .finally(() => this.pendingExecutions.delete(mint));
      }
      return;
    }

    // For non-position tokens: only hard-ban on creator_sell immediately.
    // Other hard shocks need the token to be past the observation window,
    // because concentration_worsening, slippage_shock, same_size_prints are
    // expected in the first seconds of a token's life with very few trades.
    if (manipAssessment.hardShock) {
      const isCreatorSell = manipAssessment.hardShockReason === 'creator_sell';
      if (isCreatorSell || tokenAgeSec > config.entry.observation_window_s) {
        this.stateMachine.transitionToBan(mint, `Manipulation shock: ${manipAssessment.hardShockReason}`);
        return;
      }
      // Young token with non-creator hard shock: log but don't ban yet
    }

    // State-specific logic
    let entryEv = null;
    let exitEv = null;
    let sizing = null;

    if (packet.state === TokenState.WATCH || packet.state === TokenState.ENTER_READY) {
      // Entry evaluation
      const currentPositionCount = this.db.getOpenPositionCount();
      const dailyLoss = this.db.getDailyPnl();
      // Apply circuit breaker edge multiplier (L1: 1.5x edge required)
      const effectiveMinEdge = (config.entry.min_entry_edge || 0.0005) * this.entryEdgeMultiplier;
      const entryDecision = evaluateEntry(
        packet, probabilities, features,
        { ...config, entry: { ...config.entry, min_entry_edge: effectiveMinEdge } },
        currentPositionCount, dailyLoss, isPaperMode()
      );

      entryEv = entryDecision.ev;
      sizing = entryDecision.sizing;

      if (packet.state === TokenState.WATCH && entryDecision.shouldEnter) {
        // GUARD: multiple layers to prevent duplicate entries
        if (this.pendingExecutions.has(mint)) return;
        if (this.analysisLocks.has(mint + '_entry')) return;
        const existingPosition = this.db.getPositionByMint(mint);
        if (existingPosition) return;
        // Double-check state hasn't changed (another tick may have already transitioned it)
        const freshPacket = this.stateMachine.getPacket(mint);
        if (!freshPacket || freshPacket.state !== TokenState.WATCH) return;

        log.info(`🚀 Promoting to ENTER_READY: ${packet.symbol || mint.slice(0,8)} — ${entryDecision.reason}`);
        this.stateMachine.transitionToEnterReady(mint, entryDecision.reason);

        const now = nowMs();
        const openPositionCount = this.db.getOpenPositions().length;
        const totalPending = openPositionCount + this.pendingExecutions.size;
        if (health.tradingAllowed && now > this.circuitBreakerPauseUntil && totalPending < config.risk.max_positions && (now - this.lastExecutionAttempt) > this.executionCooldownMs) {
          this.pendingExecutions.add(mint);
          this.analysisLocks.add(mint + '_entry'); // Hard lock until confirmed/failed
          this.lastExecutionAttempt = now;
          log.info(`💰 EXECUTING ENTRY: ${packet.symbol || mint.slice(0,8)} size=${entryDecision.sizing!.position_size.toFixed(4)} SOL (positions: ${openPositionCount}+${this.pendingExecutions.size} pending)`);
          this.executeEntry(packet, entryDecision.sizing!, config).finally(() => {
            this.pendingExecutions.delete(mint);
            this.analysisLocks.delete(mint + '_entry');
          });
        }
      }
    }

    if (packet.state === TokenState.LONG || packet.state === TokenState.REDUCE) {
      // Exit evaluation
      const position = this.db.getPositionByMint(mint);
      if (position) {
        const exitDecision = evaluateExit(packet, position, probabilities, features, config);
        exitEv = exitDecision.ev;

        if (exitDecision.shouldExit && !this.pendingExecutions.has(mint)) {
          this.pendingExecutions.add(mint);
          this.executeExit(mint, position, exitDecision.exitPct, exitDecision.reason, config)
            .finally(() => this.pendingExecutions.delete(mint));
        } else if (exitDecision.shouldReduce && !this.pendingExecutions.has(mint)) {
          this.pendingExecutions.add(mint);
          this.executeReduce(mint, position, exitDecision.exitPct, exitDecision.reason, config)
            .finally(() => this.pendingExecutions.delete(mint));
        }
      }
    }

    // Update state machine with analysis results
    this.stateMachine.updateAnalysis(
      mint, features, probabilities, regime, entryEv, exitEv, sizing
    );

    // Periodic cleanup: remove old EXIT/BAN tokens
    if (packet.state === TokenState.EXIT || packet.state === TokenState.BAN) {
      if (ageS(packet.state_entered_at) > 60) {
        this.stateMachine.cleanup(mint);
        this.featureEngine.removeToken(mint);
        this.feed.unsubscribeTokenTrades([mint]);
        if (this.corecast) this.corecast.unwatchMints([mint]);
      }
    }
  }

  // ====== TRADE EXECUTION ======

  private async executeEntry(
    packet: CandidatePacket,
    sizing: { position_size: number; limiting_factor: string },
    config: PumpQuantConfig
  ): Promise<void> {
    const intent: TradeIntent = {
      id: uuidv4(),
      mint: packet.mint,
      side: 'buy',
      size_sol: sizing.position_size,
      slippage_bps: config.execution.default_slippage_bps,
      priority_fee_sol: config.execution.default_priority_fee_sol,
      route_mode: config.execution.default_route_mode,
      reason: `Entry: edge=${packet.entry_ev?.EntryEdge?.toFixed(6) || 'N/A'}`,
      config_version: getConfigVersion(),
      created_at: nowMs(),
      ev_at_intent: packet.entry_ev?.EV_enter_now || 0,
    };

    this.db.insertTradeIntent(intent);

    try {
      const order = await this.executionAdapter.executeBuy(intent);

      if (order.status === OrderStatus.CONFIRMED) {
        // Fetch actual token balance from chain after buy
        // PumpPortal doesn't return token amount, so we estimate from bonding curve
        // For position tracking, we use entry_sol as the SOL spent, and
        // estimate tokens from the bonding curve price at time of buy
        const estimatedTokens = packet.v_tokens_in_curve > 0 && packet.v_sol_in_curve > 0
          ? intent.size_sol / (packet.v_sol_in_curve / packet.v_tokens_in_curve)
          : intent.size_sol * 1_000_000; // Fallback estimate
        const entryTokens = estimatedTokens;

        // Create position
        const position: Position = {
          id: uuidv4(),
          mint: packet.mint,
          symbol: packet.symbol,
          name: packet.name,
          regime: packet.regime,
          entry_order_id: order.id,
          entry_price_sol: order.realized_price || intent.size_sol,
          entry_sol: order.realized_sol || intent.size_sol,
          entry_tokens: entryTokens,
          entry_timestamp: nowMs(),
          entry_route_mode: intent.route_mode,
          entry_config_version: intent.config_version,
          current_tokens: entryTokens,
          current_value_sol: order.realized_sol || intent.size_sol,
          unrealized_pnl_sol: 0,
          unrealized_pnl_pct: 0,
          peak_net_exit_value: 0,
          exit_orders: [],
          exit_price_sol: null,
          exit_sol: null,
          exit_timestamp: null,
          exit_reason: null,
          exit_route_mode: null,
          realized_pnl_sol: null,
          realized_pnl_pct: null,
          total_fees_sol: order.fee_sol || 0,
          status: PositionStatus.OPEN,
          opened_at: nowMs(),
          closed_at: null,
          hold_duration_s: 0,
          mfe_sol: 0,
          mae_sol: 0,
          is_paper: isPaperMode(),
          config_version: intent.config_version,
        };

        this.db.insertPosition(position);
        this.stateMachine.transitionToLong(packet.mint, 'Buy confirmed');

        // Alert
        this.alertSystem.emitBuyFilled(
          packet.mint, packet.symbol, intent.size_sol, order.realized_price || 0
        );

        // Learning ledger
        this.learningLedger.recordEntry(
          packet, intent.route_mode, intent.config_version,
          'enter', null
        );

        this.healthMonitor.recordUpdate('execution_adapter');
      }
    } catch (err) {
      log.error(`Entry execution failed for ${packet.mint}: ${(err as Error).message}`);
      this.alertSystem.emitExecutionFailure(packet.mint, (err as Error).message);
    }
  }

  private async executeExit(
    mint: string,
    position: Position,
    exitPct: number,
    reason: string,
    config: PumpQuantConfig
  ): Promise<void> {
    const intent: TradeIntent = {
      id: uuidv4(),
      mint,
      side: 'sell',
      size_sol: position.current_value_sol * (exitPct / 100),
      amount_pct: exitPct,
      slippage_bps: config.execution.default_slippage_bps,
      priority_fee_sol: config.execution.default_priority_fee_sol,
      route_mode: config.execution.default_route_mode,
      reason: `Exit: ${reason}`,
      config_version: getConfigVersion(),
      created_at: nowMs(),
      ev_at_intent: 0,
    };

    this.db.insertTradeIntent(intent);

    try {
      const order = await this.executionAdapter.executeSell(intent);

      if (order.status === OrderStatus.CONFIRMED) {
        const realizedSol = order.realized_sol || 0;
        const pnl = realizedSol - position.entry_sol;

        // Update position
        this.db.updatePosition(position.id, {
          current_tokens: 0,
          current_value_sol: 0,
          exit_orders: [...position.exit_orders, order.id],
          exit_price_sol: order.realized_price || 0,
          exit_sol: realizedSol,
          exit_timestamp: nowMs(),
          exit_reason: reason as any,
          exit_route_mode: intent.route_mode,
          realized_pnl_sol: pnl,
          realized_pnl_pct: position.entry_sol > 0 ? pnl / position.entry_sol : 0,
          total_fees_sol: position.total_fees_sol + (order.fee_sol || 0),
          status: PositionStatus.CLOSED,
          closed_at: nowMs(),
          hold_duration_s: ageS(position.entry_timestamp),
        });

        this.stateMachine.transitionToExit(mint, reason);

        // Circuit breaker tracking
        this.updateCircuitBreaker(pnl);

        // Alert
        this.alertSystem.emitFullExit(mint, position.symbol, pnl, reason);

        // Learning
        const packet = this.stateMachine.getPacket(mint);
        if (packet) {
          this.learningLedger.recordExit(
            packet, intent.route_mode, intent.config_version,
            pnl, position.mfe_sol, position.mae_sol, 0,
            'exit', null
          );
        }

        this.healthMonitor.recordUpdate('execution_adapter');
      }
    } catch (err) {
      log.error(`Exit execution failed for ${mint}: ${(err as Error).message}`);
      this.alertSystem.emitExecutionFailure(mint, (err as Error).message);
    }
  }

  private async executeReduce(
    mint: string,
    position: Position,
    reducePct: number,
    reason: string,
    config: PumpQuantConfig
  ): Promise<void> {
    const intent: TradeIntent = {
      id: uuidv4(),
      mint,
      side: 'sell',
      size_sol: position.current_value_sol * (reducePct / 100),
      amount_pct: reducePct,
      slippage_bps: config.execution.default_slippage_bps,
      priority_fee_sol: config.execution.default_priority_fee_sol,
      route_mode: config.execution.default_route_mode,
      reason: `Reduce ${reducePct}%: ${reason}`,
      config_version: getConfigVersion(),
      created_at: nowMs(),
      ev_at_intent: 0,
    };

    this.db.insertTradeIntent(intent);

    try {
      const order = await this.executionAdapter.executeSell(intent);
      if (order.status === OrderStatus.CONFIRMED) {
        const remainingTokens = position.current_tokens * (1 - reducePct / 100);
        this.db.updatePosition(position.id, {
          current_tokens: remainingTokens,
          exit_orders: [...position.exit_orders, order.id],
          total_fees_sol: position.total_fees_sol + (order.fee_sol || 0),
          status: PositionStatus.REDUCING,
        });

        this.stateMachine.transitionToReduce(mint, reason);
        this.alertSystem.emitReduceFilled(mint, position.symbol, reducePct, order.realized_sol || 0);
      }
    } catch (err) {
      log.error(`Reduce failed for ${mint}: ${(err as Error).message}`);
    }
  }

  private async executeForcedExit(mint: string, reason: string): Promise<void> {
    const position = this.db.getPositionByMint(mint);
    if (!position) return;

    log.warn(`FORCED EXIT: ${mint} — ${reason}`);
    await this.executeExit(mint, position, 100, reason, this.configManager.getConfig());
    this.alertSystem.emitForcedExit(mint, position.symbol, reason);
  }

  // ====== POSITION UPDATES ======

  private updatePositionMarketData(mint: string, event: TokenTradeEvent): void {
    const position = this.db.getPositionByMint(mint);
    if (!position) return;

    // Estimate current value from bonding curve
    const pricePerToken = event.vSolInBondingCurve > 0 && event.vTokensInBondingCurve > 0
      ? event.vSolInBondingCurve / event.vTokensInBondingCurve
      : 0;

    const currentValue = position.current_tokens * pricePerToken;
    const unrealizedPnl = currentValue - position.entry_sol;
    const unrealizedPnlPct = position.entry_sol > 0 ? unrealizedPnl / position.entry_sol : 0;

    // Track MFE/MAE
    const mfe = Math.max(position.mfe_sol, unrealizedPnl);
    const mae = Math.min(position.mae_sol, unrealizedPnl);

    this.db.updatePosition(position.id, {
      current_value_sol: currentValue,
      unrealized_pnl_sol: unrealizedPnl,
      unrealized_pnl_pct: unrealizedPnlPct,
      hold_duration_s: ageS(position.entry_timestamp),
      mfe_sol: mfe,
      mae_sol: mae,
    });
  }

  // ====== HEALTH CHECK ======

  private runHealthCheck(): void {
    const health = this.healthMonitor.check();
    const config = this.configManager.getConfig();

    // Check feed staleness — CoreCast is primary when available
    const primaryFeed = this.corecast?.connected ? this.corecast : this.feed;
    const lastMsg = primaryFeed === this.corecast
      ? (this.corecast?.lastMessageTime || 0)
      : this.feed.lastMessageTime;

    if (ageS(lastMsg) > config.health.market_feed_stale_s) {
      // If CoreCast is stale but PumpPortal is fresh, that's still OK
      if (this.corecast?.connected && this.feed.connected &&
          ageS(this.feed.lastMessageTime) <= config.health.market_feed_stale_s) {
        this.healthMonitor.recordUpdate('market_feed');
      }
      // Otherwise stale
    } else {
      this.healthMonitor.recordUpdate('market_feed');
    }
  }

  // ====== API CONTEXT ======

  private buildApiContext(): DaemonContext {
    return {
      getTopCandidates: (limit = 10) => {
        return this.stateMachine.getTopCandidates(limit);
      },

      inspectCandidate: (mint: string) => {
        return this.stateMachine.getPacket(mint) || null;
      },

      buyToken: async (mint: string, sizeSol: number, slippageBps: number, priorityFeeSol: number, routeMode: string) => {
        const packet = this.stateMachine.getPacket(mint);
        if (!packet) throw new Error(`Token ${mint} not found`);
        if (packet.state !== TokenState.ENTER_READY && packet.state !== TokenState.WATCH) {
          throw new Error(`Token not in entry state (current: ${packet.state})`);
        }

        const config = this.configManager.getConfig();
        const intent: TradeIntent = {
          id: uuidv4(),
          mint,
          side: 'buy',
          size_sol: sizeSol,
          slippage_bps: slippageBps,
          priority_fee_sol: priorityFeeSol,
          route_mode: (routeMode || config.execution.default_route_mode) as RouteMode,
          reason: 'Manual buy via plugin',
          config_version: getConfigVersion(),
          created_at: nowMs(),
          ev_at_intent: packet.entry_ev?.EV_enter_now || 0,
        };

        this.db.insertTradeIntent(intent);
        return this.executionAdapter.executeBuy(intent);
      },

      sellToken: async (mint: string, amountPct: number, slippageBps: number, priorityFeeSol: number, routeMode: string, reason: string) => {
        const position = this.db.getPositionByMint(mint);
        if (!position) throw new Error(`No open position for ${mint}`);

        const config = this.configManager.getConfig();
        const intent: TradeIntent = {
          id: uuidv4(),
          mint,
          side: 'sell',
          size_sol: position.current_value_sol * (amountPct / 100),
          amount_pct: amountPct,
          slippage_bps: slippageBps,
          priority_fee_sol: priorityFeeSol,
          route_mode: (routeMode || config.execution.default_route_mode) as RouteMode,
          reason: reason || 'Manual sell via plugin',
          config_version: getConfigVersion(),
          created_at: nowMs(),
          ev_at_intent: 0,
        };

        this.db.insertTradeIntent(intent);
        return this.executionAdapter.executeSell(intent);
      },

      getPositions: () => {
        return this.db.getOpenPositions();
      },

      pauseTrading: (reason: string) => {
        this.healthMonitor.pause(reason);
        this.alertSystem.emitAutoPause(reason);
      },

      resumeTrading: () => {
        this.healthMonitor.resume();
      },

      getBotHealth: () => {
        return this.healthMonitor.check();
      },

      getRiskSettings: () => {
        return this.configManager.getConfig().risk;
      },

      updateRiskSettings: (settings: Record<string, unknown>) => {
        const patch = { risk: settings };
        return this.configManager.applyPatch(patch as any, 'operator', 'Risk settings updated via plugin');
      },

      getStrategyProfile: () => {
        return this.strategyProfile;
      },

      setStrategyProfile: (name: string) => {
        const validProfiles = ['default', 'canary', 'aggressive', 'conservative'];
        if (!validProfiles.includes(name)) {
          throw new Error(`Unknown profile: ${name}. Valid: ${validProfiles.join(', ')}`);
        }
        this.strategyProfile = name;
        if (name !== 'default') {
          this.configManager.loadFromFile(`config/${name}.json`);
        } else {
          this.configManager.loadFromFile('config/default.json');
        }
        log.info(`Strategy profile set to: ${name}`);
      },

      getRuntimeConfig: () => {
        return this.configManager.getConfig() as PumpQuantConfig;
      },

      updateRuntimeConfig: (patch: Record<string, unknown>) => {
        return this.configManager.applyPatch(patch as any, 'operator', 'Config updated via plugin');
      },
    };
  }

  // ====== CIRCUIT BREAKER ======

  /**
   * Update circuit breaker state after each closed trade.
   * L1: 3 consecutive losses → require 1.5x edge for 10 min
   * L2: 5 consecutive losses → pause new entries for 30 min
   */
  private updateCircuitBreaker(pnl: number): void {
    if (pnl >= 0) {
      // Win resets the counter
      if (this.consecutiveLosses > 0) {
        log.info(`Circuit breaker reset: ${this.consecutiveLosses} consecutive losses cleared by win`);
      }
      this.consecutiveLosses = 0;
      this.entryEdgeMultiplier = 1.0;
      return;
    }

    this.consecutiveLosses++;
    log.warn(`Circuit breaker: ${this.consecutiveLosses} consecutive losses`);

    if (this.consecutiveLosses >= 5) {
      // L2: pause for 30 min
      const pauseMs = 30 * 60 * 1000;
      this.circuitBreakerPauseUntil = nowMs() + pauseMs;
      this.entryEdgeMultiplier = 2.0;
      log.warn(`🔴 Circuit breaker L2: 5 consecutive losses — pausing entries for 30 min`);
      this.alertSystem.emit('circuit_breaker', `🔴 Circuit breaker L2: 5 consecutive losses. Pausing entries 30 min.`, {});
    } else if (this.consecutiveLosses >= 3) {
      // L1: tighten edge requirement for 10 min
      this.entryEdgeMultiplier = 1.5;
      const tightenUntil = new Date(nowMs() + 10 * 60 * 1000).toLocaleTimeString();
      log.warn(`🟡 Circuit breaker L1: 3 consecutive losses — requiring 1.5x edge until ${tightenUntil}`);
      this.alertSystem.emit('circuit_breaker', `🟡 Circuit breaker L1: 3 consecutive losses. 1.5x edge required until ${tightenUntil}.`, {});
    }
  }
}

// ====== MAIN ======

async function main(): Promise<void> {
  log.info('=== Pump Quant Bot ===');
  log.info(`Mode: ${isPaperMode() ? 'PAPER' : 'LIVE'}`);

  const daemon = new StrategyDaemon();

  // Graceful shutdown
  process.on('SIGINT', async () => {
    log.info('Received SIGINT, shutting down...');
    await daemon.stop();
    process.exit(0);
  });

  process.on('SIGTERM', async () => {
    log.info('Received SIGTERM, shutting down...');
    await daemon.stop();
    process.exit(0);
  });

  await daemon.start();
}

main().catch(err => {
  log.error(`Fatal error: ${err.message}`);
  process.exit(1);
});
