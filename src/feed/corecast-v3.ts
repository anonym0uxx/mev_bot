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
import * as fs from 'fs';
import * as grpc from '@grpc/grpc-js';
import * as protoLoader from '@grpc/proto-loader';
import * as path from 'path';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { CoreCastConfig } from '../types/config';
import { NewTokenEvent, TokenTradeEvent, MigrationEvent } from '../types/events';
import { PoolUpdate } from '../mev/pool-depth-cache';
import { EventJoiner } from './event-joiner';
import { CreatorCache } from './creator-cache';

const log = createLogger('corecast-v3');

/** Emitted by Stream 5 (whale_trades) when a known whale wallet buys a Pump.fun token */
export interface WhaleTradeEvent {
  mint: string;
  traderAddress: string;
  solAmount: number;
  txType: string;
  timestamp: number;
}

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
  /** Tracks streams currently in reconnect-pending state — prevents error+end double-firing */
  private reconnectPending: Set<string> = new Set();
  private shouldRun = false;
  private isShuttingDown = false; // Set on disconnect() — blocks new reconnect timers
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
  get activeStreamCount(): number { return this.streams.size; }
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

    // Guard against double-connect — already connected streams would create duplicates on Bitquery side
    if (this._connected || this.streams.size > 0) {
      log.warn(`CoreCast v3: connect() called while already connected (${this.streams.size} streams active) — ignoring duplicate call`);
      return;
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

    // Stream 4 (optional): DexPools — Raydium LP monitor for pool depth filtering.
    // 3 core streams required; streams 4-5 are optional enhancements.
    const subscribePoolStream = this.config.subscribe_pool_stream !== false; // default true
    if (subscribePoolStream) {
      if (typeof (this.client as any).DexPools === 'function') {
        streams.push({
          name: 'dex_pools',
          method: 'DexPools',
          request: {
            program: { addresses: [PUMP_FUN_AMM] },
          },
          handler: (msg) => this.handlePoolMessage(msg),
        });
      } else {
        log.warn('[corecast] DexPools stream not available in current Bitquery plan — pool depth filtering disabled');
      }
    }

    // Stream 5 (optional): whale_trades — DexTrades filtered to known alpha wallet addresses.
    // Only activated when: subscribe_whale_stream !== false AND whale list has >= 5 addresses.
    // Emits 'whaleTrade' events for fast MEV pre-qualification via signal bridge.
    const subscribeWhaleStream = (this.config as any).subscribe_whale_stream !== false; // default true
    if (subscribeWhaleStream) {
      const whaleAddresses = this.loadWhaleAddresses();
      if (whaleAddresses.length >= 5) {
        streams.push({
          name: 'whale_trades',
          method: 'DexTrades',
          request: {
            program: { addresses: [PUMP_FUN_BONDING] },
            trader: { addresses: whaleAddresses },
          },
          handler: (msg) => this.handleWhaleTradeMessage(msg),
        });
        log.info(`[corecast] Stream 5 (whale_trades) active — tracking ${whaleAddresses.length} whale wallets`);
      } else {
        log.warn(`[corecast] Stream 5 (whale_trades) skipped — whale list has ${whaleAddresses.length} addresses (need >= 5). Run scripts/build-whale-list.js to populate.`);
      }
    } else {
      log.info('[corecast] Stream 5 (whale_trades) disabled via config (subscribe_whale_stream=false)');
    }

    // Validate core streams: require at least 3 (bonding trades + transactions + AMM trades).
    // 3 core streams required; streams 4-5 are optional enhancements.
    const CORE_STREAMS_REQUIRED = 3;
    const MAX_STREAMS = 5; // Hard cap — Bitquery plan limit. Never exceed this.
    const coreStreams = streams.filter(s => s.name !== 'dex_pools' && s.name !== 'whale_trades');
    if (coreStreams.length < CORE_STREAMS_REQUIRED) {
      // Hard fail — Bitquery concurrent stream limit is 3. Misconfiguration means we'd be
      // trading blind or wasting quota. Refuse to start rather than silently under-subscribe.
      throw new Error(
        `CoreCast v3: expected at least ${CORE_STREAMS_REQUIRED} core streams but configured ${coreStreams.length}. ` +
        `Check subscribe_trades/subscribe_new_tokens/subscribe_migrations in config.`
      );
    }
    if (streams.length > MAX_STREAMS) {
      // Hard cap: never open more than 5 streams regardless of config.
      // Truncate to the first MAX_STREAMS to protect Bitquery quota.
      log.error(`CoreCast v3: ${streams.length} streams configured but hard cap is ${MAX_STREAMS} — truncating to first ${MAX_STREAMS}`);
      streams.length = MAX_STREAMS;
    }

    // Start all streams
    for (const stream of streams) {
      this.reconnectAttempts.set(stream.name, 0);
      this.startStream(stream);
    }

    this._connected = true;
    const optionalCount = streams.length - coreStreams.length;
    log.info(`CoreCast v3 connected — ${streams.length} streams active (${coreStreams.length} core + ${optionalCount} optional)`);
    this.emit('connected');
  }

  disconnect(): void {
    this.shouldRun = false;
    this.isShuttingDown = true; // Block any new reconnect timers from firing

    // Cancel all pending reconnect timers — prevents orphaned reconnects after shutdown
    for (const [name, timer] of this.reconnectTimers) {
      clearTimeout(timer);
      log.debug(`Reconnect timer cleared: ${name}`);
    }
    this.reconnectTimers.clear();
    this.reconnectPending.clear();

    // Cancel all active gRPC streams — sends RST_STREAM to server (clean closure)
    for (const [name, stream] of this.streams) {
      try {
        stream.cancel();
        log.debug(`Stream cancelled: ${name}`);
      } catch {}
    }
    this.streams.clear();

    // Close the underlying gRPC channel to release all server-side resources.
    // Without this, the TCP connection lingers until server keepalive times out.
    if (this.client) {
      try {
        // grpc-js: use client.close() directly (documented public API on grpc.Client).
        // getChannel().close() is NOT a public API — it's implementation-internal and
        // silently no-ops on some stub types (proxied clients, interceptors).
        if (typeof (this.client as any).close === 'function') {
          (this.client as any).close();
          log.debug('gRPC channel closed');
        }
      } catch {}
      this.client = null;
    }

    this.joiner.destroy();
    this._connected = false;
    log.info(`CoreCast v3 disconnected cleanly (${this.streams.size} streams, all timers cleared)`);
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

  // Generation counters — incremented each time a stream is replaced.
  // Closures capture their generation; stale closures from replaced streams are ignored.
  private streamGenerations: Map<string, number> = new Map();

  private startStream(config: StreamConfig): void {
    if (!this.shouldRun || !this.client) return;

    // Bump generation for this stream name. Any closures from the previous
    // stream instance will see a stale generation and skip reconnect/error handling.
    const myGeneration = (this.streamGenerations.get(config.name) ?? 0) + 1;
    this.streamGenerations.set(config.name, myGeneration);

    // Cancel the existing stream (if any) AFTER bumping generation so its
    // error/end closures see the stale generation and bail out immediately.
    const existing = this.streams.get(config.name);
    if (existing) {
      this.streams.delete(config.name);
      try { existing.cancel(); } catch (_) {}
    }

    const metadata = new grpc.Metadata();
    metadata.add('authorization', this.apiKey);

    try {
      const stream: grpc.ClientReadableStream<any> = this.client[config.method](
        config.request,
        metadata
      );

      stream.on('data', (msg: any) => {
        if (this.streamGenerations.get(config.name) !== myGeneration) return;
        this.lastMessageAt = nowMs();
        this.messageCount++;
        try {
          config.handler(msg);
        } catch (err) {
          log.warn(`Handler error in ${config.name}: ${(err as Error).message}`);
        }
      });

      stream.on('error', (err: grpc.ServiceError) => {
        // Stale generation = this stream was intentionally replaced; ignore.
        if (this.streamGenerations.get(config.name) !== myGeneration) return;
        log.warn(`Stream error [${config.name}]: code=${err.code} ${err.message}`);
        this.emit('error', err);
        if (err.code === grpc.status.UNAVAILABLE || err.code === grpc.status.INTERNAL) {
          this.scheduleReconnect(config);
        }
      });

      stream.on('end', () => {
        // Stale generation = this stream was intentionally replaced; ignore.
        if (this.streamGenerations.get(config.name) !== myGeneration) return;
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
    if (!this.shouldRun || this.isShuttingDown) return; // Don't reconnect during shutdown

    // DEDUPLICATE: both 'error' and 'end' events fire for the same stream drop in rapid succession.
    // Use reconnectPending set as the authoritative lock — first caller wins, rest are ignored.
    if (this.reconnectPending.has(config.name)) {
      log.debug(`Stream ${config.name}: reconnect already pending — skipping duplicate (error+end race)`);
      return;
    }
    this.reconnectPending.add(config.name);

    const attempts = this.reconnectAttempts.get(config.name) || 0;
    const maxAttempts = 10;

    if (attempts >= maxAttempts) {
      this.reconnectPending.delete(config.name);
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
      this.reconnectPending.delete(config.name); // Release lock before startStream (allows future reconnects)
      if (!this.isShuttingDown) this.startStream(config);
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

  // ====== WHALE WALLET LOADER ======

  /** Load whale wallet addresses from data/whale-wallets.json for Stream 5 filter */
  private loadWhaleAddresses(): string[] {
    const whalePath = path.join(process.cwd(), 'data', 'whale-wallets.json');
    try {
      if (!fs.existsSync(whalePath)) {
        log.warn('[corecast] whale-wallets.json not found — run scripts/build-whale-list.js');
        return [];
      }
      const data = JSON.parse(fs.readFileSync(whalePath, 'utf8'));
      const wallets: Array<{ address: string }> = data.wallets || [];
      return wallets.map(w => w.address).filter(Boolean);
    } catch (e) {
      log.error(`[corecast] Failed to load whale list: ${(e as Error).message}`);
      return [];
    }
  }

  // ====== WHALE TRADES HANDLER (Stream 5) ======

  /** Handle whale_trades stream message → WhaleTradeEvent */
  private handleWhaleTradeMessage(msg: any): void {
    const trade = msg?.Trade;
    if (!trade) return;

    // Deduplicate by tx signature (shared dedup set)
    const sig = msg?.Transaction?.Signature;
    const sigStr = sig ? (Buffer.isBuffer(sig) ? sig.toString('hex') : String(sig)) : '';
    if (sigStr && this.isDupe(sigStr)) return;
    if (sigStr) this.addDedupe(sigStr);

    const mint = trade.Market?.QuoteCurrency?.MintAddress;
    const mintStr = mint
      ? (typeof mint === 'string' ? mint : decodeAddress(mint))
      : '';
    if (!mintStr) return;

    // Determine buy vs sell
    const buyAmount = trade.Buy?.Amount || 0;
    const sellAmount = trade.Sell?.Amount || 0;
    const isBuy = buyAmount > 0 && buyAmount > sellAmount;

    const traderBuf = isBuy
      ? trade.Buy?.Account?.Address
      : trade.Sell?.Account?.Address;
    const traderAddress = traderBuf ? decodeAddress(traderBuf) : '';

    const solAmount = isBuy
      ? Number(trade.Sell?.Amount || 0) / 1e9
      : Number(trade.Buy?.Amount || 0) / 1e9;

    const txType = isBuy ? 'buy' : 'sell';

    const event: WhaleTradeEvent = {
      mint: mintStr,
      traderAddress,
      solAmount,
      txType,
      timestamp: nowMs(),
    };

    log.debug(`[whale] ${txType.toUpperCase()} ${mintStr.slice(0,8)} by ${traderAddress.slice(0,8)} (${solAmount.toFixed(4)} SOL)`);
    this.emit('whaleTrade', event);
  }

  // ====== POOL DEPTH HANDLER (Stream 4) ======

  /** Handle DexPools stream message → PoolUpdate event */
  private handlePoolMessage(msg: any): void {
    const poolEvent = msg?.PoolEvent;
    if (!poolEvent) return;

    const market = poolEvent.Market;
    if (!market) return;

    // Extract pool address
    const poolAddrBuf = market.MarketAddress;
    const poolAddress = poolAddrBuf ? decodeAddress(poolAddrBuf) : '';

    // QuoteCurrency holds the token mint (BaseCurrency = SOL for AMM pools)
    const quoteMintBuf = market.QuoteCurrency?.MintAddress;
    const mint = quoteMintBuf
      ? (typeof quoteMintBuf === 'string' ? quoteMintBuf : decodeAddress(quoteMintBuf))
      : '';

    if (!mint) return;

    // SOL depth: PostAmount on the BaseCurrency (SOL) side — in lamports, convert to SOL
    const basePost = Number(poolEvent.BaseCurrency?.PostAmount ?? 0);
    const baseChange = Number(poolEvent.BaseCurrency?.ChangeAmount ?? 0);

    // PostAmount is in raw lamports (9 decimals for SOL)
    const depthSol = basePost / 1e9;
    const changeSol = baseChange / 1e9;

    // LP removal: base (SOL) change is negative (SOL leaving the pool)
    const isRemoval = changeSol < 0;

    const update: PoolUpdate = {
      mint,
      poolAddress,
      depthSol,
      changeSol,
      isRemoval,
      timestamp: nowMs(),
    };

    log.debug(
      `[pool] ${mint.slice(0, 8)} depth=${depthSol.toFixed(2)} SOL change=${changeSol >= 0 ? '+' : ''}${changeSol.toFixed(2)} SOL${isRemoval ? ' ⚠️ LP REMOVAL' : ''}`
    );

    this.emit('poolUpdate', update);
  }
}
