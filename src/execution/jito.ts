/**
 * @module execution/jito
 * Jito bundle submission via jito-ts v4.2.1 gRPC SDK.
 *
 * Bundles land up to 5 VersionedTransactions atomically in the same block.
 * The SDK's Bundle.addTipTx() appends the Jito tip transfer internally.
 *
 * Key constraints:
 * - Bundle accepts VersionedTransaction only (not legacy Transaction)
 * - addTransactions() and addTipTx() return Bundle | Error — always check isError()
 * - sendBundle() returns Result<string, SearcherClientError> — check .ok
 * - Never throw on Jito failure — caller always falls back to PumpPortal
 *
 * gRPC endpoint: mainnet.block-engine.jito.wtf:443
 */

import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionMessage,
  VersionedTransaction,
  LAMPORTS_PER_SOL,
} from '@solana/web3.js';
import { searcherClient, SearcherClient } from 'jito-ts/dist/sdk/block-engine/searcher';
import { Bundle } from 'jito-ts/dist/sdk/block-engine/types';
import { isError } from 'jito-ts/dist/sdk/block-engine/utils';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { PumpQuantConfig } from '../types/config';

const log = createLogger('jito');

// ─── Constants ────────────────────────────────────────────────────────────────

const JITO_BLOCK_ENGINE_URL = 'mainnet.block-engine.jito.wtf:443';

/** Max transactions per bundle (Jito hard limit, including the tip tx). */
const BUNDLE_TRANSACTION_LIMIT = 5;

/** Jito-controlled tip accounts. One is selected randomly per bundle. */
const JITO_TIP_ACCOUNTS: readonly string[] = [
  '96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5',
  'HFqU5x63VTqvB6pQMT9zDQGGPgNDZ5Jznh5GbMggbUfU',
  'Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY',
  'ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1uw1nbn2uK6',
  'DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh',
  'ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt',
  'DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL',
  '3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT',
] as const;

/** Tip guard rails (lamports). */
const TIP_MIN_LAMPORTS     =  1_000;   // 0.000001 SOL
const TIP_DEFAULT_LAMPORTS = 10_000;   // 0.00001  SOL
const TIP_MAX_LAMPORTS     = 100_000;  // 0.0001   SOL

// ─── Types ────────────────────────────────────────────────────────────────────

export interface JitoBundleResult {
  bundleId: string;
  status: 'submitted' | 'failed';
  landingLatencyMs?: number;
  tipLamports?: number;
  errorMessage?: string;
}

// ─── Singleton gRPC client ────────────────────────────────────────────────────

/**
 * Lazily-initialised gRPC searcher client.
 * The gRPC channel is persistent (HTTP/2 keep-alive) — we create it once and
 * reuse it for all bundle submissions within a process lifetime.
 *
 * No auth keypair is required for mainnet public block-engine access.
 */
let _client: SearcherClient | null = null;

function getClient(): SearcherClient {
  if (!_client) {
    _client = searcherClient(JITO_BLOCK_ENGINE_URL, undefined, {
      // Keep-alive settings for long-lived gRPC connection
      'grpc.keepalive_time_ms': 10_000,
      'grpc.keepalive_timeout_ms': 5_000,
      'grpc.keepalive_permit_without_calls': 1,
    });
    log.info(`Jito gRPC client created: ${JITO_BLOCK_ENGINE_URL}`);
  }
  return _client;
}

/**
 * Reset the gRPC client. Useful after unrecoverable transport errors.
 * The next call to getClient() will create a fresh connection.
 */
export function resetJitoClient(): void {
  _client = null;
  log.info('Jito gRPC client reset');
}

// ─── Config helpers ───────────────────────────────────────────────────────────

/**
 * Whether Jito bundle submission is enabled.
 * Requires BOTH execution.bundle_route.enabled AND execution.jito_enabled === true.
 * Double-gated so infrastructure (bundle_route) and operations (jito_enabled) can
 * be toggled independently.
 */
export function isJitoEnabled(config: PumpQuantConfig): boolean {
  if (!config.execution?.bundle_route?.enabled) return false;
  // Explicit opt-in required — absence of the flag means disabled
  return config.execution.jito_enabled === true;
}

/**
 * Tip to pay in lamports, clamped to [TIP_MIN, TIP_MAX].
 * Prefers execution.jito_tip_lamports; falls back to private_route.jito_tip_lamports.
 */
export function getJitoTipLamports(config: PumpQuantConfig): number {
  let raw: number | undefined;

  if (typeof config.execution?.jito_tip_lamports === 'number') {
    raw = config.execution.jito_tip_lamports;
  } else if (typeof config.execution?.private_route?.jito_tip_lamports === 'number') {
    raw = config.execution.private_route.jito_tip_lamports;
  }

  if (raw == null || isNaN(raw)) raw = TIP_DEFAULT_LAMPORTS;

  const clamped = Math.max(TIP_MIN_LAMPORTS, Math.min(TIP_MAX_LAMPORTS, Math.floor(raw)));
  if (clamped !== raw) {
    log.warn(`Jito tip clamped: requested=${raw} → clamped=${clamped} lamports`);
  }
  return clamped;
}

// ─── Tip account selection ────────────────────────────────────────────────────

/**
 * Select a random Jito tip account.
 * Uniform distribution — avoids routing all tips through one account.
 */
export function selectTipAccount(): PublicKey {
  const idx = Math.floor(Math.random() * JITO_TIP_ACCOUNTS.length);
  return new PublicKey(JITO_TIP_ACCOUNTS[idx]);
}

// ─── Legacy → Versioned Transaction conversion ────────────────────────────────

/**
 * Convert a signed legacy Transaction to a signed VersionedTransaction.
 *
 * jito-ts Bundle only accepts VersionedTransaction. Our existing
 * buildBuyTransaction / buildSellTransaction return legacy Transaction
 * (web3.js v1 style). This conversion:
 *   1. Extracts all instructions from the legacy tx
 *   2. Wraps them in a v0 TransactionMessage
 *   3. Packages as VersionedTransaction and re-signs
 *
 * IMPORTANT: recentBlockhash must already be set on the legacy tx before
 * calling this function.
 *
 * @param legacyTx  Signed or unsigned legacy Transaction with recentBlockhash set.
 * @param wallet    Keypair to sign the new VersionedTransaction.
 * @returns         Signed VersionedTransaction (v0).
 */
export function toVersionedTransaction(
  legacyTx: Transaction,
  wallet: Keypair,
): VersionedTransaction {
  if (!legacyTx.recentBlockhash) {
    throw new Error('toVersionedTransaction: legacy tx must have recentBlockhash set');
  }

  const messageV0 = new TransactionMessage({
    payerKey: wallet.publicKey,
    recentBlockhash: legacyTx.recentBlockhash,
    instructions: legacyTx.instructions,
  }).compileToV0Message();

  const vtx = new VersionedTransaction(messageV0);
  vtx.sign([wallet]);
  return vtx;
}

// ─── Bundle submission ────────────────────────────────────────────────────────

/**
 * Submit a Jito bundle via gRPC.
 *
 * Flow:
 *   1. Fetch fresh blockhash (shared across all txs for slot validity alignment)
 *   2. Convert legacy trade tx(s) → VersionedTransaction
 *   3. Build Bundle with trade txs
 *   4. Append tip tx via Bundle.addTipTx() (SDK handles tip tx construction + signing)
 *   5. Submit via SearcherClient.sendBundle()
 *   6. Return JitoBundleResult — never throw (caller falls back to PumpPortal)
 *
 * @param legacyTxs  Unsigned legacy Transaction(s) from buildBuy/SellTransaction.
 *                   Must NOT have recentBlockhash set yet — this function sets it.
 *                   Max 4 txs (tip tx occupies the 5th slot).
 * @param connection Solana connection for blockhash fetch.
 * @param wallet     Wallet to sign all transactions.
 * @param config     Bot config for tip amount.
 */
export async function submitBundle(
  legacyTxs: Transaction[],
  connection: Connection,
  wallet: Keypair,
  config: PumpQuantConfig,
): Promise<JitoBundleResult> {
  const startMs = nowMs();

  if (legacyTxs.length === 0) {
    return { bundleId: '', status: 'failed', errorMessage: 'No transactions provided' };
  }

  // Reserve one slot for the tip tx added by addTipTx()
  const maxTradeTxs = BUNDLE_TRANSACTION_LIMIT - 1;
  if (legacyTxs.length > maxTradeTxs) {
    return {
      bundleId: '',
      status: 'failed',
      errorMessage: `Too many txs: max ${maxTradeTxs} trade txs (tip tx takes the last slot)`,
    };
  }

  const tipLamports = getJitoTipLamports(config);
  const tipAccount  = selectTipAccount();

  try {
    // 1. Single blockhash shared by all txs — ensures they all expire together
    const { blockhash } = await connection.getLatestBlockhash('confirmed');

    // 2. Convert legacy → VersionedTransaction and sign
    const versionedTxs: VersionedTransaction[] = legacyTxs.map(legacyTx => {
      legacyTx.recentBlockhash = blockhash;
      return toVersionedTransaction(legacyTx, wallet);
    });

    // 3. Build bundle with trade txs
    const bundle = new Bundle([], BUNDLE_TRANSACTION_LIMIT);

    const maybeWithTrades = bundle.addTransactions(...versionedTxs);
    if (isError(maybeWithTrades)) {
      const msg = (maybeWithTrades as Error).message;
      log.warn(`Jito Bundle.addTransactions failed: ${msg}`);
      return { bundleId: '', status: 'failed', errorMessage: `addTransactions: ${msg}` };
    }

    // 4. Append tip tx (SDK builds + signs the SystemProgram.transfer internally)
    const maybeWithTip = maybeWithTrades.addTipTx(wallet, tipLamports, tipAccount, blockhash);
    if (isError(maybeWithTip)) {
      const msg = (maybeWithTip as Error).message;
      log.warn(`Jito Bundle.addTipTx failed: ${msg}`);
      return { bundleId: '', status: 'failed', errorMessage: `addTipTx: ${msg}` };
    }

    log.info(
      `Submitting Jito bundle: txs=${versionedTxs.length + 1} ` +
      `tip=${tipLamports}L (${(tipLamports / LAMPORTS_PER_SOL).toFixed(6)} SOL) ` +
      `tipAccount=${tipAccount.toBase58().slice(0, 8)}…`
    );

    // 5. Submit via gRPC
    const client = getClient();
    const result = await client.sendBundle(maybeWithTip);
    const landingLatencyMs = nowMs() - startMs;

    if (!result.ok) {
      const err = result.error;
      log.warn(`Jito sendBundle RPC error: code=${err.code} msg=${err.message} details=${err.details}`);

      // Reset client on transport-level errors so next call gets a fresh connection
      if (err.code === 14 /* UNAVAILABLE */ || err.code === 13 /* INTERNAL */) {
        resetJitoClient();
      }

      return {
        bundleId: '',
        status: 'failed',
        landingLatencyMs,
        tipLamports,
        errorMessage: `gRPC ${err.code}: ${err.message}`,
      };
    }

    const bundleId = result.value;
    log.info(
      `Jito bundle submitted ✓ id=${bundleId} latency=${landingLatencyMs}ms ` +
      `tip=${tipLamports}L (${(tipLamports / LAMPORTS_PER_SOL).toFixed(6)} SOL)`
    );

    return {
      bundleId,
      status: 'submitted',
      landingLatencyMs,
      tipLamports,
    };

  } catch (err) {
    const landingLatencyMs = nowMs() - startMs;
    const msg = (err as Error).message ?? String(err);
    log.warn(`Jito bundle exception: ${msg}`);
    // Reset client on unexpected errors in case the gRPC channel is broken
    resetJitoClient();
    return {
      bundleId: '',
      status: 'failed',
      landingLatencyMs,
      errorMessage: msg,
    };
  }
}
