import * as fs from 'fs';
import * as path from 'path';
import * as crypto from 'crypto';
import { Keypair } from '@solana/web3.js';
import { createLogger } from '../utils/logger';

const log = createLogger('mev:wallet-store');

const STORE_PATH = path.join(process.cwd(), 'data', 'wallets.enc');
const ALGORITHM = 'aes-256-gcm';
const KEY_LEN = 32;
const SALT_LEN = 32;
const IV_LEN = 16;
const TAG_LEN = 16;

interface WalletEntry {
  label: string;
  publicKey: string;
  secretKeyBase64: string;
  addedAt: string;
}

interface WalletStore {
  version: 1;
  wallets: WalletEntry[];
}

/**
 * EncryptedWalletStore — AES-256-GCM encrypted wallet storage.
 *
 * The encryption key is derived from a password using PBKDF2 with a random salt.
 * The salt, IV, auth tag, and ciphertext are all stored together in the .enc file.
 *
 * File format (binary):
 *   [4 bytes: version=1] [32 bytes: salt] [16 bytes: IV] [16 bytes: GCM auth tag] [N bytes: ciphertext]
 *
 * Password comes from env var WALLET_STORE_PASSWORD. If not set, uses a machine-derived default
 * (hostname + a fixed pepper) — NOT secure for production but ensures the file is never plaintext.
 */
export class EncryptedWalletStore {
  private password: string;

  constructor(password?: string) {
    this.password = password || process.env.WALLET_STORE_PASSWORD || this.defaultPassword();
  }

  private defaultPassword(): string {
    const os = require('os');
    return `pump-quant-${os.hostname()}-2026-default-pepper-x9k2`;
  }

  private deriveKey(salt: Buffer): Buffer {
    return crypto.pbkdf2Sync(this.password, salt, 100_000, KEY_LEN, 'sha256');
  }

  private encrypt(plaintext: string, salt: Buffer): { iv: Buffer; tag: Buffer; ciphertext: Buffer } {
    const key = this.deriveKey(salt);
    const iv = crypto.randomBytes(IV_LEN);
    const cipher = crypto.createCipheriv(ALGORITHM, key, iv) as crypto.CipherGCM;
    const ciphertext = Buffer.concat([cipher.update(plaintext, 'utf8'), cipher.final()]);
    const tag = cipher.getAuthTag();
    return { iv, tag, ciphertext };
  }

  private decrypt(data: Buffer): string {
    // Parse: [4 version][32 salt][16 iv][16 tag][N ciphertext]
    let offset = 0;
    const version = data.readUInt32BE(offset); offset += 4;
    if (version !== 1) throw new Error(`Unknown store version: ${version}`);
    const salt = data.subarray(offset, offset + SALT_LEN); offset += SALT_LEN;
    const iv = data.subarray(offset, offset + IV_LEN); offset += IV_LEN;
    const tag = data.subarray(offset, offset + TAG_LEN); offset += TAG_LEN;
    const ciphertext = data.subarray(offset);
    const key = this.deriveKey(Buffer.from(salt));
    const decipher = crypto.createDecipheriv(ALGORITHM, key, iv) as crypto.DecipherGCM;
    decipher.setAuthTag(tag);
    return decipher.update(ciphertext) + decipher.final('utf8');
  }

  load(): WalletStore {
    if (!fs.existsSync(STORE_PATH)) return { version: 1, wallets: [] };
    try {
      const data = fs.readFileSync(STORE_PATH);
      const json = this.decrypt(data);
      return JSON.parse(json);
    } catch (e) {
      log.error(`Failed to decrypt wallet store: ${(e as Error).message}`);
      throw e;
    }
  }

  save(store: WalletStore): void {
    const salt = crypto.randomBytes(SALT_LEN);
    const { iv, tag, ciphertext } = this.encrypt(JSON.stringify(store), salt);
    // Write: [4 version][32 salt][16 iv][16 tag][N ciphertext]
    const versionBuf = Buffer.alloc(4); versionBuf.writeUInt32BE(1, 0);
    const out = Buffer.concat([versionBuf, salt, iv, tag, ciphertext]);
    fs.mkdirSync(path.dirname(STORE_PATH), { recursive: true });
    fs.writeFileSync(STORE_PATH, out, { mode: 0o600 }); // owner read/write only
    log.info(`Wallet store saved: ${store.wallets.length} wallet(s) at ${STORE_PATH}`);
  }

  /** Add a keypair to the store */
  addWallet(keypair: Keypair, label: string): void {
    const store = this.load();
    const existing = store.wallets.find(w => w.publicKey === keypair.publicKey.toBase58());
    if (existing) { log.info(`Wallet ${label} already in store`); return; }
    store.wallets.push({
      label,
      publicKey: keypair.publicKey.toBase58(),
      secretKeyBase64: Buffer.from(keypair.secretKey).toString('base64'),
      addedAt: new Date().toISOString(),
    });
    this.save(store);
    log.info(`Added wallet ${label} (${keypair.publicKey.toBase58().slice(0, 8)}...) to store`);
  }

  /** Load all keypairs from store */
  loadKeypairs(): Keypair[] {
    const store = this.load();
    return store.wallets.map(w => {
      const secret = Buffer.from(w.secretKeyBase64, 'base64');
      return Keypair.fromSecretKey(new Uint8Array(secret));
    });
  }

  /** List wallet public keys (safe to log) */
  listWallets(): Array<{ label: string; publicKey: string; addedAt: string }> {
    return this.load().wallets.map(({ label, publicKey, addedAt }) => ({ label, publicKey, addedAt }));
  }
}
