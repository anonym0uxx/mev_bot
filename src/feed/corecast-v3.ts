/**
 * @module feed/corecast-v3
 * Bitquery CoreCast gRPC client — sub-100ms real-time Pump.fun data.
 *
 * Streams:
 *   1. dex_trades  — Pump.fun bonding curve trades (primary)
 *   2. transactions — Pool creation events + creator address
 *   3. dex_trades  — Pump.fun AMM post-graduation trades
 *
 * Replaces corecast-v2.ts (HTTP polling, 1-2s latency, ~86k calls/day).
 */

import { EventEmitter } from 'events';
import * as grpc from '@grpc/grpc-js';
import * as protoLoader from '@grpc/proto-loader';
import * as path from 'path';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { CoreCastConfig } from '../types/config';
import { NewTokenEvent, TokenTradeEvent, MigrationEvent } from '../types/events';
import { EventJoiner } from './event-joiner';
import { CreatorCache } from './creator-cache';

const log = createLogger('corecast-v3');

const PUMP_FUN_BONDING = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P';
const PUMP_FUN_AMM     = 'pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA';
const CORECAST_ENDPOINT = 'corecast.bitquery.io';

const GRPC_OPTIONS: grpc.ChannelOptions = {
  'grpc.keepalive_time_ms': 30_000,
  'grpc.keepalive_timeout_ms': 5_000,
  'grpc.keepalive_permit_without_calls': 1,
  'grpc.max_receive_message_length': 4 * 1024 * 1024,
  'grpc.max_send_message_length': 4 * 1024 * 1024,
  'grpc.enable_retries': 1,
  'grpc.max_connection_idle_ms': 30_000,
};

// bs58 exports { default: encoder } in ESM-compat mode
// eslint-disable-next-line @typescript-eslint/no-var-requires
const _bs58 = require('bs58');
const _bs58enc = _bs58.default ?? _bs58;

/** Helper to decode bytes → base58 address */
function decodeAddress(buf: Buffer | Uint8Array | null | undefined): string {
  if (!buf || buf.length === 0) return '';
  try {
    return _bs58enc.encode(buf instanceof Buffer ? buf : Buffer.from(buf));
  } catch {
    return Buffer.from(buf).toString('hex');
  }
}

interface StreamConfig {
  name: string;
  method: string;
  request: object;
  handler: (msg: any) => void;
}

export class CoreCastV3Client extends EventEmitter {
  private client: any = null;
  private streams: Map<string, grpc.ClientReadableStream<any>> = new Map();
  private reconnectTimers: Map<string, NodeJS.Timeout> = new Map();
  private reconnectAttempts: Map<string, number> = new Map();
  private shouldRun = false;
  private _connected = false;
  private messageCount = 0;
  private startTime = 0;
  private lastMessageAt = 0;

  // Dedup: track seen signatures (bounded at 100k)
  private dedupSet: Set<string> = new Set();
  private dedupOrder: string[] = [];
  private readonly DEDUP_MAX = 100_000;

  // Sub-components
  private joiner: EventJoiner;
  private creatorCache: CreatorCache;

  private apiKey: string;

  constructor(private config: CoreCastConfig) {
    super();
    this.apiKey = process.env[config.api_key_env] || process.env.BITQUERY_API_KEY || '';
    this.joiner = new EventJoiner();
    this.creatorCache = new CreatorCache(config.api_key_env);
  }

  get connected(): boolean { return this._connected; }
  get lastMessageTime(): number { return this.lastMessageAt; }
  get stats() {
    return {
      messageCount: this.messageCount,
      uptimeMs: this.startTime > 0 ? nowMs() - this.startTime : 0,
      lastMessageAt: this.lastMessageAt,
    };
  }

  async connect(): Promise<void> {
    if (!this.apiKey) {
      throw new Error(`CoreCast v3: API key not found in ${this.config.api_key_env}`);
    }

    this.shouldRun = true;
    this.startTime = nowMs();

    // Build gRPC client from proto package definition
    // Load protos manually (bitquery-corecast-proto has a path resolution bug with includeDirs)
    const protoBase = path.dirname(require.resolve('bitquery-corecast-proto'));
    const solanaPath = path.join(protoBase, 'solana');
    const protoFiles = [
      'corecast/corecast.proto',
      'corecast/request.proto',
      'corecast/stream_message.proto',
      'dex_block_message.proto',
      'block_message.proto',
      'token_block_message.proto',
      'parsed_idl_block_message.proto',
    ].map(f => path.join(solanaPath, f));

    const packageDef = protoLoader.loadSync(protoFiles, {
      keepCase: true,
      longs: String,
      enums: String,
      defaults: true,
      oneofs: true,
      includeDirs: [protoBase, solanaPath],
      bytes: Buffer,
      arrays: true,
      objects: true,
    });
    const grpcObj = grpc.loadPackageDefinition(packageDef) as any;
    const CoreCast = grpcObj?.solana_corecast?.CoreCast;
    if (!CoreCast) {
      throw new Error('CoreCast service not found in proto package');
    }

    const endpoint = (this.config as any).grpc_endpoint || CORECAST_ENDPOINT;
    this.client = new CoreCast(
      endpoint,
      grpc.credentials.createSsl(),
      GRPC_OPTIONS
    );

    log.info(`CoreCast v3 connecting to ${endpoint}`);

    // Define our 3 streams
    const streams: StreamConfig[] = [];

    if (this.config.subscribe_trades) {
      streams.push({
        name: 'bonding_trades',
        method: 'DexTrades',
        request: {
          program: { addresses: [PUMP_FUN_BONDING] },
        },
        handler: (msg) => this.handleTradeMessage(msg, false),
      });
    }

    if (this.config.subscribe_new_tokens) {
      streams.push({
        name: 'transactions',
        method: 'Transactions',
        request: {
          program: { addresses: [PUMP_FUN_BONDING] },
        },
        handler: (msg) => this.handleTransactionMessage(msg),
      });
    }

    if (this.config.subscribe_migrations) {
      streams.push({
        name: 'amm_trades',
        method: 'DexTrades',
        request: {
          program: { addresses: [PUMP_FUN_AMM] },
        },
        handler: (msg) => this.handleTradeMessage(msg, true),
      });
    }

    // Start all streams
    for (const stream of streams) {
      this.reconnectAttempts.set(stream.name, 0);
      this.startStream(stream);
    }

    this._connected = true;
    log.info(`CoreCast v3 connected — ${streams.length} streams active`);
    this.emit('connected');
  }

  disconnect(): void {
    this.shouldRun = false;

    // Cancel all reconnect timers
    for (const timer of this.reconnectTimers.values()) {
      clearTimeout(timer);
    }
    this.reconnectTimers.clear();

    // Cancel all streams
    for (const [name, stream] of this.streams) {
      try {
        stream.cancel();
        log.debug(`Stream cancelled: ${name}`);
      } catch {}
    }
    this.streams.clear();

    this.joiner.destroy();
    this._connected = false;
    log.info('CoreCast v3 disconnected');
    this.emit('disconnected', 'operator');
  }

  watchMints(_mints: string[]): void { /* no-op for streaming */ }
  unwatchMints(_mints: string[]): void { /* no-op for streaming */ }

  // ====== DEDUPLICATION ======

  private isDupe(sig: string): boolean {
    return this.dedupSet.has(sig);
  }

  private addDedupe(sig: string): void {
    this.dedupSet.add(sig);
    this.dedupOrder.push(sig);
    if (this.dedupOrder.length > this.DEDUP_MAX) {
      const evicted = this.dedupOrder.shift();
      if (evicted) this.dedupSet.delete(evicted);
    }
  }

  // ====== STREAM MANAGEMENT ======

  private startStream(config: StreamConfig): void {
    if (!this.shouldRun || !this.client) return;

    const metadata = new grpc.Metadata();
    metadata.add('authorization', this.apiKey);

    try {
      const stream: grpc.ClientReadableStream<any> = this.client[config.method](
        config.request,
        metadata
      );

      stream.on('data', (msg: any) => {
        this.lastMessageAt = nowMs();
        this.messageCount++;
        try {
          config.handler(msg);
        } catch (err) {
          log.warn(`Handler error in ${config.name}: ${(err as Error).message}`);
        }
      });

      stream.on('error', (err: grpc.ServiceError) => {
        log.warn(`Stream error [${config.name}]: code=${err.code} ${err.message}`);
        this.emit('error', err);
        if (err.code === grpc.status.UNAVAILABLE || err.code === grpc.status.INTERNAL) {
          this.scheduleReconnect(config);
        }
      });

      stream.on('end', () => {
        log.warn(`Stream ended: ${config.name}`);
        if (this.shouldRun) {
          this.scheduleReconnect(config);
        }
      });

      this.streams.set(config.name, stream);
      this.reconnectAttempts.set(config.name, 0);
      log.info(`Stream started: ${config.name} (${config.method})`);

    } catch (err) {
      log.error(`Failed to start stream ${config.name}: ${(err as Error).message}`);
      this.scheduleReconnect(config);
    }
  }

  private scheduleReconnect(config: StreamConfig): void {
    if (!this.shouldRun) return;

    const attempts = this.reconnectAttempts.get(config.name) || 0;
    const maxAttempts = 10;

    if (attempts >= maxAttempts) {
      log.error(`Stream ${config.name}: max reconnect attempts (${maxAttempts}) reached`);
      this.emit('disconnected', `${config.name}: max reconnect attempts`);
      return;
    }

    // Exponential backoff with jitter
    const delay = Math.min(1000 * Math.pow(2, attempts), 60_000) + Math.random() * 1000;
    this.reconnectAttempts.set(config.name, attempts + 1);

    log.info(`Stream ${config.name}: reconnecting in ${Math.round(delay)}ms (attempt ${attempts + 1})`);

    const timer = setTimeout(() => {
      this.reconnectTimers.delete(config.name);
      this.startStream(config);
    }, delay);

    this.reconnectTimers.set(config.name, timer);
  }

  // ====== MESSAGE HANDLERS ======

  /** Handle dex_trades stream message → TokenTradeEvent */
  private handleTradeMessage(msg: any, isAmm: boolean): void {
    const trade = msg?.Trade;
    if (!trade) return;

    // Deduplicate by tx signature
    const sig = msg?.Transaction?.Signature;
    const sigStr = sig ? (Buffer.isBuffer(sig) ? sig.toString('hex') : String(sig)) : '';
    if (sigStr && this.isDupe(sigStr)) return;
    if (sigStr) this.addDedupe(sigStr);

    const mint = trade.Market?.QuoteCurrency?.MintAddress;
    const mintStr = mint
      ? (typeof mint === 'string' ? mint : decodeAddress(mint))
      : '';
    if (!mintStr) return;

    // Determine buy vs sell: if Buy.Amount > 0, it's a buy
    const buyAmount = trade.Buy?.Amount || 0;
    const sellAmount = trade.Sell?.Amount || 0;
    const isBuy = buyAmount > 0 && buyAmount > sellAmount;

    const traderBuf = isBuy
      ? trade.Buy?.Account?.Address
      : trade.Sell?.Account?.Address;
    const trader = traderBuf ? decodeAddress(traderBuf) : '';

    const tokenAmount = isBuy ? Number(buyAmount) : Number(sellAmount);
    const solAmount = isBuy
      ? Number(trade.Sell?.Amount || 0) / 1e9
      : Number(trade.Buy?.Amount || 0) / 1e9;

    const event: TokenTradeEvent = {
      mint: mintStr,
      txType: isBuy ? 'buy' : 'sell',
      traderPublicKey: trader,
      tokenAmount,
      solAmount,
      newTokenBalance: 0,
      bondingCurveKey: '',
      vTokensInBondingCurve: 0,
      vSolInBondingCurve: 0,
      marketCapSol: 0,
      signature: sigStr,
      timestamp: nowMs(),
    };

    this.emit('tokenTrade', event);

    // Log periodic progress
    if (this.messageCount % 100 === 0) {
      log.info(`CoreCast v3 trades: ${this.messageCount} (latest: ${mintStr.slice(0, 8)} ${event.txType})`);
    }
  }

  /** Handle transactions stream message → NewTokenEvent (pool creation) */
  private handleTransactionMessage(msg: any): void {
    const tx = msg?.Transaction;
    if (!tx) return;

    const txSig = tx?.Signature;
    const sigStr = txSig ? (Buffer.isBuffer(txSig) ? txSig.toString('hex') : String(txSig)) : '';

    const instructions: any[] = tx?.ParsedIdlInstructions || [];
    if (instructions.length === 0) return;

    // Look for 'create' instruction from Pump.fun
    for (const instr of instructions) {
      const programAddr = instr?.Program?.Address;
      const programAddrStr = programAddr
        ? (typeof programAddr === 'string' ? programAddr : decodeAddress(programAddr))
        : '';
      const method = instr?.Program?.Method || '';

      if (!programAddrStr.includes(PUMP_FUN_BONDING.slice(0, 8)) && method !== 'create') continue;
      if (method !== 'create') continue;

      // Extract creator from transaction signer
      const signerBuf = tx?.Header?.Signer;
      const creator = signerBuf ? decodeAddress(signerBuf) : '';
      if (!creator) continue;

      // Parse arguments
      const args: any[] = instr?.Arguments || [];
      const getArg = (name: string): string =>
        args.find((a: any) => a.Name === name)?.Value || '';

      // Extract mint from accounts
      const accounts: any[] = instr?.Accounts || [];
      const mintAccount = accounts.find((a: any) => a.IsSigner === false && a.IsWritable === true);
      const mintBuf = mintAccount?.Address;
      const mint = mintBuf ? decodeAddress(mintBuf) : '';

      if (!mint) continue;

      const name = getArg('name');
      const symbol = getArg('symbol');
      const uri = getArg('uri');

      // Store creator in extended metadata (not in NewTokenEvent type directly)
      const event: NewTokenEvent = {
        signature: sigStr,
        mint,
        traderPublicKey: creator, // creator = first signer
        txType: 'create',
        name,
        symbol,
        uri,
        bondingCurveKey: '',
        vTokensInBondingCurve: 1_073_000_000,
        vSolInBondingCurve: 30_000_000_000,
        marketCapSol: 0,
        timestamp: nowMs(),
      };

      log.info(`New token: ${symbol || mint.slice(0, 8)} creator=${creator.slice(0, 8)}`);
      this.emit('newToken', event);

      // Async creator lookup (non-blocking, respects daily budget)
      if (this.creatorCache.shouldLookup(creator)) {
        this.creatorCache.fetchFromApi(creator).catch(() => {});
      }

      break; // Only process first 'create' instruction per tx
    }
  }
}
