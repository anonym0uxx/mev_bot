/**
 * @module feed/corecast
 * Bitquery CoreCast gRPC client — PRIMARY fast-lane live feed for Solana/Pump.fun.
 *
 * CoreCast provides real-time streaming of:
 * - New token creations on Pump.fun
 * - Token trades (buy/sell) on bonding curves
 * - Migration events
 *
 * This replaces PumpPortal as the primary market data source when enabled.
 * PumpPortal remains as execution venue + supplemental/fallback context.
 *
 * Architecture:
 * - Uses Bitquery Streaming API (EAP) via Server-Sent Events (SSE) over HTTPS
 *   as the initial transport (gRPC client to be added when @grpc/grpc-js is warranted).
 * - Emits the same event types as PumpPortal for seamless daemon integration.
 * - Maintains health stats for fast-lane freshness checks.
 */

import { EventEmitter } from 'events';
import https from 'https';
import http from 'http';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { CoreCastConfig } from '../types/config';
import {
  NewTokenEvent, TokenTradeEvent, MigrationEvent,
} from '../types/events';

const log = createLogger('corecast');

const PUMP_FUN_PROGRAM = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P';

export interface CoreCastEvents {
  newToken: (event: NewTokenEvent) => void;
  tokenTrade: (event: TokenTradeEvent) => void;
  migration: (event: MigrationEvent) => void;
  connected: () => void;
  disconnected: (reason: string) => void;
  error: (error: Error) => void;
}

/**
 * CoreCast streaming client using Bitquery's Streaming GraphQL API.
 *
 * Uses SSE (Server-Sent Events) for real-time streaming — the recommended
 * approach for Bitquery's EAP streaming endpoint. This avoids heavy gRPC
 * dependencies while providing the same low-latency data.
 */
export class CoreCastClient extends EventEmitter {
  private config: CoreCastConfig;
  private apiKey: string;
  private endpoint: string;
  private _connected: boolean = false;
  private lastMessageAt: number = 0;
  private reconnectAttempts: number = 0;
  private reconnectTimer: NodeJS.Timeout | null = null;
  private shouldReconnect: boolean = true;
  private activeStreams: Map<string, http.ClientRequest> = new Map();
  private watchedMints: Set<string> = new Set();
  private messageCount: number = 0;
  private startTime: number = 0;

  constructor(config: CoreCastConfig) {
    super();
    this.config = config;
    this.apiKey = process.env[config.api_key_env] || process.env.BITQUERY_API_KEY || '';
    this.endpoint = config.endpoint || 'https://streaming.bitquery.io/graphql';
  }

  get connected(): boolean {
    return this._connected;
  }

  get lastMessageTime(): number {
    return this.lastMessageAt;
  }

  get stats(): { messageCount: number; uptimeMs: number; lastMessageAt: number } {
    return {
      messageCount: this.messageCount,
      uptimeMs: this.startTime > 0 ? nowMs() - this.startTime : 0,
      lastMessageAt: this.lastMessageAt,
    };
  }

  /**
   * Connect to CoreCast streaming API.
   * Opens SSE streams for new tokens, trades, and migrations.
   */
  async connect(): Promise<void> {
    if (!this.apiKey) {
      throw new Error(`CoreCast API key not found in env var: ${this.config.api_key_env}`);
    }

    this.shouldReconnect = true;
    this.startTime = nowMs();

    try {
      // Start all configured subscriptions
      if (this.config.subscribe_new_tokens) {
        await this.startNewTokenStream();
      }
      if (this.config.subscribe_trades) {
        await this.startTradeStream();
      }
      if (this.config.subscribe_migrations) {
        await this.startMigrationStream();
      }

      this._connected = true;
      this.reconnectAttempts = 0;
      this.lastMessageAt = nowMs();
      this.emit('connected');
      log.info('CoreCast connected — primary fast-lane active');
    } catch (err) {
      log.error(`CoreCast connection failed: ${(err as Error).message}`);
      this.emit('error', err as Error);
      this.scheduleReconnect();
      throw err;
    }
  }

  /** Disconnect all streams */
  disconnect(): void {
    this.shouldReconnect = false;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    for (const [name, req] of this.activeStreams) {
      req.destroy();
      log.debug(`Stream closed: ${name}`);
    }
    this.activeStreams.clear();
    this._connected = false;
    log.info('CoreCast disconnected');
  }

  /** Add mints to the watched set (for trade filtering) */
  watchMints(mints: string[]): void {
    for (const m of mints) this.watchedMints.add(m);
  }

  /** Remove mints from the watched set */
  unwatchMints(mints: string[]): void {
    for (const m of mints) this.watchedMints.delete(m);
  }

  // ====== STREAM SETUP ======

  /**
   * Stream new Pump.fun token creations via Bitquery Streaming GraphQL.
   */
  private async startNewTokenStream(): Promise<void> {
    const query = `
      subscription {
        Solana {
          Instructions(
            where: {
              Instruction: {
                Program: { Address: { is: "${PUMP_FUN_PROGRAM}" } }
              }
              Transaction: { Result: { Success: true } }
            }
          ) {
            Instruction {
              Program { Method }
              Accounts { Address IsWritable Token { Mint Owner } }
              InternalSeqNumber
            }
            Transaction {
              Signature
              Signer
            }
            Block { Time Slot }
          }
        }
      }
    `;

    this.openSSEStream('new_tokens', query, (data: any) => {
      this.handleNewTokenData(data);
    });
  }

  /**
   * Stream Pump.fun trades via Bitquery Streaming GraphQL.
   */
  private async startTradeStream(): Promise<void> {
    const query = `
      subscription {
        Solana {
          DEXTrades(
            where: {
              Trade: {
                Dex: { ProgramAddress: { is: "${PUMP_FUN_PROGRAM}" } }
              }
            }
          ) {
            Block { Slot Time }
            Trade {
              Buy {
                Amount
                Account { Address }
                Price
                Currency { MintAddress Name }
              }
              Sell {
                Amount
                Currency { MintAddress Name }
              }
            }
            Transaction {
              Signature
              Signer
            }
          }
        }
      }
    `;

    this.openSSEStream('trades', query, (data: any) => {
      this.handleTradeData(data);
    });
  }

  /**
   * Stream Pump.fun migration events.
   */
  private async startMigrationStream(): Promise<void> {
    // Migration = token moving from bonding curve to Raydium
    const query = `
      subscription {
        Solana {
          Instructions(
            where: {
              Instruction: {
                Program: { Address: { is: "${PUMP_FUN_PROGRAM}" } }
                Program: { Method: { is: "withdraw" } }
              }
              Transaction: { Result: { Success: true } }
            }
          ) {
            Instruction {
              Program { Method }
              Accounts { Address Token { Mint } }
            }
            Transaction { Signature }
            Block { Time }
          }
        }
      }
    `;

    this.openSSEStream('migrations', query, (data: any) => {
      this.handleMigrationData(data);
    });
  }

  // ====== SSE TRANSPORT ======

  /**
   * Open an SSE stream to the Bitquery Streaming API.
   * Uses POST with subscription query; response is chunked SSE.
   */
  private openSSEStream(
    name: string,
    query: string,
    handler: (data: any) => void
  ): void {
    const url = new URL(this.endpoint);
    const isHttps = url.protocol === 'https:';
    const lib = isHttps ? https : http;

    const body = JSON.stringify({ query });

    const options: https.RequestOptions = {
      hostname: url.hostname,
      port: url.port || (isHttps ? 443 : 80),
      path: url.pathname,
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Authorization': `Bearer ${this.apiKey}`,
        'Accept': 'text/event-stream',
        'Content-Length': Buffer.byteLength(body),
      },
    };

    const req = lib.request(options, (res) => {
      if (res.statusCode !== 200) {
        const errMsg = `CoreCast stream ${name} HTTP ${res.statusCode}`;
        log.error(errMsg);
        this.emit('error', new Error(errMsg));
        return;
      }

      log.info(`CoreCast stream ${name} connected`);
      let buffer = '';

      res.on('data', (chunk: Buffer) => {
        buffer += chunk.toString();

        // Parse SSE events
        const lines = buffer.split('\n');
        buffer = lines.pop() || ''; // Keep incomplete line in buffer

        for (const line of lines) {
          if (line.startsWith('data: ')) {
            try {
              const data = JSON.parse(line.slice(6));
              this.lastMessageAt = nowMs();
              this.messageCount++;
              handler(data);
            } catch (err) {
              log.debug(`CoreCast ${name} parse error: ${(err as Error).message}`);
            }
          }
        }
      });

      res.on('end', () => {
        log.warn(`CoreCast stream ${name} ended`);
        this.activeStreams.delete(name);
        this.checkConnectionHealth();
      });

      res.on('error', (err) => {
        log.error(`CoreCast stream ${name} error: ${err.message}`);
        this.activeStreams.delete(name);
        this.checkConnectionHealth();
      });
    });

    req.on('error', (err) => {
      log.error(`CoreCast ${name} request error: ${err.message}`);
      this.activeStreams.delete(name);
      this.checkConnectionHealth();
    });

    req.write(body);
    req.end();
    this.activeStreams.set(name, req);
  }

  // ====== DATA HANDLERS ======

  private handleNewTokenData(data: any): void {
    try {
      const instructions = data?.data?.Solana?.Instructions || [];
      for (const instr of instructions) {
        if (instr.Instruction?.Program?.Method !== 'create') continue;

        const accounts = instr.Instruction?.Accounts || [];
        const mint = this.extractMintFromAccounts(accounts);
        if (!mint) continue;

        const event: NewTokenEvent = {
          signature: instr.Transaction?.Signature || '',
          mint,
          traderPublicKey: instr.Transaction?.Signer || '',
          txType: 'create',
          initialBuy: 0,
          bondingCurveKey: this.extractBondingCurveFromAccounts(accounts),
          vTokensInBondingCurve: 0,
          vSolInBondingCurve: 0,
          marketCapSol: 0,
          name: '',
          symbol: '',
          uri: '',
          pool: undefined,
          timestamp: new Date(instr.Block?.Time || 0).getTime() || nowMs(),
        };

        this.emit('newToken', event);
      }
    } catch (err) {
      log.debug(`CoreCast new token parse error: ${(err as Error).message}`);
    }
  }

  private handleTradeData(data: any): void {
    try {
      const trades = data?.data?.Solana?.DEXTrades || [];
      for (const trade of trades) {
        const mint = trade.Trade?.Currency?.MintAddress;
        if (!mint) continue;

        // Only process trades for watched mints (or all if no filter)
        if (this.watchedMints.size > 0 && !this.watchedMints.has(mint)) continue;

        const side = trade.Trade?.Side?.Type?.toLowerCase();
        const isBuy = side === 'buy';

        const event: TokenTradeEvent = {
          signature: trade.Transaction?.Signature || '',
          mint,
          traderPublicKey: trade.Transaction?.Signer || '',
          txType: isBuy ? 'buy' : 'sell',
          tokenAmount: parseFloat(trade.Trade?.Amount || '0'),
          solAmount: parseFloat(trade.Trade?.Side?.Amount?.Amount || '0'),
          newTokenBalance: 0, // Not directly available from CoreCast
          bondingCurveKey: '',
          vTokensInBondingCurve: 0, // Will be enriched by deep lane
          vSolInBondingCurve: 0,
          marketCapSol: 0,
          timestamp: new Date(trade.Block?.Time || 0).getTime() || nowMs(),
        };

        this.emit('tokenTrade', event);
      }
    } catch (err) {
      log.debug(`CoreCast trade parse error: ${(err as Error).message}`);
    }
  }

  private handleMigrationData(data: any): void {
    try {
      const instructions = data?.data?.Solana?.Instructions || [];
      for (const instr of instructions) {
        const accounts = instr.Instruction?.Accounts || [];
        const mint = this.extractMintFromAccounts(accounts);
        if (!mint) continue;

        const event: MigrationEvent = {
          signature: instr.Transaction?.Signature || '',
          mint,
          pool: undefined,
          timestamp: new Date(instr.Block?.Time || 0).getTime() || nowMs(),
        };

        this.emit('migration', event);
      }
    } catch (err) {
      log.debug(`CoreCast migration parse error: ${(err as Error).message}`);
    }
  }

  // ====== HELPERS ======

  private extractMintFromAccounts(accounts: any[]): string {
    // In Pump.fun instructions, the mint is typically the 3rd account (index 2)
    // or has Token.Mint set
    for (const acct of accounts) {
      if (acct.Token?.Mint) return acct.Token.Mint;
    }
    // Fallback: look for a writable non-system account
    if (accounts.length > 2) return accounts[2]?.Address || '';
    return '';
  }

  private extractBondingCurveFromAccounts(accounts: any[]): string {
    // Bonding curve is typically the 4th account (index 3)
    if (accounts.length > 3) return accounts[3]?.Address || '';
    return '';
  }

  private checkConnectionHealth(): void {
    if (this.activeStreams.size === 0 && this._connected) {
      this._connected = false;
      this.emit('disconnected', 'All streams closed');
      this.scheduleReconnect();
    }
  }

  private scheduleReconnect(): void {
    if (!this.shouldReconnect) return;

    const delay = Math.min(
      this.config.reconnect_base_ms * Math.pow(2, this.reconnectAttempts),
      this.config.reconnect_max_ms
    );
    this.reconnectAttempts++;

    log.info(`CoreCast reconnecting in ${delay}ms (attempt ${this.reconnectAttempts})`);
    this.reconnectTimer = setTimeout(() => {
      this.connect().catch(err => {
        log.error(`CoreCast reconnect failed: ${err.message}`);
      });
    }, delay);
  }
}
