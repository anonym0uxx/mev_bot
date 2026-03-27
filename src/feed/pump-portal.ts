/**
 * @module feed/pump-portal
 * PumpPortal WebSocket client for live market data.
 * Single connection for: subscribeNewToken, subscribeMigration,
 * subscribeTokenTrade, subscribeAccountTrade.
 */

import WebSocket from 'ws';
import { EventEmitter } from 'events';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import {
  NewTokenEvent, TokenTradeEvent, AccountTradeEvent, MigrationEvent,
} from '../types/events';

const log = createLogger('pump-portal');

const DEFAULT_WS_URL = 'wss://pumpportal.fun/api/data';
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 30000;
const PING_INTERVAL_MS = 30000;

export interface PumpPortalEvents {
  newToken: (event: NewTokenEvent) => void;
  tokenTrade: (event: TokenTradeEvent) => void;
  accountTrade: (event: AccountTradeEvent) => void;
  migration: (event: MigrationEvent) => void;
  connected: () => void;
  disconnected: (reason: string) => void;
  error: (error: Error) => void;
}

export class PumpPortalClient extends EventEmitter {
  private ws: WebSocket | null = null;
  private wsUrl: string;
  private apiKey: string;
  private reconnectAttempts: number = 0;
  private reconnectTimer: NodeJS.Timeout | null = null;
  private pingTimer: NodeJS.Timeout | null = null;
  private subscribedTokens: Set<string> = new Set();
  private subscribedAccounts: Set<string> = new Set();
  private isConnecting: boolean = false;
  private shouldReconnect: boolean = true;
  private lastMessageAt: number = 0;
  private _connected: boolean = false;

  constructor(apiKey?: string, wsUrl?: string) {
    super();
    this.apiKey = apiKey || process.env.PUMP_PORTAL_API_KEY || '';
    this.wsUrl = wsUrl || process.env.PUMP_PORTAL_WS_URL || DEFAULT_WS_URL;
  }

  /** Whether the client is currently connected */
  get connected(): boolean {
    return this._connected;
  }

  /** Time of last received message */
  get lastMessageTime(): number {
    return this.lastMessageAt;
  }

  /** Connect to PumpPortal WebSocket */
  async connect(): Promise<void> {
    if (this.isConnecting || this._connected) return;
    this.isConnecting = true;
    this.shouldReconnect = true;

    return new Promise((resolve, reject) => {
      try {
        this.ws = new WebSocket(this.wsUrl);

        this.ws.on('open', () => {
          this._connected = true;
          this.isConnecting = false;
          this.reconnectAttempts = 0;
          this.lastMessageAt = nowMs();
          log.info('PumpPortal WebSocket connected');

          this.startPingTimer();
          this.subscribeNewTokens();
          this.subscribeMigrations();

          // Re-subscribe to any previously watched tokens/accounts
          if (this.subscribedTokens.size > 0) {
            this.subscribeTokenTrades([...this.subscribedTokens]);
          }
          if (this.subscribedAccounts.size > 0) {
            this.subscribeAccountTrades([...this.subscribedAccounts]);
          }

          this.emit('connected');
          resolve();
        });

        this.ws.on('message', (data: WebSocket.Data) => {
          this.lastMessageAt = nowMs();
          try {
            const msg = JSON.parse(data.toString());
            this.handleMessage(msg);
          } catch (err) {
            log.warn('Failed to parse PumpPortal message', { error: (err as Error).message });
          }
        });

        this.ws.on('close', (code: number, reason: Buffer) => {
          const reasonStr = reason.toString() || `code=${code}`;
          this._connected = false;
          this.isConnecting = false;
          log.warn(`PumpPortal WebSocket closed: ${reasonStr}`);
          this.stopPingTimer();
          this.emit('disconnected', reasonStr);
          this.scheduleReconnect();
        });

        this.ws.on('error', (err: Error) => {
          this.isConnecting = false;
          log.error(`PumpPortal WebSocket error: ${err.message}`);
          this.emit('error', err);
          if (!this._connected) {
            reject(err);
          }
        });
      } catch (err) {
        this.isConnecting = false;
        reject(err);
      }
    });
  }

  /** Disconnect from WebSocket */
  disconnect(): void {
    this.shouldReconnect = false;
    this.stopPingTimer();
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
    this._connected = false;
    log.info('PumpPortal disconnected');
  }

  /** Subscribe to new token creation events */
  private subscribeNewTokens(): void {
    this.send({ method: 'subscribeNewToken' });
    log.info('Subscribed to new tokens');
  }

  /** Subscribe to migration events */
  private subscribeMigrations(): void {
    this.send({ method: 'subscribeMigration' });
    log.info('Subscribed to migrations');
  }

  /** Subscribe to trades for specific token mints */
  subscribeTokenTrades(mints: string[]): void {
    if (mints.length === 0) return;
    for (const mint of mints) {
      this.subscribedTokens.add(mint);
    }
    this.send({ method: 'subscribeTokenTrade', keys: mints });
    log.debug(`Subscribed to token trades for ${mints.length} mints`);
  }

  /** Unsubscribe from token trades */
  unsubscribeTokenTrades(mints: string[]): void {
    for (const mint of mints) {
      this.subscribedTokens.delete(mint);
    }
    this.send({ method: 'unsubscribeTokenTrade', keys: mints });
    log.debug(`Unsubscribed from token trades for ${mints.length} mints`);
  }

  /** Subscribe to trades for specific wallet accounts */
  subscribeAccountTrades(accounts: string[]): void {
    if (accounts.length === 0) return;
    for (const acct of accounts) {
      this.subscribedAccounts.add(acct);
    }
    this.send({ method: 'subscribeAccountTrade', keys: accounts });
    log.debug(`Subscribed to account trades for ${accounts.length} accounts`);
  }

  /** Unsubscribe from account trades */
  unsubscribeAccountTrades(accounts: string[]): void {
    for (const acct of accounts) {
      this.subscribedAccounts.delete(acct);
    }
    this.send({ method: 'unsubscribeAccountTrade', keys: accounts });
  }

  /** Get set of currently subscribed token mints */
  getSubscribedTokens(): ReadonlySet<string> {
    return this.subscribedTokens;
  }

  /** Handle incoming WebSocket message */
  private handleMessage(msg: any): void {
    // Detect message type from PumpPortal format
    if (msg.txType === 'create') {
      const event: NewTokenEvent = {
        signature: msg.signature,
        mint: msg.mint,
        traderPublicKey: msg.traderPublicKey,
        txType: 'create',
        initialBuy: msg.tokenAmount || 0,
        bondingCurveKey: msg.bondingCurveKey || '',
        vTokensInBondingCurve: msg.vTokensInBondingCurve || 0,
        vSolInBondingCurve: msg.vSolInBondingCurve || 0,
        marketCapSol: msg.marketCapSol || 0,
        name: msg.name || '',
        symbol: msg.symbol || '',
        uri: msg.uri || '',
        pool: msg.pool,
        timestamp: msg.timestamp || nowMs(),
      };
      this.emit('newToken', event);
    } else if (msg.txType === 'buy' || msg.txType === 'sell') {
      // Check if this is an account trade or token trade
      if (this.subscribedAccounts.has(msg.traderPublicKey)) {
        const event: AccountTradeEvent = {
          signature: msg.signature,
          mint: msg.mint,
          traderPublicKey: msg.traderPublicKey,
          txType: msg.txType,
          tokenAmount: msg.tokenAmount || 0,
          solAmount: msg.solAmount || 0,
          newTokenBalance: msg.newTokenBalance || 0,
          bondingCurveKey: msg.bondingCurveKey || '',
          vTokensInBondingCurve: msg.vTokensInBondingCurve || 0,
          vSolInBondingCurve: msg.vSolInBondingCurve || 0,
          marketCapSol: msg.marketCapSol || 0,
          timestamp: msg.timestamp || nowMs(),
        };
        this.emit('accountTrade', event);
      }

      // Also emit as token trade if we're watching this mint
      if (this.subscribedTokens.has(msg.mint) || msg.txType) {
        const event: TokenTradeEvent = {
          signature: msg.signature,
          mint: msg.mint,
          traderPublicKey: msg.traderPublicKey,
          txType: msg.txType,
          tokenAmount: msg.tokenAmount || 0,
          solAmount: msg.solAmount || 0,
          newTokenBalance: msg.newTokenBalance || 0,
          bondingCurveKey: msg.bondingCurveKey || '',
          vTokensInBondingCurve: msg.vTokensInBondingCurve || 0,
          vSolInBondingCurve: msg.vSolInBondingCurve || 0,
          marketCapSol: msg.marketCapSol || 0,
          timestamp: msg.timestamp || nowMs(),
        };
        this.emit('tokenTrade', event);
        // Per-mint event for MEV / backrun modules that need targeted routing
        this.emit('trade:' + event.mint, event);
      }
    } else if (msg.pool || msg.migration) {
      const event: MigrationEvent = {
        signature: msg.signature || '',
        mint: msg.mint,
        pool: msg.pool,
        timestamp: msg.timestamp || nowMs(),
      };
      this.emit('migration', event);
    }
  }

  /** Send a message via WebSocket */
  private send(msg: Record<string, unknown>): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      log.warn('Cannot send: WebSocket not connected');
      return;
    }
    this.ws.send(JSON.stringify(msg));
  }

  /** Schedule reconnect with exponential backoff */
  private scheduleReconnect(): void {
    if (!this.shouldReconnect) return;

    const delay = Math.min(
      RECONNECT_BASE_MS * Math.pow(2, this.reconnectAttempts),
      RECONNECT_MAX_MS
    );
    this.reconnectAttempts += 1;

    log.info(`Reconnecting in ${delay}ms (attempt ${this.reconnectAttempts})`);
    this.reconnectTimer = setTimeout(() => {
      this.connect().catch(err => {
        log.error(`Reconnect failed: ${err.message}`);
      });
    }, delay);
  }

  /** Start ping timer to keep connection alive */
  private startPingTimer(): void {
    this.stopPingTimer();
    this.pingTimer = setInterval(() => {
      if (this.ws && this.ws.readyState === WebSocket.OPEN) {
        this.ws.ping();
      }
    }, PING_INTERVAL_MS);
  }

  /** Stop ping timer */
  private stopPingTimer(): void {
    if (this.pingTimer) {
      clearInterval(this.pingTimer);
      this.pingTimer = null;
    }
  }
}
