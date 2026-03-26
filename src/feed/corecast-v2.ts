/**
 * @module feed/corecast-v2
 * Bitquery CoreCast client — PRIMARY fast-lane live feed for Solana/Pump.fun.
 * 
 * Rewritten to use Bitquery's actual subscription protocol (polling-based GraphQL).
 */

import { EventEmitter } from 'events';
import https from 'https';
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

export class CoreCastClient extends EventEmitter {
  private config: CoreCastConfig;
  private apiKey: string;
  private endpoint: string;
  private _connected: boolean = false;
  private lastMessageAt: number = 0;
  private reconnectAttempts: number = 0;
  private shouldReconnect: boolean = true;
  private pollers: Map<string, NodeJS.Timeout> = new Map();
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

  async connect(): Promise<void> {
    if (!this.apiKey) {
      throw new Error(`CoreCast API key not found in env var: ${this.config.api_key_env}`);
    }

    this.shouldReconnect = true;
    this.startTime = nowMs();

    try {
      // Start all configured subscriptions (polling-based)
      if (this.config.subscribe_trades) {
        this.startTradePoller();
      }
      if (this.config.subscribe_new_tokens) {
        this.startNewTokenPoller();
      }
      if (this.config.subscribe_migrations) {
        this.startMigrationPoller();
      }

      this._connected = true;
      this.reconnectAttempts = 0;
      this.lastMessageAt = nowMs();
      this.emit('connected');
      log.info('CoreCast connected — primary fast-lane active');
    } catch (err) {
      log.error(`CoreCast connection failed: ${(err as Error).message}`);
      this.emit('error', err as Error);
      throw err;
    }
  }

  disconnect(): void {
    this.shouldReconnect = false;
    for (const [name, timer] of this.pollers) {
      clearTimeout(timer);
      log.debug(`Poller stopped: ${name}`);
    }
    this.pollers.clear();
    this._connected = false;
    log.info('CoreCast disconnected');
  }

  watchMints(_mints: string[]): void {
    // No-op for polling-based implementation
  }

  unwatchMints(_mints: string[]): void {
    // No-op for polling-based implementation
  }

  // ====== POLLERS ======

  private startTradePoller(): void {
    const query = `subscription {
      Solana {
        DEXTrades(
          where: {
            Trade: {
              Dex: { ProgramAddress: { is: "${PUMP_FUN_PROGRAM}" } }
            }
          }
          limit: {count: 50}
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
    }`;

    this.poll('trades', query, 1000, (data: any) => {
      if (data.Solana?.DEXTrades) {
        for (const trade of data.Solana.DEXTrades) {
          this.handleTradeData(trade);
        }
      }
    });
  }

  private startNewTokenPoller(): void {
    const query = `subscription {
      Solana {
        Instructions(
          where: {
            Instruction: {
              Program: { Address: { is: "${PUMP_FUN_PROGRAM}" } }
            }
            Transaction: { Result: { Success: true } }
          }
          limit: {count: 20}
        ) {
          Instruction {
            Program { Method }
            Accounts { Address IsWritable }
          }
          Transaction {
            Signature
            Signer
          }
          Block { Time Slot }
        }
      }
    }`;

    this.poll('new_tokens', query, 2000, (data: any) => {
      if (data.Solana?.Instructions) {
        for (const instr of data.Solana.Instructions) {
          this.handleNewTokenData(instr);
        }
      }
    });
  }

  private startMigrationPoller(): void {
    const query = `subscription {
      Solana {
        Instructions(
          where: {
            Instruction: {
              Program: { Address: { is: "${PUMP_FUN_PROGRAM}" } }
              Program: { Method: { is: "migrate" } }
            }
            Transaction: { Result: { Success: true } }
          }
          limit: {count: 10}
        ) {
          Instruction {
            Accounts { Address }
          }
          Transaction { Signature }
          Block { Time Slot }
        }
      }
    }`;

    this.poll('migrations', query, 5000, (data: any) => {
      if (data.Solana?.Instructions) {
        for (const instr of data.Solana.Instructions) {
          this.handleMigrationData(instr);
        }
      }
    });
  }

  private poll(name: string, query: string, intervalMs: number, handler: (data: any) => void): void {
    const doPoll = () => {
      if (!this.shouldReconnect) return;

      const body = JSON.stringify({ query });
      const url = new URL(this.endpoint);

      const options: https.RequestOptions = {
        hostname: url.hostname,
        port: url.port || 443,
        path: url.pathname,
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${this.apiKey}`,
          'Accept': 'application/json',
          'Content-Length': Buffer.byteLength(body),
        },
      };

      const req = https.request(options, (res) => {
        let buffer = '';
        res.on('data', (chunk: Buffer) => { buffer += chunk.toString(); });
        res.on('end', () => {
          try {
            const response = JSON.parse(buffer);
            if (response.data) {
              this.lastMessageAt = nowMs();
              this.messageCount++;
              handler(response.data);
            }
          } catch (err) {
            log.debug(`CoreCast ${name} parse error: ${(err as Error).message}`);
          }
          this.pollers.set(name, setTimeout(doPoll, intervalMs));
        });
        res.on('error', (err) => {
          log.debug(`CoreCast ${name} error: ${err.message}`);
          this.pollers.set(name, setTimeout(doPoll, intervalMs * 2));
        });
      });

      req.on('error', (err) => {
        log.debug(`CoreCast ${name} request error: ${err.message}`);
        this.pollers.set(name, setTimeout(doPoll, intervalMs * 2));
      });

      req.write(body);
      req.end();
    };

    log.info(`CoreCast stream ${name} connected`);
    doPoll();
  }

  // ====== DATA HANDLERS ======

  private handleTradeData(trade: any): void {
    try {
      const mintAddress = trade.Trade?.Buy?.Currency?.MintAddress || trade.Trade?.Sell?.Currency?.MintAddress;
      if (!mintAddress) return;

      const event: TokenTradeEvent = {
        mint: mintAddress,
        signature: trade.Transaction?.Signature || '',
        timestamp: new Date(trade.Block?.Time || Date.now()).getTime(),
        txType: trade.Trade?.Buy?.Amount ? 'buy' : 'sell',
        solAmount: parseFloat(trade.Trade?.Buy?.Amount || trade.Trade?.Sell?.Amount || '0'),
        tokenAmount: 0,
        traderPublicKey: trade.Transaction?.Signer || trade.Trade?.Buy?.Account?.Address || '',
        newTokenBalance: 0,
        bondingCurveKey: '',
        vTokensInBondingCurve: 0,
        vSolInBondingCurve: 0,
        marketCapSol: 0,
      };

      this.emit('tokenTrade', event);
    } catch (err) {
      log.debug(`handleTradeData error: ${(err as Error).message}`);
    }
  }

  private handleNewTokenData(instr: any): void {
    try {
      const accounts = instr.Instruction?.Accounts || [];
      const mintAccount = accounts.find((a: any) => !a.IsWritable);
      if (!mintAccount) return;

      const event: NewTokenEvent = {
        mint: mintAccount.Address,
        signature: instr.Transaction?.Signature || '',
        name: '',
        symbol: '',
        uri: '',
        timestamp: new Date(instr.Block?.Time || Date.now()).getTime(),
        initialBuy: 0,
        bondingCurveKey: '',
        traderPublicKey: instr.Transaction?.Signer || '',
        txType: 'create',
        vTokensInBondingCurve: 0,
        vSolInBondingCurve: 0,
        marketCapSol: 0,
      };

      this.emit('newToken', event);
    } catch (err) {
      log.debug(`handleNewTokenData error: ${(err as Error).message}`);
    }
  }

  private handleMigrationData(instr: any): void {
    try {
      const event: MigrationEvent = {
        mint: instr.Instruction?.Accounts?.[0]?.Address || '',
        signature: instr.Transaction?.Signature || '',
        timestamp: new Date(instr.Block?.Time || Date.now()).getTime(),
      };

      this.emit('migration', event);
    } catch (err) {
      log.debug(`handleMigrationData error: ${(err as Error).message}`);
    }
  }
}
