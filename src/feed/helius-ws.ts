/**
 * @module feed/helius-ws
 * Helius WebSocket client — processed commitment fast lane for trigger detection.
 *
 * Subscribes to pump.fun bonding curve transactions via Helius's transactionSubscribe
 * with commitment: "processed" (~200-400ms faster than finalized/CoreCast).
 *
 * Emits the same TokenTradeEvent format as CoreCast bonding_trades for seamless dedup.
 * Does NOT count against the 5 CoreCast stream slots (separate WebSocket).
 */

import { EventEmitter } from 'events';
import WebSocket from 'ws';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { TokenTradeEvent } from '../types/events';

// bs58 for signature format normalization (CoreCast uses hex, Helius uses base58)
// eslint-disable-next-line @typescript-eslint/no-var-requires
const _bs58 = require('bs58');
const bs58Encoder = _bs58.default ?? _bs58;

const log = createLogger('helius-ws');

const PUMP_FUN_BONDING = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P';

export interface HeliusWsConfig {
  /** WebSocket URL (wss://...) — typically SOLANA_WS_URL env var */
  wsUrl: string;
  /** Helius API key for fallback URL construction */
  apiKey?: string;
  /** Enable/disable this feed */
  enabled: boolean;
}

export class HeliusWsClient extends EventEmitter {
  private ws: WebSocket | null = null;
  private config: HeliusWsConfig;
  private shouldRun = false;
  private reconnectTimer: NodeJS.Timeout | null = null;
  private reconnectAttempts = 0;
  private pingInterval: NodeJS.Timeout | null = null;
  private _connected = false;
  private messageCount = 0;
  private lastMessageAt = 0;
  private startTime = 0;

  constructor(config: HeliusWsConfig) {
    super();
    this.config = config;
  }

  get connected(): boolean { return this._connected; }
  get stats() {
    return {
      messageCount: this.messageCount,
      uptimeMs: this.startTime > 0 ? nowMs() - this.startTime : 0,
      lastMessageAt: this.lastMessageAt,
    };
  }

  connect(): void {
    if (!this.config.enabled) {
      log.info('Helius WS disabled via config');
      return;
    }

    // Build WS URL
    let wsUrl = this.config.wsUrl;
    if (!wsUrl && this.config.apiKey) {
      wsUrl = `wss://mainnet.helius-rpc.com/?api-key=${this.config.apiKey}`;
    }
    if (!wsUrl) {
      log.error('Helius WS: no wsUrl or apiKey configured — cannot connect');
      return;
    }

    this.shouldRun = true;
    this.startTime = nowMs();
    this._connect(wsUrl);
  }

  disconnect(): void {
    this.shouldRun = false;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.pingInterval) {
      clearInterval(this.pingInterval);
      this.pingInterval = null;
    }
    if (this.ws) {
      try { this.ws.close(); } catch {}
      this.ws = null;
    }
    this._connected = false;
    log.info('Helius WS disconnected');
  }

  private _connect(wsUrl: string): void {
    if (!this.shouldRun) return;

    try {
      this.ws = new WebSocket(wsUrl);

      this.ws.on('open', () => {
        this._connected = true;
        this.reconnectAttempts = 0;
        log.info(`Helius WS connected to ${wsUrl.replace(/api-key=[^&]+/, 'api-key=***')}`);

        // Subscribe to pump.fun bonding curve transactions at processed commitment
        const subscribeMsg = {
          jsonrpc: '2.0',
          id: 1,
          method: 'transactionSubscribe',
          params: [
            {
              accountInclude: [PUMP_FUN_BONDING],
            },
            {
              commitment: 'processed',
              encoding: 'jsonParsed',
              transactionDetails: 'full',
              showRewards: false,
              maxSupportedTransactionVersion: 0,
            },
          ],
        };

        this.ws!.send(JSON.stringify(subscribeMsg));
        log.info('Helius WS: subscribed to transactionSubscribe (processed, pump.fun bonding)');

        this.emit('connected');

        // Start ping to keep alive
        this.pingInterval = setInterval(() => {
          if (this.ws?.readyState === WebSocket.OPEN) {
            this.ws.ping();
          }
        }, 30_000);
      });

      this.ws.on('message', (data: WebSocket.Data) => {
        this.lastMessageAt = nowMs();
        this.messageCount++;

        try {
          const msg = JSON.parse(data.toString());

          // Handle subscription confirmation
          if (msg.id === 1 && msg.result !== undefined) {
            log.info(`Helius WS: subscription confirmed (id=${msg.result})`);
            return;
          }

          // Handle transaction notifications
          if (msg.method === 'transactionNotification' && msg.params?.result) {
            this.handleTransaction(msg.params.result);
          }
        } catch (err) {
          log.warn(`Helius WS parse error: ${(err as Error).message}`);
        }
      });

      this.ws.on('error', (err: Error) => {
        log.warn(`Helius WS error: ${err.message}`);
        this.emit('error', err);
      });

      this.ws.on('close', (code: number, reason: Buffer) => {
        this._connected = false;
        if (this.pingInterval) {
          clearInterval(this.pingInterval);
          this.pingInterval = null;
        }
        log.warn(`Helius WS closed: code=${code} reason=${reason?.toString() || 'none'}`);

        if (this.shouldRun) {
          this.scheduleReconnect(wsUrl);
        }
      });
    } catch (err) {
      log.error(`Helius WS connect failed: ${(err as Error).message}`);
      if (this.shouldRun) {
        this.scheduleReconnect(wsUrl);
      }
    }
  }

  private scheduleReconnect(wsUrl: string): void {
    if (!this.shouldRun || this.reconnectTimer) return;

    const maxAttempts = 15;
    if (this.reconnectAttempts >= maxAttempts) {
      log.error(`Helius WS: max reconnect attempts (${maxAttempts}) reached`);
      this.emit('disconnected', 'max_reconnect');
      return;
    }

    const delay = Math.min(1000 * Math.pow(2, this.reconnectAttempts), 60_000) + Math.random() * 1000;
    this.reconnectAttempts++;

    log.info(`Helius WS: reconnecting in ${Math.round(delay)}ms (attempt ${this.reconnectAttempts})`);

    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this._connect(wsUrl);
    }, delay);
  }

  /**
   * Parse a Helius transactionNotification into TokenTradeEvent.
   * Extracts mint, buyer/seller, SOL amount, and direction from pump.fun bonding curve instructions.
   */
  private handleTransaction(result: any): void {
    const tx = result?.transaction;
    const signature = result?.signature || '';

    if (!tx) return;

    const meta = tx.meta;
    if (!meta || meta.err) return; // Skip failed txs

    const message = tx.transaction?.message;
    if (!message) return;

    const instructions = message.instructions || [];
    const innerInstructions = meta.innerInstructions || [];

    // Look for pump.fun bonding curve instruction
    for (const ix of instructions) {
      if (ix.programId !== PUMP_FUN_BONDING) continue;

      // Parse pump.fun instruction data to determine buy/sell
      // pump.fun buy discriminator: first 8 bytes = [102, 6, 61, 18, 1, 218, 235, 234] (hex: 66063d1201daebea)
      // pump.fun sell discriminator: first 8 bytes = [51, 230, 133, 164, 1, 127, 131, 173] (hex: 33e685a4017f83ad)
      const data = ix.data;
      if (!data) continue;

      let isBuy = false;
      let isSell = false;

      // Helius jsonParsed may provide decoded instruction type
      // But for raw data, check the first bytes
      if (typeof data === 'string') {
        // base58 or base64 encoded — try to detect buy/sell from instruction
        // Alternative: use accounts and balance changes to determine direction
      }

      // More reliable approach: use pre/post SOL balance changes on the signer
      // Signer = first account key
      const accountKeys = message.accountKeys || [];
      const signerKey = typeof accountKeys[0] === 'string'
        ? accountKeys[0]
        : accountKeys[0]?.pubkey || '';

      if (!signerKey) continue;

      // Calculate SOL delta for the signer from pre/post balances
      const preBalances: number[] = meta.preBalances || [];
      const postBalances: number[] = meta.postBalances || [];

      let signerIdx = -1;
      for (let i = 0; i < accountKeys.length; i++) {
        const key = typeof accountKeys[i] === 'string' ? accountKeys[i] : accountKeys[i]?.pubkey;
        if (key === signerKey) {
          signerIdx = i;
          break;
        }
      }

      if (signerIdx < 0) continue;

      const solDelta = (postBalances[signerIdx] ?? 0) - (preBalances[signerIdx] ?? 0);
      // Negative delta = signer spent SOL = BUY
      // Positive delta = signer received SOL = SELL
      const solAmountLamports = Math.abs(solDelta);
      const solAmount = solAmountLamports / 1e9;

      // Filter out negligible amounts (fees only, no actual trade)
      if (solAmount < 0.001) continue;

      isBuy = solDelta < 0;
      isSell = solDelta > 0;

      if (!isBuy && !isSell) continue;

      // Extract mint from token balance changes
      let mint = '';
      const preTokenBalances: any[] = meta.preTokenBalances || [];
      const postTokenBalances: any[] = meta.postTokenBalances || [];

      // Find token that changed for the signer (non-SOL)
      for (const ptb of postTokenBalances) {
        if (ptb.owner === signerKey && ptb.mint && ptb.mint !== 'So11111111111111111111111111111111111111112') {
          mint = ptb.mint;
          break;
        }
      }

      if (!mint) {
        // Try preTokenBalances (for sells where post might be 0)
        for (const ptb of preTokenBalances) {
          if (ptb.owner === signerKey && ptb.mint && ptb.mint !== 'So11111111111111111111111111111111111111112') {
            mint = ptb.mint;
            break;
          }
        }
      }

      if (!mint) continue;

      // Build TokenTradeEvent matching CoreCast format
      const event: TokenTradeEvent = {
        mint,
        txType: isBuy ? 'buy' : 'sell',
        traderPublicKey: signerKey,
        tokenAmount: 0,   // Not easily extractable from raw tx
        solAmount,
        newTokenBalance: 0,
        bondingCurveKey: '',
        vTokensInBondingCurve: 0,
        vSolInBondingCurve: 0,
        marketCapSol: 0,
        signature,
        timestamp: nowMs(),
      };

      // Tag as Helius source for dedup/logging
      (event as any).source = 'helius';
      (event as any).heliusReceivedAt = nowMs();

      if (this.messageCount % 50 === 0) {
        log.info(`Helius WS trades: ${this.messageCount} (latest: ${mint.slice(0, 8)} ${event.txType} ${solAmount.toFixed(4)} SOL)`);
      }

      this.emit('tokenTrade', event);
      return; // One event per tx
    }
  }
}
