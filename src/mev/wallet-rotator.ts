/**
 * @module mev/wallet-rotator
 * WalletRotator: round-robin keypair rotation for MEV anti-fingerprinting.
 *
 * Supports two key formats:
 *   - JSON uint8array string: "[1,2,3,...,64]"
 *   - Base58 string: decoded via bs58
 *
 * Wallets are loaded from config wallet_private_keys (which should be sourced
 * from env vars — never hardcoded in config files).
 *
 * In paper mode this class is not exercised; it's wired for live mode only.
 */

import { Keypair } from '@solana/web3.js';
import bs58 from 'bs58';
import { createLogger } from '../utils/logger';
import { EncryptedWalletStore } from './wallet-store';

const log = createLogger('mev:wallet-rotator');

export class WalletRotator {
  private wallets: Keypair[] = [];
  private index = 0;

  constructor(privateKeys: string[]) {
    // Each key is a JSON uint8array string or a base58-encoded private key
    for (const key of privateKeys) {
      try {
        let kp: Keypair;
        if (key.startsWith('[')) {
          // JSON uint8array format: "[1,2,3,...,64]"
          kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(key)));
        } else {
          // Base58 format
          kp = Keypair.fromSecretKey(bs58.decode(key));
        }
        this.wallets.push(kp);
        log.info(`Loaded wallet: ${kp.publicKey.toBase58().slice(0, 8)}...`);
      } catch (e) {
        log.warn(`Failed to load wallet key: ${(e as Error).message}`);
      }
    }

    if (this.wallets.length === 0) {
      // Fallback: load from EncryptedWalletStore
      try {
        const storeKeypairs = new EncryptedWalletStore().loadKeypairs();
        if (storeKeypairs.length > 0) {
          this.wallets = storeKeypairs;
          log.info(`WalletRotator loaded ${this.wallets.length} wallet(s) from encrypted store`);
        } else {
          log.warn('No wallets loaded — wallet rotator disabled (no env keys, no store wallets)');
        }
      } catch (e) {
        log.warn(`Failed to load wallets from encrypted store: ${(e as Error).message}`);
        log.warn('No wallets loaded — wallet rotator disabled');
      }
    } else {
      log.info(`WalletRotator initialized with ${this.wallets.length} wallet(s) from env`);
    }
  }

  /**
   * Get next wallet in round-robin order and advance the index.
   * Returns null if no wallets are loaded.
   */
  next(): Keypair | null {
    if (this.wallets.length === 0) return null;
    const w = this.wallets[this.index % this.wallets.length];
    this.index++;
    return w;
  }

  /**
   * Get current wallet without advancing the index.
   * Returns null if no wallets are loaded.
   */
  current(): Keypair | null {
    if (this.wallets.length === 0) return null;
    return this.wallets[this.index % this.wallets.length];
  }

  /** Number of wallets loaded. */
  get count(): number {
    return this.wallets.length;
  }
}
