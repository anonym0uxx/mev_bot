/**
 * @module execution/adapter
 * Execution adapter: bridges strategy decisions to Solana transaction execution.
 * Handles: tx construction, signing, sending, confirmation, fill recording.
 * Routes: Local (default), Lightning (conditional), Jito (atomic only).
 *
 * Also supports PumpPortal API integration for trade execution.
 */

import { Connection, Keypair, PublicKey, LAMPORTS_PER_SOL } from '@solana/web3.js';
import fetch from 'node-fetch';
import { v4 as uuidv4 } from 'uuid';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { PumpQuantConfig, RouteMode } from '../types/config';
import { Order, OrderStatus, TradeIntent } from '../types/trade';
import { PumpQuantDB } from '../persistence/database';
import {
  initSolanaConnection, loadWalletKeypair,
  buildBuyTransaction, buildSellTransaction, signAndSendTransaction,
} from './solana';
import { RoutePolicy, RouteExecutionRecord } from './route-policy';

const log = createLogger('execution');

const PUMP_PORTAL_API_URL = process.env.PUMP_PORTAL_API_URL || 'https://pumpportal.fun/api';

export class ExecutionAdapter {
  private connection: Connection;
  private wallet: Keypair;
  private routePolicy: RoutePolicy;
  private db: PumpQuantDB;
  private config: PumpQuantConfig;
  private isPaper: boolean;

  constructor(db: PumpQuantDB, config: PumpQuantConfig, isPaper: boolean = false) {
    this.db = db;
    this.config = config;
    this.isPaper = isPaper;
    this.routePolicy = new RoutePolicy(config.execution);

    if (!isPaper) {
      this.connection = initSolanaConnection();
      this.wallet = loadWalletKeypair();
      log.info(`Execution adapter initialized: wallet=${this.wallet.publicKey.toBase58()}`);
    } else {
      // Paper mode: mock connection and wallet
      this.connection = null as any;
      this.wallet = null as any;
      log.info('Execution adapter initialized in PAPER mode');
    }
  }

  /** Update config */
  updateConfig(config: PumpQuantConfig): void {
    this.config = config;
    this.routePolicy.updateConfig(config.execution);
  }

  /** Get route policy for health stats */
  getRoutePolicy(): RoutePolicy {
    return this.routePolicy;
  }

  /**
   * Execute a buy trade.
   * HARD CLAMP: enforces max_position_size_sol regardless of upstream sizing logic.
   * This is a last-resort guard — the strategy layer should also enforce limits,
   * but this ensures no oversize order ever reaches the chain.
   */
  async executeBuy(intent: TradeIntent): Promise<Order> {
    const hardLimit = this.config.risk.max_position_size_sol;
    if (intent.size_sol > hardLimit) {
      const err = new Error(
        `EXECUTION BLOCKED: size_sol=${intent.size_sol.toFixed(4)} exceeds ` +
        `max_position_size_sol=${hardLimit} for mint=${intent.mint.slice(0, 8)}. ` +
        `Order rejected before signing.`
      );
      log.error(err.message);
      throw err;
    }

    const order = this.createOrderFromIntent(intent);
    this.db.insertOrder(order);

    if (this.isPaper) {
      return this.executePaperBuy(order, intent);
    }

    return this.executeLiveBuy(order, intent);
  }

  /**
   * Execute a sell trade.
   */
  async executeSell(intent: TradeIntent): Promise<Order> {
    const order = this.createOrderFromIntent(intent);
    this.db.insertOrder(order);

    if (this.isPaper) {
      return this.executePaperSell(order, intent);
    }

    return this.executeLiveSell(order, intent);
  }

  /** Get wallet balance (live mode only) */
  async getBalance(): Promise<number> {
    if (this.isPaper) return this.config.risk.bankroll_sol;
    // PumpPortal wallet is the actual trading wallet — check its balance.
    // Falls back to WALLET_PRIVATE_KEY wallet if no PumpPortal pubkey configured.
    // NOTE: Use the already-imported PublicKey and LAMPORTS_PER_SOL (top-level static import),
    // and this.connection which is already initialised in the constructor.
    // The previous dynamic import(  '@solana/web3.js') inside an async method was fragile
    // and redundant — removed.
    const pubKeyStr = process.env.PUMP_PORTAL_PUBLIC_KEY || this.wallet.publicKey.toBase58();
    const pubKey = new PublicKey(pubKeyStr);
    const balance = await this.connection.getBalance(pubKey);
    return balance / LAMPORTS_PER_SOL;
  }

  /** Get wallet public key */
  getPublicKey(): string {
    if (this.isPaper) return 'PAPER_MODE_WALLET';
    return this.wallet.publicKey.toBase58();
  }

  // ====== LIVE EXECUTION ======

  private async executeLiveBuy(order: Order, intent: TradeIntent): Promise<Order> {
    const startTime = nowMs();

    try {
      order.status = OrderStatus.SENT;
      order.sent_at = startTime;
      this.db.updateOrder(order.id, { status: order.status, sent_at: order.sent_at });

      // Use PumpPortal API for all routes — avoids Solana public RPC rate limits
      let result: { signature: string; confirmedAt: number };
      result = await this.executeViaPumpPortal(intent, 'buy');

      // Record fill
      order.tx_signature = result.signature;
      order.confirmed_at = result.confirmedAt;
      order.status = OrderStatus.CONFIRMED;
      order.realized_sol = intent.size_sol;
      // Token amount would be parsed from transaction logs in production
      order.realized_tokens = 0; // Will be updated from on-chain data
      order.realized_price = intent.size_sol; // Simplified — real impl parses logs
      order.realized_slippage_pct = 0; // Computed from expected vs realized
      // FIX: include pump_portal_fee_pct (0.5%) — was previously only counting pump_fee_pct (1%)
      order.fee_sol = (this.config.fees.pump_fee_pct + this.config.fees.pump_portal_fee_pct) * intent.size_sol + this.config.fees.solana_base_fee_sol;
      order.priority_fee_paid_sol = intent.priority_fee_sol;

      this.db.updateOrder(order.id, {
        tx_signature: order.tx_signature,
        confirmed_at: order.confirmed_at,
        status: order.status,
        realized_sol: order.realized_sol,
        realized_tokens: order.realized_tokens,
        realized_price: order.realized_price,
        realized_slippage_pct: order.realized_slippage_pct,
        fee_sol: order.fee_sol,
        priority_fee_paid_sol: order.priority_fee_paid_sol,
      });

      // Record for route health
      this.routePolicy.recordExecution({
        mode: intent.route_mode,
        success: true,
        landingLatencyMs: (result.confirmedAt - startTime),
        retried: false,
        feePaid: order.fee_sol + order.priority_fee_paid_sol,
        tradeSizeSol: intent.size_sol,
        timestamp: nowMs(),
      });

      log.info(`Buy executed: ${intent.mint} ${intent.size_sol} SOL, sig=${result.signature}`);
      return order;
    } catch (err) {
      order.status = OrderStatus.FAILED;
      order.error = (err as Error).message;
      this.db.updateOrder(order.id, { status: order.status, error: order.error });

      this.routePolicy.recordExecution({
        mode: intent.route_mode,
        success: false,
        landingLatencyMs: nowMs() - startTime,
        retried: false,
        feePaid: 0,
        tradeSizeSol: intent.size_sol,
        timestamp: nowMs(),
      });

      log.error(`Buy failed: ${intent.mint} — ${(err as Error).message}`);
      throw err;
    }
  }

  private async executeLiveSell(order: Order, intent: TradeIntent): Promise<Order> {
    const startTime = nowMs();

    try {
      order.status = OrderStatus.SENT;
      order.sent_at = startTime;
      this.db.updateOrder(order.id, { status: order.status, sent_at: order.sent_at });

      let result: { signature: string; confirmedAt: number };
      // Use PumpPortal API for all routes
      result = await this.executeViaPumpPortal(intent, 'sell');
      if (false) { // Dead code — kept for reference
        result = await this.executeViaSolana(intent, 'sell');
      }

      order.tx_signature = result.signature;
      order.confirmed_at = result.confirmedAt;
      order.status = OrderStatus.CONFIRMED;
      // FIX: for sells, realized_sol should be estimated SOL received (after slippage).
      // PumpPortal doesn't return the exact fill amount; approximate with configured slippage.
      // This gives more accurate P&L than using input notional (which ignored slippage entirely).
      const estimatedSlippage = (intent.slippage_bps / 10000) * 0.5; // assume ~50% of max slippage is realized
      order.realized_sol = intent.size_sol * (1 - estimatedSlippage);
      order.realized_slippage_pct = estimatedSlippage;
      // FIX: include pump_portal_fee_pct (0.5%) — was only counting pump_fee_pct (1%)
      order.fee_sol = (this.config.fees.pump_fee_pct + this.config.fees.pump_portal_fee_pct) * intent.size_sol + this.config.fees.solana_base_fee_sol;
      order.priority_fee_paid_sol = intent.priority_fee_sol;

      this.db.updateOrder(order.id, {
        tx_signature: order.tx_signature,
        confirmed_at: order.confirmed_at,
        status: order.status,
        realized_sol: order.realized_sol,
        realized_slippage_pct: order.realized_slippage_pct,
        fee_sol: order.fee_sol,
        priority_fee_paid_sol: order.priority_fee_paid_sol,
      });

      this.routePolicy.recordExecution({
        mode: intent.route_mode,
        success: true,
        landingLatencyMs: (result.confirmedAt - startTime),
        retried: false,
        feePaid: order.fee_sol + order.priority_fee_paid_sol,
        tradeSizeSol: intent.size_sol,
        timestamp: nowMs(),
      });

      log.info(`Sell executed: ${intent.mint} ${intent.amount_pct}%, sig=${result.signature}`);
      return order;
    } catch (err) {
      order.status = OrderStatus.FAILED;
      order.error = (err as Error).message;
      this.db.updateOrder(order.id, { status: order.status, error: order.error });

      this.routePolicy.recordExecution({
        mode: intent.route_mode,
        success: false,
        landingLatencyMs: nowMs() - startTime,
        retried: false,
        feePaid: 0,
        tradeSizeSol: intent.size_sol,
        timestamp: nowMs(),
      });

      log.error(`Sell failed: ${intent.mint} — ${(err as Error).message}`);
      throw err;
    }
  }

  /** Execute via direct Solana RPC (Local route) */
  private async executeViaSolana(
    intent: TradeIntent,
    side: 'buy' | 'sell'
  ): Promise<{ signature: string; confirmedAt: number }> {
    const mint = new PublicKey(intent.mint);
    // Derive bonding curve from mint — in production this comes from the token state
    const bondingCurve = mint; // Placeholder — real impl derives from PDA

    let tx;
    if (side === 'buy') {
      tx = buildBuyTransaction(
        this.wallet, mint, bondingCurve,
        intent.size_sol, intent.slippage_bps, intent.priority_fee_sol
      );
    } else {
      tx = buildSellTransaction(
        this.wallet, mint, bondingCurve,
        intent.size_sol, intent.slippage_bps, intent.priority_fee_sol
      );
    }

    return signAndSendTransaction(
      this.connection, this.wallet, tx,
      this.config.execution.skip_preflight,
      this.config.execution.confirmation_timeout_ms
    );
  }

  /** Execute via PumpPortal API (Lightning route) */
  private async executeViaPumpPortal(
    intent: TradeIntent,
    side: 'buy' | 'sell'
  ): Promise<{ signature: string; confirmedAt: number }> {
    const apiKey = process.env.PUMP_PORTAL_API_KEY;
    if (!apiKey) throw new Error('PUMP_PORTAL_API_KEY not set');

    // Use PumpPortal hosted trade API — handles tx construction + submission
    // For sells: use percentage string (e.g. "100%") — PumpPortal resolves token balance server-side
    // This avoids the bug where we don't know exact token amount post-buy
    let amount: number | string;
    let denominatedInSol: string;
    if (side === 'buy') {
      amount = intent.size_sol;
      denominatedInSol = 'true';
    } else {
      // PumpPortal accepts percentage strings for sells
      const pct = intent.amount_pct || 100;
      amount = `${pct}%`;
      denominatedInSol = 'false';
    }

    // PumpPortal requires wallet keys to sign and submit transactions
    const publicKey = process.env.PUMP_PORTAL_PUBLIC_KEY || this.wallet.publicKey.toBase58();
    const privateKey = process.env.PUMP_PORTAL_PRIVATE_KEY || process.env.WALLET_PRIVATE_KEY;
    
    if (!privateKey) {
      throw new Error('Missing wallet private key for PumpPortal trade execution');
    }

    const body: Record<string, any> = {
      publicKey,
      privateKey,
      action: side,
      mint: intent.mint,
      amount,
      denominatedInSol,
      slippage: intent.slippage_bps / 100,
      priorityFee: intent.priority_fee_sol,
      pool: 'pump',
    };

    log.info(`PumpPortal ${side}: mint=${intent.mint.slice(0,8)} amount=${body.amount} slippage=${body.slippage}%`);

    const startTime = nowMs();
    const response = await fetch(
      `${PUMP_PORTAL_API_URL}/trade?api-key=${apiKey}`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      }
    );

    const responseText = await response.text();

    if (!response.ok) {
      throw new Error(`PumpPortal trade API ${response.status}: ${responseText}`);
    }

    // Response is the tx signature string or JSON with signature
    let signature = '';
    try {
      const parsed = JSON.parse(responseText);
      signature = parsed.signature || parsed.txid || parsed.tx || responseText;
    } catch {
      signature = responseText.trim().replace(/"/g, '');
    }

    log.info(`PumpPortal ${side} success: ${signature}`);
    const confirmedAt = nowMs();

    return { signature, confirmedAt };
  }

  // ====== PAPER EXECUTION ======

  private async executePaperBuy(order: Order, intent: TradeIntent): Promise<Order> {
    // Simulate fill with configurable slippage
    const slippagePct = this.config.friction.default_entry_slippage_pct;
    const effectivePrice = intent.size_sol * (1 + slippagePct);

    order.status = OrderStatus.CONFIRMED;
    order.sent_at = nowMs();
    order.confirmed_at = nowMs() + 500; // Simulate 500ms latency
    order.tx_signature = `paper_${uuidv4().substring(0, 8)}`;
    order.realized_sol = intent.size_sol;
    order.realized_tokens = intent.size_sol * 1_000_000; // Placeholder token amount
    order.realized_price = effectivePrice;
    order.realized_slippage_pct = slippagePct;
    order.fee_sol = this.config.fees.pump_fee_pct * intent.size_sol + this.config.fees.solana_base_fee_sol;
    order.priority_fee_paid_sol = intent.priority_fee_sol;
    order.is_paper = true;

    this.db.updateOrder(order.id, {
      status: order.status,
      sent_at: order.sent_at,
      confirmed_at: order.confirmed_at,
      tx_signature: order.tx_signature,
      realized_sol: order.realized_sol,
      realized_tokens: order.realized_tokens,
      realized_price: order.realized_price,
      realized_slippage_pct: order.realized_slippage_pct,
      fee_sol: order.fee_sol,
      priority_fee_paid_sol: order.priority_fee_paid_sol,
      is_paper: true,
    });

    log.info(`Paper buy: ${intent.mint} ${intent.size_sol} SOL`);
    return order;
  }

  private async executePaperSell(order: Order, intent: TradeIntent): Promise<Order> {
    const slippagePct = this.config.friction.default_exit_slippage_pct;

    order.status = OrderStatus.CONFIRMED;
    order.sent_at = nowMs();
    order.confirmed_at = nowMs() + 500;
    order.tx_signature = `paper_${uuidv4().substring(0, 8)}`;
    order.realized_sol = intent.size_sol * (1 - slippagePct);
    order.realized_slippage_pct = slippagePct;
    order.fee_sol = this.config.fees.pump_fee_pct * intent.size_sol + this.config.fees.solana_base_fee_sol;
    order.priority_fee_paid_sol = intent.priority_fee_sol;
    order.is_paper = true;

    this.db.updateOrder(order.id, {
      status: order.status,
      sent_at: order.sent_at,
      confirmed_at: order.confirmed_at,
      tx_signature: order.tx_signature,
      realized_sol: order.realized_sol,
      realized_slippage_pct: order.realized_slippage_pct,
      fee_sol: order.fee_sol,
      priority_fee_paid_sol: order.priority_fee_paid_sol,
      is_paper: true,
    });

    log.info(`Paper sell: ${intent.mint} ${intent.amount_pct}%`);
    return order;
  }

  // ====== HELPERS ======

  private createOrderFromIntent(intent: TradeIntent): Order {
    return {
      id: uuidv4(),
      trade_intent_id: intent.id,
      mint: intent.mint,
      side: intent.side,
      size_sol: intent.size_sol,
      amount_pct: intent.amount_pct,
      slippage_bps: intent.slippage_bps,
      priority_fee_sol: intent.priority_fee_sol,
      route_mode: intent.route_mode,
      status: OrderStatus.PENDING,
      tx_signature: null,
      created_at: nowMs(),
      sent_at: null,
      confirmed_at: null,
      realized_sol: null,
      realized_tokens: null,
      realized_price: null,
      realized_slippage_pct: null,
      fee_sol: null,
      priority_fee_paid_sol: null,
      error: null,
      retry_count: 0,
      config_version: intent.config_version,
      is_paper: this.isPaper,
    };
  }
}
