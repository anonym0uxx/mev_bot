/**
 * @module feed/whale-tracker
 * WhaleTracker: loads a list of known alpha wallets and emits 'whaleBuy' events
 * when any of those wallets buys a Pump.fun token.
 *
 * Used by daemon/index.ts to log whale activity.
 * Whale list populated by: scripts/build-whale-list.js (run periodically to refresh).
 */
import * as fs from 'fs';
import * as path from 'path';
import { EventEmitter } from 'events';
import { createLogger } from '../utils/logger';

const log = createLogger('mev:whale-tracker');

const WHALE_LIST_PATH = path.join(process.cwd(), 'data', 'whale-wallets.json');

export interface WhaleBuyEvent {
  mint: string;
  traderAddress: string;
  solAmount: number;
  timestamp: number;
}

export class WhaleTracker extends EventEmitter {
  private whaleAddresses = new Set<string>();
  private loadedCount = 0;

  constructor() {
    super();
    this.loadWhaleList();
  }

  private loadWhaleList(): void {
    try {
      if (!fs.existsSync(WHALE_LIST_PATH)) {
        log.warn('Whale list not found — whale pre-qualification disabled. Run scripts/build-whale-list.js to bootstrap.');
        return;
      }
      const data = JSON.parse(fs.readFileSync(WHALE_LIST_PATH, 'utf8'));
      const wallets: Array<{ address: string }> = data.wallets || [];
      this.whaleAddresses = new Set(wallets.map(w => w.address));
      this.loadedCount = this.whaleAddresses.size;
      log.info(`Whale tracker loaded ${this.loadedCount} wallets (updated: ${data.updatedAt || 'unknown'})`);
    } catch (e) {
      log.error(`Failed to load whale list: ${(e as Error).message}`);
    }
  }

  isWhale(address: string): boolean {
    return this.whaleAddresses.has(address);
  }

  getCount(): number {
    return this.loadedCount;
  }

  /** Call when a trade event comes in — emits 'whaleBuy' if trader is a whale */
  checkTrade(mint: string, traderAddress: string, solAmount: number, txType: string): void {
    if (txType !== 'buy' && txType !== 'Buy') return;
    if (!this.isWhale(traderAddress)) return;
    const event: WhaleBuyEvent = { mint, traderAddress, solAmount, timestamp: Date.now() };
    log.debug(`Whale buy detected: ${traderAddress.slice(0,8)}... bought ${mint.slice(0,8)}... (${solAmount.toFixed(4)} SOL)`);
    this.emit('whaleBuy', event);
  }

  /** Reload the whale list from disk (call after running build-whale-list.js) */
  reload(): void {
    this.loadWhaleList();
  }
}
