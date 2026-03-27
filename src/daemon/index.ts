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
import { CoreCastClient } from '../feed/corecast-v2';
import { CoreCastV3Client } from '../feed/corecast-v3';
import { BitqueryClient } from '../feed/bitquery';
import { SocialCache } from '../feed/social-cache';
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
import { loadThresholdState, getStats as getThresholdStats } from '../threshold/manager';
import { LearningJobScheduler } from '../learning/jobs';
import { startApiServer, DaemonContext } from './api';
import { isPaperMode } from '../paper/engine';
import { PumpQuantConfig, RouteMode } from '../types/config';
import { BackrunEngine } from '../mev/backrun-engine';
import { QualifiedMintCache } from '../mev/signal-bridge';
import { PoolDepthCache, PoolUpdate } from '../mev/pool-depth-cache';
import { WhaleTracker, WhaleBuyEvent } from '../feed/whale-tracker';
import {
  TokenState, Regime, CandidatePacket, AnalysisTier, ExitReason,
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
  private corecast: CoreCastClient | CoreCastV3Client | null = null;
  private socialCache = new SocialCache();
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
  private backrunEngine: BackrunEngine | null = null;
  private qualifiedMintCache: QualifiedMintCache = new QualifiedMintCache();
  private poolDepthCache: PoolDepthCache = new PoolDepthCache();
  private whaleTracker: WhaleTracker = new WhaleTracker();
  private pendingExecutions: Set<string> = new Set();
  private forcedExitCooldowns: Map<string, number> = new Map();

  /** Set to true immediately on SIGTERM — gates new entries and stream event processing. */
  private isShuttingDown: boolean = false;

  /**
   * Tracks in-flight executeExit() / executeEntry() promises for graceful drain on stop().
   * stop() awaits all in-flight trades (up to 15s) before closing DB.
   * Prevents ghost open positions from SIGTERM racing mid-exit-confirmation.
   */
  private _inflightTrades: Set<Promise<void>> = new Set();
  private _trackTrade(p: Promise<void>): Promise<void> {
    this._inflightTrades.add(p);
    p.finally(() => this._inflightTrades.delete(p));
    return p;
  }
  // Cross-feed dedup: prevents gRPC + PumpPortal from double-adding same trade to featureEngine
  private tradeDedupSet: Set<string> = new Set();
  private tradeDedupOrder: string[] = [];
  private readonly TRADE_DEDUP_MAX = 50_000;
  private lastExecutionAttempt: number = 0;
  private executionCooldownMs: number = 5000; // 5s between execution attempts
  private analysisLocks: Set<string> = new Set(); // Prevent concurrent analysis of same mint
  // Committed mints: set the moment we decide to buy, cleared only after DB position confirmed or failed.
  // This is the definitive guard against duplicate entries — survives async gaps where pendingExecutions
  // is cleared before the position record is written.
  private committedMints: Set<string> = new Set();
  // Circuit breaker state
  // ARCHITECTURE: CB modulates EXPOSURE (size + pauses), NEVER edge thresholds.
  // Multiplying edge thresholds without model ceiling awareness causes deadlocks.
  // L1 = size cut, L2 = full pause, L3 = session halt. No multipliers. No recovery traps.
  private consecutiveLosses: number = 0;
  private circuitBreakerPauseUntil: number = 0;
  private circuitBreakerLevel: 0 | 1 | 2 | 3 = 0;
  private circuitBreakerSizeMul: number = 1.0; // L1: 0.5x, others: 1.0x
  private circuitBreakerDeadlockWatchdogAt: number = 0; // timestamp of last trade attempt or CB transition

  /**
   * Config change epoch: set whenever config is reloaded mid-session.
   * getDailyPnl(configChangeEpoch) returns only losses AFTER this point,
   * so a new max_daily_loss_sol limit applies to the new config's trades
   * rather than being blocked by the old session's accumulated losses.
   */
  private configChangeEpoch: number = nowMs(); // Initialized to daemon start time

  /**
   * Session absolute start: set ONCE when the daemon boots, NEVER reset on config changes.
   * Used by the L3 circuit breaker so an operator cannot bypass the 24h halt by simply
   * reloading config. A -0.30 SOL session loss is a risk-of-ruin signal — it persists
   * across config changes within the same daemon process lifetime.
   */
  private readonly sessionAbsoluteStartMs: number = nowMs();
  // REMOVED: entryEdgeMultiplier — was root cause of the 2x deadlock incident

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
    // Use v3 (gRPC) if grpc_enabled flag is set, otherwise fall back to v2 (polling)
    if (config.corecast?.enabled) {
      const useV3 = (config.corecast as any).grpc_enabled === true;
      if (useV3) {
        this.corecast = new CoreCastV3Client(config.corecast);
        log.info('CoreCast v3 enabled — gRPC streaming (sub-100ms)');
      } else {
        this.corecast = new CoreCastClient(config.corecast);
        log.info('CoreCast enabled — primary fast-lane feed');
      }
    }
    this.bitquery = new BitqueryClient();
    this.featureEngine = new FeatureEngine(config);
    this.stateMachine = new TokenStateMachine(this.db, config);
    this.executionAdapter = new ExecutionAdapter(this.db, config, paper);
    this.healthMonitor = new HealthMonitor(this.db, config);
    this.alertSystem = new AlertSystem(config);
    this.learningLedger = new LearningLedger(this.db);
    this.learningJobs = new LearningJobScheduler(this.db, this.configManager);

    // Wire immediate alerts to Telegram via OpenClaw webhook
    // TELEGRAM_CHAT_ID env var should be set to the Alon's chat ID
    const telegramChatId = process.env.TELEGRAM_CHAT_ID || '5024153101';
    const telegramBotToken = process.env.TELEGRAM_BOT_TOKEN;
    if (telegramBotToken) {
      // Telegram delivery helper — async but fully logged on success AND failure.
      // IMPORTANT: parse_mode 'Markdown' rejects messages with unescaped special chars
      // (underscores, brackets, Cyrillic in token names, etc.) → silently dropped.
      // Use no parse_mode (plain text) for reliability. Rate limit: 1 msg/sec per chat.
      const sendTelegram = async (text: string): Promise<void> => {
        const url = `https://api.telegram.org/bot${telegramBotToken}/sendMessage`;
        const body = JSON.stringify({ chat_id: telegramChatId, text });
        try {
          const res = await fetch(url, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body,
            signal: AbortSignal.timeout(5000),
          });
          if (!res.ok) {
            const errText = await res.text().catch(() => '');
            log.warn(`Telegram delivery HTTP ${res.status}: ${errText.slice(0, 200)}`, { component: 'alerts' });
          } else {
            log.debug(`Telegram delivered: ${text.slice(0, 60)}`, { component: 'alerts' });
          }
        } catch (err) {
          log.warn(`Telegram delivery failed: ${(err as Error).message}`, { component: 'alerts' });
        }
      };

      this.alertSystem.onImmediate((alert) => {
        // Fire-and-forget but with full error logging above
        sendTelegram(alert.message).catch(() => {});
      });
      log.info(`Alert delivery: Telegram chat ${telegramChatId} (plain text, no parse_mode)`, { component: 'alerts' });
    } else {
      log.warn('TELEGRAM_BOT_TOKEN not set — immediate alerts will log only', { component: 'alerts' });
    }

    // Load adaptive threshold state (persisted across restarts)
    loadThresholdState();

    // Initialize MEV backrun engine if enabled
    if (config.mev?.enabled) {
      this.backrunEngine = new BackrunEngine(config.mev, this.feed, this.qualifiedMintCache, this.poolDepthCache);
      log.info(`MEV BackrunEngine initialized (paper_mode=${config.mev.paper_mode})`);
    }

    log.info(`Daemon initialized (paper=${paper})`);
  }

  /** Log config change to DB audit table (config_changes). Zero-cost post-mortem tool. */
  private _logConfigChange(oldConfig: PumpQuantConfig | null, newConfig: PumpQuantConfig): void {
    try {
      const sessionPnl = this.db.getDailyPnl(this.sessionAbsoluteStartMs);
      this.db.raw().prepare(`
        INSERT INTO config_changes (changed_at, old_config, new_config, session_pnl)
        VALUES (?, ?, ?, ?)
      `).run(nowMs(), JSON.stringify(oldConfig ?? {}), JSON.stringify(newConfig), sessionPnl);
    } catch (err) {
      log.warn(`Config audit log failed (non-critical): ${(err as Error).message}`);
    }
  }

  /**
   * Startup reconciliation: check on-chain token balances for any positions that
   * appear 'open' in DB from a previous hard-killed process.
   * If balance = 0 on-chain, the position was sold but DB wasn't updated → mark closed.
   * Called BEFORE streams connect so the position manager doesn't re-manage ghost positions.
   */
  private async reconcileOpenPositions(): Promise<void> {
    const openPositions = this.db.getOpenPositions();
    if (openPositions.length === 0) return;

    log.warn(`Startup reconciliation: found ${openPositions.length} open position(s) — verifying on-chain state`);

    const rpcUrl = process.env.SOLANA_RPC_URL || 'https://api.mainnet-beta.solana.com';
    const walletPubkey = this.executionAdapter.getPublicKey();

    for (const pos of openPositions) {
      try {
        // Query SPL token balance for this mint in our wallet
        const body = JSON.stringify({
          jsonrpc: '2.0', id: 1,
          method: 'getTokenAccountsByOwner',
          params: [
            walletPubkey,
            { mint: pos.mint },
            { encoding: 'jsonParsed' },
          ],
        });
        const resp = await fetch(rpcUrl, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body,
          signal: AbortSignal.timeout(5000),
        });
        const json = await resp.json() as any;
        const accounts = json?.result?.value ?? [];
        const totalBalance = accounts.reduce((sum: number, acct: any) => {
          return sum + (acct.account?.data?.parsed?.info?.tokenAmount?.uiAmount ?? 0);
        }, 0);

        if (totalBalance === 0) {
          log.warn(`Reconcile: position ${pos.mint.slice(0,8)} has zero on-chain balance — marking closed (ghost from prior process)`);
          this.db.updatePosition(pos.id, {
            status: 'closed' as any,
            closed_at: nowMs(),
            exit_reason: 'reconciled_on_boot' as any,
            realized_pnl_sol: null as any, // unknown — flagged for manual review
            hold_duration_s: pos.opened_at ? (nowMs() - pos.opened_at) / 1000 : 0,
          });
        } else {
          log.info(`Reconcile: position ${pos.mint.slice(0,8)} confirmed open (balance=${totalBalance.toFixed(0)} tokens) — resuming management`);
        }
      } catch (err) {
        // Don't crash boot on reconciliation failure — log and continue managing (safe default)
        log.error(`Reconcile: RPC check failed for ${pos.mint.slice(0,8)} — assuming still open: ${(err as Error).message}`);
      }
    }
  }

  /**
   * Bankroll floor check: if live wallet balance falls below min_bankroll_sol,
   * self-halt to prevent fee drag grinding to zero.
   * Called on startup and after each closed position.
   */
  private async checkBankrollFloor(config: PumpQuantConfig): Promise<void> {
    const floor = (config.risk as any).min_bankroll_sol ?? 0.20;
    if (floor <= 0 || isPaperMode()) return;

    try {
      const balance = await this.executionAdapter.getBalance();
      if (balance < floor) {
        log.error(`🛑 Bankroll floor: wallet balance ${balance.toFixed(4)} SOL < floor ${floor} SOL. Self-halting to prevent fee drain.`);
        this.alertSystem.emit('circuit_breaker',
          `🛑 Bankroll floor hit: ${balance.toFixed(4)} SOL remaining (floor: ${floor} SOL). Add capital or reduce position size before resuming.`, {});
        this.healthMonitor.pause(`Bankroll below floor (${balance.toFixed(4)} < ${floor} SOL)`);
      }
    } catch (err) {
      log.warn(`Bankroll floor check failed: ${(err as Error).message}`);
    }
  }

  /** Start the daemon */
  async start(): Promise<void> {
    log.info('Starting strategy daemon...');

    // Startup reconciliation: resolve any ghost open positions from prior hard-killed process.
    // Must run BEFORE streams connect so position manager doesn't re-manage stale state.
    await this.reconcileOpenPositions();

    // Bankroll floor check: self-halt if wallet is below minimum viable operating balance.
    const startupConfig = this.configManager.getConfig();
    await this.checkBankrollFloor(startupConfig);

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

    // Position scanner: evaluate exit for all LONG positions every 500ms
    // (4x faster for active exits - Phase 1 data architect recommendation)
    setInterval(() => {
      this.scanOpenPositions();
    }, 500);

    // Start learning jobs
    this.learningJobs.start();

    // Start HTTP API
    const apiPort = parseInt(process.env.DAEMON_PORT || '9420');
    const apiHost = process.env.DAEMON_HOST || '127.0.0.1';
    startApiServer(this.buildApiContext(), apiPort, apiHost);

    // Start MEV backrun engine (after feed is connected)
    if (this.backrunEngine) {
      this.backrunEngine.start();
    }

    log.info('Strategy daemon started');
  }

  /** Stop the daemon */
  async stop(): Promise<void> {
    log.info('Stopping strategy daemon...');

    // Gate new entries/exits immediately — before disconnecting streams
    this.isShuttingDown = true;

    // Stop stream event delivery first
    if (this.corecast) this.corecast.disconnect();
    this.feed.disconnect();
    this.learningJobs.stop();
    if (this.backrunEngine) this.backrunEngine.stop();
    if (this.healthCheckInterval) clearInterval(this.healthCheckInterval);
    if (this.analysisInterval) clearInterval(this.analysisInterval);

    // Drain in-flight trades (executeEntry / executeExit) before closing DB.
    // Prevents ghost open positions from SIGTERM racing mid-exit-confirmation.
    // Max 15s drain window — exit confirmation typically resolves in <2s on mainnet.
    const inflightCount = this._inflightTrades.size;
    if (inflightCount > 0) {
      log.info(`Draining ${inflightCount} in-flight trade(s) before shutdown...`);
      const drainPromise = Promise.allSettled([...this._inflightTrades]);
      const timeoutPromise = new Promise<void>(resolve => setTimeout(resolve, 15_000));
      await Promise.race([drainPromise, timeoutPromise]);
      const remaining = this._inflightTrades.size;
      if (remaining > 0) {
        log.error(`Drain timeout — closing DB with ${remaining} in-flight trade(s) still pending. Positions may be inconsistent.`);
      } else {
        log.info('All in-flight trades drained cleanly');
      }
    }

    this.db.close();
    log.info('Strategy daemon stopped');
  }

  // ====== FEED HANDLERS ======

  private setupFeedHandlers(): void {
    // New token creation — always process from PumpPortal.
    // PumpPortal provides bondingCurveKey and full reserves; gRPC v3 doesn't.
    this.feed.on('newToken', (event: NewTokenEvent) => {
      this.handleNewToken(event);
    });

    // Token trade — always process from PumpPortal for bonding curve state enrichment.
    // When CoreCast v3 (gRPC) is primary, gRPC provides fast event discovery but lacks
    // vTokensInBondingCurve/vSolInBondingCurve/marketCapSol fields. PumpPortal fills those in.
    let tradeCount = 0;
    this.feed.on('tokenTrade', (event: TokenTradeEvent) => {
      tradeCount++;
      if (tradeCount % 100 === 1) {
        const src = this.corecast?.connected ? 'enrichment' : 'fallback';
        log.info(`PumpPortal trades (${src}): ${tradeCount} (latest: ${event.mint.slice(0,8)} ${event.txType} ${event.solAmount?.toFixed(4)} SOL)`);
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

    // CoreCast trade → same handler, but marks source; also route through whale tracker
    let ccTradeCount = 0;
    this.corecast.on('tokenTrade', (event: TokenTradeEvent) => {
      ccTradeCount++;
      if (ccTradeCount % 100 === 1) {
        log.info(`CoreCast trades: ${ccTradeCount} (latest: ${event.mint.slice(0,8)} ${event.txType})`);
      }
      this.handleTokenTrade(event);
      // Route every trade through whale tracker (fallback path when Stream 5 is inactive)
      this.whaleTracker.checkTrade(event.mint, event.traderPublicKey, event.solAmount, event.txType);
    });

    // CoreCast migration
    this.corecast.on('migration', (event: MigrationEvent) => {
      this.handleMigration(event);
    });

    this.corecast.on('connected', () => {
      this.healthMonitor.recordUpdate('market_feed');
      // Verify at least 3 core Bitquery streams are active (streams 4-5 are optional enhancements).
      // 3 core streams required: bonding_trades + transactions + amm_trades.
      const v3 = this.corecast as import('../feed/corecast-v3').CoreCastV3Client;
      const activeStreams = typeof v3.activeStreamCount === 'number' ? v3.activeStreamCount : -1;
      if (activeStreams !== -1 && activeStreams < 3) {
        log.error(`CRITICAL: CoreCast connected with only ${activeStreams} stream(s). Minimum 3 required. Halting to prevent blind trading.`);
        this.healthMonitor.pause('stream_count_mismatch');
      } else {
        log.info(`CoreCast fast-lane connected (${activeStreams === -1 ? '?' : activeStreams} streams active, 3 core required)`);
      }
    });

    // Stream 4: DexPools — Raydium LP depth monitor (optional, graceful degradation if unavailable)
    this.corecast.on('poolUpdate', (update: PoolUpdate) => {
      this.poolDepthCache.update(update.mint, update.depthSol);

      // LP removal = forced exit signal
      if (update.isRemoval && update.changeSol < -1) { // >1 SOL removed
        const hasPos = this.backrunEngine?.hasOpenPosition(update.mint);
        if (hasPos) {
          log.warn(`[daemon] LP removal detected on ${update.mint.slice(0, 8)} — forcing exit`);
          this.backrunEngine?.forceExit(update.mint, 'lp_removal');
        }
      }
    });

    // Stream 5: whale_trades — dedicated whale stream (only active when whale list >= 5 addresses)
    // Catches whale buys with lower latency than the general tokenTrade path above
    this.corecast.on('whaleTrade', (event: { mint: string; traderAddress: string; solAmount: number; txType: string }) => {
      this.whaleTracker.checkTrade(event.mint, event.traderAddress, event.solAmount, event.txType);
    });

    // Whale buy → pre-qualify mint in signal bridge with extended TTL (60s)
    this.whaleTracker.on('whaleBuy', (event: WhaleBuyEvent) => {
      log.info(`[daemon] Whale buy: ${event.traderAddress.slice(0,8)}... on ${event.mint.slice(0,8)}... — pre-qualifying for MEV (60s TTL)`);
      this.qualifiedMintCache.preQualify(event.mint, 60_000);
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

    // Async social pre-fetch — non-blocking, result cached for entry evaluation
    if (event.mint) this.socialCache.prefetch(event.mint);

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

    const rawProgress = computeBondingCurveProgress(event.vTokensInBondingCurve);
    // -1 sentinel = reserves unknown (new token, PumpPortal not yet enriched)
    // Treat as 0 (fully fresh) for regime classification purposes
    const bondingCurveProgress = rawProgress < 0 ? 0 : rawProgress;

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

    // Initialize feature tracking — pass token creation timestamp for maturity-aware manipulation gating
    this.featureEngine.initToken(event.mint, event.traderPublicKey, packet.created_at);

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

    // Cross-feed dedup: only add to featureEngine once per tx signature
    // (gRPC + PumpPortal both fire this handler for same on-chain trade)
    const sig = event.signature || event.traderPublicKey + ':' + (event.timestamp || 0);
    const alreadySeen = sig && this.tradeDedupSet.has(sig);
    if (!alreadySeen) {
      if (sig) {
        this.tradeDedupSet.add(sig);
        this.tradeDedupOrder.push(sig);
        if (this.tradeDedupOrder.length > this.TRADE_DEDUP_MAX) {
          const evicted = this.tradeDedupOrder.shift();
          if (evicted) this.tradeDedupSet.delete(evicted);
        }
      }
      this.featureEngine.addTrade(event.mint, tradePoint);
    }

    // Update market data in state machine
    // Only update if we have real reserve data (vTokens > 0 = PumpPortal enriched)
    // Skip updates from gRPC-only events which have zero reserves
    const rawBcp = computeBondingCurveProgress(event.vTokensInBondingCurve);
    if (rawBcp >= 0) {
      // Real data from PumpPortal — update state
      this.stateMachine.updateMarketData(
        event.mint,
        event.vTokensInBondingCurve,
        event.vSolInBondingCurve,
        event.marketCapSol,
        rawBcp
      );
    }
    // If rawBcp < 0 (gRPC event, no reserves): skip market data update, keep last known state

    // Update position if we hold this token
    this.updatePositionMarketData(event.mint, event);

    this.healthMonitor.recordUpdate('market_feed');

    // Event-driven analysis: fire immediately on each trade for active tokens
    // Don't wait for the 1s polling interval — gRPC data is sub-100ms, act on it now
    if (packet.state === TokenState.WATCH || packet.state === TokenState.ENTER_READY || packet.state === TokenState.LONG) {
      const config = this.configManager.getConfig();
      const health = this.healthMonitor.check();
      this.analyzeToken(packet, config, health);
    }
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

    // Deadlock watchdog: check CB state on every analysis call
    this.tickCircuitBreakerWatchdog();

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

    // NEW: Defer analysis until sufficient trade density (Phase 1 data architect fix)
    const tradeCount = this.featureEngine.getTradeCount(mint);
    const minTrades = (config.entry as any).min_trades_for_analysis ?? 3;
    if (tradeCount < minTrades) {
      log.debug(`${mint.slice(0, 8)}: deferred eval (trades=${tradeCount}/${minTrades})`, { component: 'daemon' });
      return;
    }

    // Compute features
    const features = this.featureEngine.computeFeatures(mint);
    if (!features) return;

    // Mark observation start baseline once the observation window has elapsed.
    // This snapshots vSolAtObservationStart and swapCountAtObservationStart so that
    // window-scoped capital efficiency metrics (windowVSolAccumulated, windowSwapCount)
    // measure quality from the entry-evaluation window forward, not from token creation.
    // markObservationStart() is idempotent — subsequent calls are no-ops.
    const tokenAgeS = ageS(packet.created_at);
    if (tokenAgeS >= config.entry.observation_window_s && !this.featureEngine.isObservationStartMarked(mint)) {
      this.featureEngine.markObservationStart(mint);
    }

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
    // Phase 1 fix: age-adjusted thresholds (defer non-creator shocks until 1.6x observation window)
    if (manipAssessment.hardShock) {
      const isCreatorSell = manipAssessment.hardShockReason === 'creator_sell';
      const minAgeForHardBan = config.entry.observation_window_s * 1.6; // 8s when window=5s
      
      if (isCreatorSell || tokenAgeSec > minAgeForHardBan) {
        this.stateMachine.transitionToBan(mint, `Manipulation shock: ${manipAssessment.hardShockReason}`);
        return;
      }
      // Young token with non-creator hard shock: log but don't ban yet
      log.debug(`${mint.slice(0,8)}: Hard shock (${manipAssessment.hardShockReason}) deferred, age=${tokenAgeSec.toFixed(1)}s`);
    }

    // State-specific logic
    let entryEv = null;
    let exitEv = null;
    let sizing = null;

    if (packet.state === TokenState.WATCH || packet.state === TokenState.ENTER_READY) {
      // Skip evaluation entirely if mint is already committed (in-flight buy).
      // This prevents evaluateEntry from logging ENTRY APPROVED multiple times
      // while executeEntry is still awaiting confirmation.
      if (this.committedMints.has(mint)) return;

      // Entry evaluation
      const currentPositionCount = this.db.getOpenPositionCount();
      // Use configChangeEpoch so mid-session config changes reset the daily loss window.
      // New max_daily_loss_sol limits apply to trades AFTER the config change, not the full day.
      const dailyLoss = this.db.getDailyPnl(this.configChangeEpoch);
      const dailyEntryCount = this.db.getDailyEntryCount();

      // Social pre-filter: banned or NSFW tokens are instant disqualifiers
      const social = this.socialCache.getOrNeutral(mint);
      if (social.fetchOk && (social.is_banned || social.nsfw)) {
        log.info(`REJECT ${packet.symbol || mint.slice(0,8)}: social_banned (banned=${social.is_banned} nsfw=${social.nsfw})`);
        return;
      }

      // Circuit breaker: NEVER multiply edge threshold — only adjust position size (L1) or pause (L2/L3).
      // Edge threshold is anchored to model output ceiling via threshold/manager. Multiplying it can
      // exceed the model's physical output range → infinite deadlock (the March-26 incident).
      // Size is scaled by circuitBreakerSizeMul (1.0 normal, 0.5 at L1).
      const cbConfig = this.circuitBreakerSizeMul !== 1.0
        ? { ...config, risk: { ...config.risk, quick_spend_sol: config.risk.quick_spend_sol * this.circuitBreakerSizeMul } }
        : config;
      const entryDecision = evaluateEntry(
        packet, probabilities, features,
        cbConfig,
        currentPositionCount, dailyLoss, dailyEntryCount, isPaperMode()
      );

      entryEv = entryDecision.ev;
      sizing = entryDecision.sizing;

      if (packet.state === TokenState.WATCH && entryDecision.shouldEnter) {
        // GUARD: layered duplicate-entry prevention
        // committedMints is the primary lock — set synchronously, cleared only after
        // DB write succeeds or fails. Survives the async gap between decision and confirmation.
        if (this.committedMints.has(mint)) return;
        if (this.pendingExecutions.has(mint)) return;
        if (this.analysisLocks.has(mint + '_entry')) return;
        const existingPosition = this.db.getPositionByMint(mint);
        if (existingPosition) return;
        // Double-check state hasn't changed
        const freshPacket = this.stateMachine.getPacket(mint);
        if (!freshPacket || freshPacket.state !== TokenState.WATCH) return;

        const now = nowMs();
        const openPositionCount = this.db.getOpenPositions().length;
        const totalPending = openPositionCount + this.committedMints.size;
        // Paper mode: bypass circuit breaker pause, execution cooldown, and position limit.
        // Purpose is maximum data collection — risk controls are irrelevant for paper trades.
        const paperBypass = isPaperMode();
        const cbAllowed = paperBypass || (now > this.circuitBreakerPauseUntil);
        const cooldownAllowed = paperBypass || ((now - this.lastExecutionAttempt) > this.executionCooldownMs);
        const positionAllowed = paperBypass || (totalPending < config.risk.max_positions);
        if (health.tradingAllowed && cbAllowed && positionAllowed && cooldownAllowed) {
          // Commit synchronously — all subsequent ticks for this mint will see this and bail
          this.committedMints.add(mint);
          this.pendingExecutions.add(mint);
          this.analysisLocks.add(mint + '_entry');
          this.lastExecutionAttempt = now;

          // Store full entry decision on packet for ML feature capture in executeEntry
          packet.entry_decision = entryDecision;
          // Signal bridge: pre-qualify this mint for MEV engine (30s TTL)
          this.qualifiedMintCache.add(mint);
          log.info(`🚀 Promoting to ENTER_READY: ${packet.symbol || mint.slice(0,8)} — ${entryDecision.reason}`);
          this.stateMachine.transitionToEnterReady(mint, entryDecision.reason);
          log.info(`💰 EXECUTING ENTRY: ${packet.symbol || mint.slice(0,8)} size=${entryDecision.sizing!.position_size.toFixed(4)} SOL | social: twitter=${social.has_twitter} tg=${social.has_telegram} replies=${social.reply_count} score=${social.social_score.toFixed(2)} (positions: ${openPositionCount}+${this.committedMints.size - 1} pending)`);

          if (!this.isShuttingDown) {
            this._trackTrade(
              this.executeEntry(packet, entryDecision.sizing!, config).finally(() => {
                this.pendingExecutions.delete(mint);
                this.analysisLocks.delete(mint + '_entry');
                // committedMints cleared in executeEntry after DB write (success or failure)
              })
            );
          } else {
            this.pendingExecutions.delete(mint);
            this.analysisLocks.delete(mint + '_entry');
          }
        }
      }
    }

    if (packet.state === TokenState.LONG || packet.state === TokenState.REDUCE) {
      // Exit evaluation
      const position = this.db.getPositionByMint(mint);
      if (position) {
        // SETTLEMENT GUARD: don't evaluate exits within 3s of entry
        // PumpPortal needs time to settle the buy on-chain before we can sell.
        // Selling too early results in "sell zero amount" (error 6022) because
        // the token account hasn't been credited yet.
        const holdMs = nowMs() - position.entry_timestamp;
        if (holdMs < 3000) {
          // Skip exit eval — too early, buy not settled yet
        } else {
          const exitDecision = evaluateExit(packet, position, probabilities, features, config);
          exitEv = exitDecision.ev;

          if (exitDecision.shouldExit && !this.pendingExecutions.has(mint)) {
            this.pendingExecutions.add(mint);
            this._trackTrade(
              this.executeExit(mint, position, exitDecision.exitPct, exitDecision.reason, config)
                .finally(() => this.pendingExecutions.delete(mint))
            );
          } else if (exitDecision.shouldReduce && !this.pendingExecutions.has(mint)) {
            this.pendingExecutions.add(mint);
            this._trackTrade(
              this.executeReduce(mint, position, exitDecision.exitPct, exitDecision.reason, config)
                .finally(() => this.pendingExecutions.delete(mint))
            );
          }
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

  // Priority fee cache: refresh every 10s to avoid per-trade RPC latency on exit path.
  // A 1.5s RPC call before every exit on a fast dump would cost more in slippage than it saves in fees.
  private _priorityFeeCache: number | null = null;
  private _priorityFeeCacheAt: number = 0;
  private readonly PRIORITY_FEE_CACHE_TTL_MS = 10_000; // 10 seconds

  /**
   * Dynamic priority fee resolver — cached.
   * Refreshes at most every 10s via getRecentPrioritizationFees.
   * Returns cached value on subsequent calls within the TTL window.
   * Falls back to default_priority_fee_sol on error or if dynamic disabled.
   */
  private async resolvePriorityFee(config: PumpQuantConfig): Promise<number> {
    const execCfg = config.execution as any;
    const floor = execCfg.priority_fee_floor_sol ?? config.execution.default_priority_fee_sol;
    const cap = execCfg.priority_fee_cap_sol ?? config.execution.default_priority_fee_sol;
    const dflt = config.execution.default_priority_fee_sol;

    if (!execCfg.dynamic_priority_fee) return dflt;

    // Return cached value if fresh (< 10s old) — avoids per-trade RPC latency on critical exit path.
    // A 1.5s network call before every exit on a fast dump costs more in slippage than it saves.
    const now = nowMs();
    if (this._priorityFeeCache !== null && (now - this._priorityFeeCacheAt) < this.PRIORITY_FEE_CACHE_TTL_MS) {
      return this._priorityFeeCache;
    }

    try {
      const PUMP_PROGRAM = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P';
      const rpcUrl = process.env.SOLANA_RPC_URL || 'https://api.mainnet-beta.solana.com';
      const body = JSON.stringify({
        jsonrpc: '2.0', id: 1,
        method: 'getRecentPrioritizationFees',
        params: [[PUMP_PROGRAM]],
      });
      const resp = await fetch(rpcUrl, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body,
        signal: AbortSignal.timeout(1500),
      });
      const json = await resp.json() as any;
      const fees: number[] = (json?.result || [])
        .map((f: any) => f.prioritizationFee as number)
        .filter((f: number) => f > 0)
        .sort((a: number, b: number) => a - b);

      const pctTarget = (execCfg.dynamic_priority_fee_percentile ?? 75) / 100;
      let clamped = dflt;
      if (fees.length > 0) {
        const idx = Math.floor(fees.length * pctTarget);
        const p75FeeLamports = fees[Math.min(idx, fees.length - 1)];
        const feeSol = (p75FeeLamports * 1.2) / 1_000_000_000;
        clamped = Math.min(cap, Math.max(floor, feeSol));
        log.debug(`Dynamic priority fee: p75=${p75FeeLamports} lamports → ${clamped.toFixed(7)} SOL`);
      } else {
        // No prioritization fees in recent slots — network is uncongested, use floor
        clamped = floor;
        log.debug(`Dynamic priority fee: no recent fees, using floor ${floor}`);
      }

      // Cache the result
      this._priorityFeeCache = clamped;
      this._priorityFeeCacheAt = now;
      return clamped;
    } catch (err) {
      log.debug(`Dynamic priority fee fallback to default: ${(err as Error).message}`);
      // Don't cache on error — retry next call
      return dflt;
    }
  }

  private async executeEntry(
    packet: CandidatePacket,
    sizing: { position_size: number; limiting_factor: string },
    config: PumpQuantConfig
  ): Promise<void> {
    // Pre-execution migration guard: check bonding curve progress right before sending.
    // A token can graduate between entry approval and execution (typically 100-500ms).
    // If progress >= graduation_boundary_start, abort — don't attempt the buy.
    const latestProgress = packet.bonding_curve_progress ?? 0;
    const gradBoundary = config.regime.graduation_boundary_start ?? 0.75;
    if (latestProgress >= gradBoundary) {
      log.warn(`${packet.mint.slice(0,8)}: Aborting entry — bonding curve graduated (progress=${latestProgress.toFixed(3)} >= ${gradBoundary})`);
      this.stateMachine.transitionToBan(packet.mint, 'Pre-execution migration guard: curve graduated');
      this.featureEngine.removeToken(packet.mint);
      this.feed.unsubscribeTokenTrades([packet.mint]);
      if (this.corecast) this.corecast.unwatchMints([packet.mint]);
      this.committedMints.delete(packet.mint);
      return;
    }

    const priorityFee = await this.resolvePriorityFee(config);
    const intent: TradeIntent = {
      id: uuidv4(),
      mint: packet.mint,
      side: 'buy',
      size_sol: sizing.position_size,
      slippage_bps: config.execution.default_slippage_bps,
      priority_fee_sol: priorityFee,
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
          // IMPORTANT: mfe_sol MUST initialize to entry_sol (not 0).
          // The momentum reversal exit gate: mfePct = (mfe_sol - entry_sol) / entry_sol
          // If mfe_sol = 0: mfePct = (0 - entry) / entry = -1.0 → gate PERMANENTLY inactive.
          // If mfe_sol = entry_sol: mfePct = 0.0 → correctly waits for 5% unrealized gain.
          // The DOA position-scanner check was `mfeSol <= 0` — updated below to `mfeSol <= entry_sol`.
          mfe_sol: 0, // tracks max unrealized PnL (SOL) during hold; 0 = never went positive
          mae_sol: 0, // tracks max adverse excursion (negative SOL); 0 = never went negative
          is_paper: isPaperMode(),
          config_version: intent.config_version,
          // ML feature snapshot — entry signal values for supervised learning
          entry_features: packet.entry_decision?.featureSnapshot
            ? JSON.stringify(packet.entry_decision.featureSnapshot)
            : null,
          feat_p_cont: packet.entry_decision?.featureSnapshot?.p_cont ?? null,
          feat_bcd_score: packet.entry_decision?.featureSnapshot?.bcd_score ?? null,
          feat_manip_score: packet.entry_decision?.featureSnapshot?.manip_score ?? null,
          feat_creator_prior: packet.entry_decision?.featureSnapshot?.creator_prior ?? null,
          feat_velocity: packet.entry_decision?.featureSnapshot?.velocity ?? null,
          feat_breadth_score: packet.entry_decision?.featureSnapshot?.breadth_score ?? null,
          feat_unique_buyers: packet.entry_decision?.featureSnapshot?.unique_buyers ?? null,
          feat_mcap_sol: packet.entry_decision?.featureSnapshot?.vsol_in_curve ?? null,
          entry_ts: nowMs(),
          active_stop_pct: config.risk.raw_stop_pct,
          active_target_pct: config.exit.take_profit_pct,
          active_max_hold_s: config.exit.max_hold_time_s,
        };

        this.db.insertPosition(position);
        // Position confirmed in DB — release the committed lock so the mint
        // can be evaluated again in future sessions (but existingPosition check will block re-entry)
        this.committedMints.delete(packet.mint);
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
      } else {
        // Order didn't confirm — release lock so the mint can retry
        this.committedMints.delete(packet.mint);
      }
    } catch (err) {
      const errMsg = (err as Error).message;
      log.error(`Entry execution failed for ${packet.mint}: ${errMsg}`);
      this.alertSystem.emitExecutionFailure(packet.mint, errMsg);

      // Migration race: token graduated to Raydium AMM between approval and execution.
      // PumpPortal returns 400 with "migrated or does not exist" or "pump-amm is the correct option".
      // Don't retry — BAN immediately so the state machine doesn't keep re-approving.
      if (errMsg.includes('migrated') || errMsg.includes('pump-amm') || errMsg.includes('does not exist')) {
        log.warn(`${packet.mint.slice(0,8)}: Migration race detected on entry — banning token`);
        this.stateMachine.transitionToBan(packet.mint, 'Migration race: token graduated before entry confirmed');
        this.featureEngine.removeToken(packet.mint);
        this.feed.unsubscribeTokenTrades([packet.mint]);
        if (this.corecast) this.corecast.unwatchMints([packet.mint]);
      } else {
        // Non-migration failure — release lock so the mint can be retried on next tick
        this.committedMints.delete(packet.mint);
      }
    }
  }

  private async executeExit(
    mint: string,
    position: Position,
    exitPct: number,
    reason: string,
    config: PumpQuantConfig
  ): Promise<void> {
    const priorityFee = await this.resolvePriorityFee(config);
    const intent: TradeIntent = {
      id: uuidv4(),
      mint,
      side: 'sell',
      size_sol: position.current_value_sol * (exitPct / 100),
      amount_pct: exitPct,
      slippage_bps: config.execution.default_slippage_bps,
      priority_fee_sol: priorityFee,
      route_mode: config.execution.default_route_mode,
      reason: `Exit: ${reason}`,
      config_version: getConfigVersion(),
      created_at: nowMs(),
      ev_at_intent: 0,
    };

    // SAFETY GUARD: never sell if we have no tokens — PumpPortal returns error 6022 "sell zero amount"
    // This can happen if the buy hasn't settled on-chain yet when exit fires
    if (position.current_tokens <= 0) {
      log.warn(`Exit aborted for ${mint}: current_tokens=${position.current_tokens} (buy not settled yet)`);
      return;
    }

    this.db.insertTradeIntent(intent);

    try {
      const order = await this.executionAdapter.executeSell(intent);

      if (order.status === OrderStatus.CONFIRMED) {
        const realizedSol = order.realized_sol || 0;
        const closedAt = nowMs();

        // Close the primary position
        const pnl = realizedSol - position.entry_sol;
        this.db.updatePosition(position.id, {
          current_tokens: 0,
          current_value_sol: 0,
          exit_orders: [...position.exit_orders, order.id],
          exit_price_sol: order.realized_price || 0,
          exit_sol: realizedSol,
          exit_timestamp: closedAt,
          exit_reason: reason as any,
          exit_route_mode: intent.route_mode,
          realized_pnl_sol: pnl,
          realized_pnl_pct: position.entry_sol > 0 ? pnl / position.entry_sol : 0,
          total_fees_sol: position.total_fees_sol + (order.fee_sol || 0),
          status: PositionStatus.CLOSED,
          closed_at: closedAt,
          hold_duration_s: ageS(position.entry_timestamp),
          exit_ts: closedAt,
        });

        // Close any ghost duplicate positions for the same mint (race condition artifacts)
        const allOpen = this.db.getAllOpenPositionsByMint(mint);
        for (const ghost of allOpen) {
          if (ghost.id === position.id) continue; // already closed above
          log.warn(`Closing ghost duplicate position ${ghost.id.slice(0,8)} for ${position.symbol || mint.slice(0,8)}`);
          this.db.updatePosition(ghost.id, {
            current_tokens: 0,
            current_value_sol: 0,
            exit_orders: [order.id],
            exit_price_sol: order.realized_price || 0,
            exit_sol: 0, // Ghost — no real SOL realized separately
            exit_timestamp: closedAt,
            exit_reason: reason as any,
            exit_route_mode: intent.route_mode,
            realized_pnl_sol: 0, // Zero out ghost PnL — it didn't actually trade
            realized_pnl_pct: 0,
            total_fees_sol: ghost.total_fees_sol,
            status: PositionStatus.CLOSED,
            closed_at: closedAt,
            hold_duration_s: ageS(ghost.entry_timestamp),
          });
        }

        this.stateMachine.transitionToExit(mint, reason);

        // Circuit breaker tracking — skip for paper trades (no real capital at risk)
        if (!isPaperMode()) {
          this.updateCircuitBreaker(pnl);
        }

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

        // Bankroll floor check after each closed position — self-halt if below minimum viable
        const currentConfig = this.configManager.getConfig();
        this.checkBankrollFloor(currentConfig).catch(err =>
          log.warn(`Post-exit bankroll check failed: ${(err as Error).message}`)
        );
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
    const priorityFee = await this.resolvePriorityFee(config);
    const intent: TradeIntent = {
      id: uuidv4(),
      mint,
      side: 'sell',
      size_sol: position.current_value_sol * (reducePct / 100),
      amount_pct: reducePct,
      slippage_bps: config.execution.default_slippage_bps,
      priority_fee_sol: priorityFee,
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

    // Track MFE/MAE as unrealized PnL (SOL), relative to entry cost.
    // mfe_sol initialized to 0 at open (not entry_sol — that was a unit mismatch bug).
    // unrealizedPnl = currentValue - entry_sol, so MFE=0 means "never went positive".
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

    // Config is accessible and valid — record config_integrity heartbeat
    this.healthMonitor.recordUpdate('config_integrity');

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
        const oldCfg = this.configManager.getConfig();
        const patch = { risk: settings };
        const result = this.configManager.applyPatch(patch as any, 'operator', 'Risk settings updated via plugin');
        // Risk changes mean new limits — reset epoch so they apply to new trades only
        this.configChangeEpoch = nowMs();
        this._logConfigChange(oldCfg, this.configManager.getConfig());
        log.info('Risk settings updated — daily loss counter reset');
        return result;
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
        // Reset daily loss epoch so new limits apply to this config's trades, not old session's losses
        this.configChangeEpoch = nowMs();
        this._logConfigChange(null, this.configManager.getConfig());
        log.info(`Strategy profile set to: ${name} — daily loss counter reset`);
      },

      getRuntimeConfig: () => {
        return this.configManager.getConfig() as PumpQuantConfig;
      },

      updateRuntimeConfig: (patch: Record<string, unknown>) => {
        const oldCfg = this.configManager.getConfig();
        const result = this.configManager.applyPatch(patch as any, 'operator', 'Config updated via plugin');
        // Reset daily loss epoch on any runtime config change so new limits apply fresh
        this.configChangeEpoch = nowMs();
        this._logConfigChange(oldCfg, this.configManager.getConfig());
        log.info('Config updated via plugin — daily loss counter reset');
        return result;
      },
    };
  }

  // ====== POSITION SCANNER ======

  /**
   * Periodic safety scan: evaluate stop loss and time decay for all open positions.
   * Triggers even when no trade events arrive (quiet tokens). Fires every 2s.
   */
  private scanOpenPositions(): void {
    const config = this.configManager.getConfig() as PumpQuantConfig;
    const openPositions = this.db.getOpenPositions();

    for (const position of openPositions) {
      const { mint } = position;
      if (this.pendingExecutions.has(mint)) continue;

      const packet = this.stateMachine.getPacket(mint);
      if (!packet) continue;

      // Ensure state is LONG (fix transition if buy confirmed but state not updated)
      if (packet.state === TokenState.WATCH || packet.state === TokenState.ENTER_READY) {
        log.warn(`Position scanner: fixing stale state for ${packet.symbol || mint.slice(0,8)} → transitioning to LONG`);
        this.stateMachine.transitionToLong(mint, 'position_scanner_recovery');
      }

      // Dead on arrival (DOA) check: if MFE never exceeded entry_sol after 15s AND down >5%, exit immediately.
      // NOTE: mfe_sol is initialized to entry_sol at open (not 0), so 'never moved above entry'
      // is detected by mfeSol <= entry_sol (not mfeSol <= 0 as before).
      const holdTime = ageS(position.entry_timestamp);
      const mfeSol = position.mfe_sol;
      const currentPnL = position.current_value_sol - position.entry_sol;
      const pnlPct = position.entry_sol > 0 ? currentPnL / position.entry_sol : 0;
      
      if (holdTime >= 15 && mfeSol <= position.entry_sol && pnlPct < -0.05) {
        log.warn(`💀 DOA exit: ${packet.symbol || mint.slice(0,8)} held ${holdTime.toFixed(0)}s, MFE=0, down ${(pnlPct*100).toFixed(1)}%`);
        if (!this.pendingExecutions.has(mint) && !this.isShuttingDown) {
          this.pendingExecutions.add(mint);
          this._trackTrade(
            this.executeExit(mint, position, 100, ExitReason.STOP_LOSS, config).finally(() => {
              this.pendingExecutions.delete(mint);
            })
          );
        }
        continue;
      }

      // Raw stop loss check (deterministic, no features needed)
      if (position.entry_sol > 0) {
        const lossPct = (position.current_value_sol - position.entry_sol) / position.entry_sol;
        if (lossPct <= -config.risk.raw_stop_pct) {
          log.warn(`🛑 Position scanner stop loss: ${packet.symbol || mint.slice(0,8)} loss=${(lossPct*100).toFixed(1)}%`);
          if (!this.pendingExecutions.has(mint) && !this.isShuttingDown) {
            this.pendingExecutions.add(mint);
            this._trackTrade(
              this.executeExit(mint, position, 100, ExitReason.STOP_LOSS, config).finally(() => {
                this.pendingExecutions.delete(mint);
              })
            );
          }
          continue;
        }
      }

      // Hard time limit
      const holdS = ageS(position.entry_timestamp);
      if (holdS > config.exit.max_hold_time_s) {
        log.warn(`⏰ Position scanner time exit: ${packet.symbol || mint.slice(0,8)} held ${holdS.toFixed(0)}s`);
        if (!this.pendingExecutions.has(mint)) {
          this.pendingExecutions.add(mint);
          this.executeExit(mint, position, 100, ExitReason.TIME_DECAY, config).finally(() => {
            this.pendingExecutions.delete(mint);
          });
        }
      }
    }
  }

  // ====== CIRCUIT BREAKER ======

  /**
   * Update circuit breaker state after each closed trade.
   *
   * REDESIGNED (2026-03-26) — post-deadlock incident:
   * Old design multiplied edge thresholds (1.5x L1, 2.0x L2) which could exceed the model's
   * physical output ceiling → permanent deadlock requiring manual intervention.
   *
   * New design: modulates EXPOSURE (position size + time pauses), never thresholds.
   *   L0: Normal operation (full size, no pause)
   *   L1: 3 consecutive losses → 50% size for 5 min, then reassess
   *   L2: 5 consecutive losses → full pause 15 min, resume at L1
   *   L3: session PnL < -0.30 SOL → session halt, alert, manual restart required
   *
   * Rules:
   * - A WIN always resets the streak and restores full size
   * - No multipliers on any threshold, ever
   * - Deadlock watchdog: if no trade attempt for 15 min during trading hours, auto-reset to L0 with alert
   */
  private updateCircuitBreaker(pnl: number): void {
    const now = nowMs();
    this.circuitBreakerDeadlockWatchdogAt = now; // Touch watchdog

    if (pnl >= 0) {
      // Win resets everything — no gambler's fallacy "need a win to reset"
      if (this.consecutiveLosses > 0 || this.circuitBreakerLevel > 0) {
        log.info(`✅ Circuit breaker reset: ${this.consecutiveLosses} consecutive losses cleared by win. Level ${this.circuitBreakerLevel} → L0`);
        this.alertSystem.emit('circuit_breaker', `✅ Circuit breaker reset to L0 after win. Full size restored.`, {});
      }
      this.consecutiveLosses = 0;
      this.circuitBreakerLevel = 0;
      this.circuitBreakerSizeMul = 1.0;
      this.circuitBreakerPauseUntil = 0;
      return;
    }

    this.consecutiveLosses++;
    log.warn(`Circuit breaker: ${this.consecutiveLosses} consecutive losses (level=${this.circuitBreakerLevel})`);

    // Check L3: session hard stop.
    // IMPORTANT: L3 uses sessionAbsoluteStartMs (set at daemon boot, NEVER reset on config reload).
    // This prevents an operator from bypassing the 24h halt by reloading config — the session PnL
    // window spans the entire process lifetime, regardless of mid-session config changes.
    const sessionPnl = this.db.getDailyPnl(this.sessionAbsoluteStartMs);
    if (sessionPnl <= -0.30) {
      this.circuitBreakerLevel = 3;
      this.circuitBreakerPauseUntil = now + 24 * 60 * 60 * 1000; // session halt
      this.circuitBreakerSizeMul = 0;
      log.error(`🛑 Circuit breaker L3: session PnL=${sessionPnl.toFixed(4)} SOL <= -0.30 SOL threshold. Session halted.`);
      this.alertSystem.emit('circuit_breaker', `🛑 Circuit breaker L3: session loss limit hit (${sessionPnl.toFixed(4)} SOL). Manual restart required.`, {});
      return;
    }

    if (this.consecutiveLosses >= 5 && this.circuitBreakerLevel < 2) {
      // L2: 15-minute full pause, then resume at L1 size
      const pauseMs = 15 * 60 * 1000;
      this.circuitBreakerLevel = 2;
      this.circuitBreakerPauseUntil = now + pauseMs;
      this.circuitBreakerSizeMul = 0.5; // Resume at half size after pause
      const resumeAt = new Date(now + pauseMs).toLocaleTimeString();
      log.warn(`🔴 Circuit breaker L2: 5 consecutive losses — pausing entries for 15 min (resume ~${resumeAt})`);
      this.alertSystem.emit('circuit_breaker', `🔴 Circuit breaker L2: 5 consecutive losses. Pausing 15 min, resuming at 50% size. NO threshold changes.`, {});
    } else if (this.consecutiveLosses >= 3 && this.circuitBreakerLevel < 1) {
      // L1: 50% position size for 5 min (no pause, no threshold change)
      this.circuitBreakerLevel = 1;
      this.circuitBreakerSizeMul = 0.5;
      const restoreAt = new Date(now + 5 * 60 * 1000).toLocaleTimeString();
      log.warn(`🟡 Circuit breaker L1: 3 consecutive losses — position size cut to 50% until ${restoreAt}`);
      this.alertSystem.emit('circuit_breaker', `🟡 Circuit breaker L1: 3 consecutive losses. 50% size until ${restoreAt}. Edge threshold unchanged.`, {});
      // Schedule L1 auto-restore after 5 min.
      // NOTE: This timer is intentionally ephemeral — NOT persisted to disk.
      // A daemon restart always resets CB state to L0 (see class field defaults),
      // so there is NO scenario where L1 incorrectly persists across a restart.
      // DO NOT add persistence for this timer — it would create state-reconciliation
      // complexity with no benefit. The 5-min window is a within-session cooldown only.
      setTimeout(() => {
        if (this.circuitBreakerLevel === 1) {
          this.circuitBreakerLevel = 0;
          this.circuitBreakerSizeMul = 1.0;
          log.info('Circuit breaker L1 auto-restored: 5 min elapsed, full size restored');
        }
      }, 5 * 60 * 1000);
    }
  }

  /**
   * Deadlock watchdog: called on every analysis tick.
   * If we've been in a non-L0 state for > 15 min with no activity, auto-reset.
   * Prevents the March-26 class of "stuck in CB, can't trade, can't reset" incidents.
   */
  private tickCircuitBreakerWatchdog(): void {
    if (this.circuitBreakerLevel === 0 || this.circuitBreakerLevel === 3) return;
    const now = nowMs();
    const idleMs = now - this.circuitBreakerDeadlockWatchdogAt;
    if (idleMs > 15 * 60 * 1000) {
      const prevLevel = this.circuitBreakerLevel;
      this.circuitBreakerLevel = 0;
      this.circuitBreakerSizeMul = 1.0;
      this.circuitBreakerPauseUntil = 0;
      log.warn(`⚠️ Circuit breaker watchdog: ${(idleMs / 60000).toFixed(1)} min idle at L${prevLevel} → auto-reset to L0`);
      this.alertSystem.emit('circuit_breaker', `⚠️ CB watchdog: idle ${(idleMs / 60000).toFixed(1)} min at L${prevLevel} — auto-reset to L0. Verify market conditions.`, {});
    }
  }
}

// ====== MAIN ======

async function main(): Promise<void> {
  log.info('=== Pump Quant Bot ===');
  log.info(`Mode: ${isPaperMode() ? 'PAPER' : 'LIVE'}`);

  const daemon = new StrategyDaemon();

  // Graceful shutdown with hard deadline.
  // Must complete before supervisor's kill timeout (run-daemon.sh waits 8s then SIGKILL).
  const SHUTDOWN_TIMEOUT_MS = 6000;
  const shutdown = async (signal: string): Promise<void> => {
    log.info(`Received ${signal}, shutting down...`);
    const forceExitTimer = setTimeout(() => {
      log.error('Shutdown timed out after 6s — forcing exit');
      process.exit(1);
    }, SHUTDOWN_TIMEOUT_MS);
    forceExitTimer.unref(); // Don't prevent exit if cleanup finishes naturally
    try {
      await daemon.stop();
    } catch (err) {
      log.error(`Error during shutdown: ${(err as Error).message}`);
    }
    clearTimeout(forceExitTimer);
    process.exit(0);
  };

  process.on('SIGINT', () => shutdown('SIGINT').catch(() => process.exit(1)));
  process.on('SIGTERM', () => shutdown('SIGTERM').catch(() => process.exit(1)));

  await daemon.start();
}

main().catch(err => {
  log.error(`Fatal error: ${err.message}`);
  process.exit(1);
});
