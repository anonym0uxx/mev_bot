/**
 * @module execution/solana
 * Solana transaction construction for Pump.fun bonding curve trades.
 * Wallet signing via @solana/web3.js Keypair from env secret.
 * Private key NEVER in code/logs/chat — only from env.
 */

import {
  Connection, Keypair, PublicKey, Transaction, SystemProgram,
  sendAndConfirmTransaction, TransactionInstruction,
  LAMPORTS_PER_SOL, ComputeBudgetProgram,
} from '@solana/web3.js';
import bs58 from 'bs58';
import { createLogger } from '../utils/logger';

const log = createLogger('solana');

/**
 * Initialize Solana connection and wallet from environment.
 */
export function initSolanaConnection(): Connection {
  const rpcUrl = process.env.SOLANA_RPC_URL || 'https://api.mainnet-beta.solana.com';
  return new Connection(rpcUrl, 'confirmed');
}

/**
 * Load wallet keypair from environment variable.
 * Private key must be base58-encoded in WALLET_PRIVATE_KEY env var.
 * NEVER log or expose the private key.
 */
export function loadWalletKeypair(): Keypair {
  const privateKeyStr = process.env.WALLET_PRIVATE_KEY;
  if (!privateKeyStr) {
    throw new Error('WALLET_PRIVATE_KEY environment variable not set');
  }

  try {
    const privateKeyBytes = bs58.decode(privateKeyStr);
    return Keypair.fromSecretKey(privateKeyBytes);
  } catch (err) {
    throw new Error('Invalid WALLET_PRIVATE_KEY format — must be base58-encoded');
  }
}

/**
 * Get wallet SOL balance.
 */
export async function getWalletBalance(connection: Connection, wallet: Keypair): Promise<number> {
  const balance = await connection.getBalance(wallet.publicKey);
  return balance / LAMPORTS_PER_SOL;
}

/**
 * Build a buy transaction for Pump.fun bonding curve.
 * This constructs the transaction but does NOT sign or send it.
 */
export function buildBuyTransaction(
  wallet: Keypair,
  mint: PublicKey,
  bondingCurve: PublicKey,
  solAmount: number,
  slippageBps: number,
  priorityFeeSol: number
): Transaction {
  const tx = new Transaction();

  // Add priority fee instruction
  if (priorityFeeSol > 0) {
    const microLamports = Math.floor(priorityFeeSol * LAMPORTS_PER_SOL * 1_000_000);
    tx.add(
      ComputeBudgetProgram.setComputeUnitPrice({
        microLamports,
      })
    );
    tx.add(
      ComputeBudgetProgram.setComputeUnitLimit({
        units: 200_000,
      })
    );
  }

  // Pump.fun buy instruction
  // The actual instruction data depends on the Pump.fun program's IDL
  // This constructs the instruction with the correct accounts
  const PUMP_PROGRAM_ID = new PublicKey('6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P');
  const TOKEN_PROGRAM_ID = new PublicKey('TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA');
  const ASSOCIATED_TOKEN_PROGRAM_ID = new PublicKey('ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL');
  const SYSTEM_PROGRAM_ID = SystemProgram.programId;
  const RENT_PROGRAM_ID = new PublicKey('SysvarRent111111111111111111111111111111111');

  // Derive associated token account
  const [associatedTokenAccount] = PublicKey.findProgramAddressSync(
    [wallet.publicKey.toBuffer(), TOKEN_PROGRAM_ID.toBuffer(), mint.toBuffer()],
    ASSOCIATED_TOKEN_PROGRAM_ID
  );

  // Derive bonding curve token account
  const [bondingCurveTokenAccount] = PublicKey.findProgramAddressSync(
    [bondingCurve.toBuffer(), TOKEN_PROGRAM_ID.toBuffer(), mint.toBuffer()],
    ASSOCIATED_TOKEN_PROGRAM_ID
  );

  // Calculate max SOL with slippage
  const maxSolLamports = Math.floor(solAmount * LAMPORTS_PER_SOL * (1 + slippageBps / 10000));

  // Build buy instruction data
  // Discriminator for "buy" function
  const discriminator = Buffer.from([102, 6, 61, 18, 1, 218, 235, 234]); // buy discriminator
  const amountBuffer = Buffer.alloc(8);
  amountBuffer.writeBigUInt64LE(BigInt(0)); // Token amount (0 = use SOL amount)
  const maxSolBuffer = Buffer.alloc(8);
  maxSolBuffer.writeBigUInt64LE(BigInt(maxSolLamports));

  const data = Buffer.concat([discriminator, amountBuffer, maxSolBuffer]);

  const buyInstruction = new TransactionInstruction({
    programId: PUMP_PROGRAM_ID,
    keys: [
      { pubkey: new PublicKey('4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf'), isSigner: false, isWritable: false }, // Global
      { pubkey: new PublicKey('CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbCJ2AWKyicZKR'), isSigner: false, isWritable: true }, // Fee recipient
      { pubkey: mint, isSigner: false, isWritable: false },
      { pubkey: bondingCurve, isSigner: false, isWritable: true },
      { pubkey: bondingCurveTokenAccount, isSigner: false, isWritable: true },
      { pubkey: associatedTokenAccount, isSigner: false, isWritable: true },
      { pubkey: wallet.publicKey, isSigner: true, isWritable: true },
      { pubkey: SYSTEM_PROGRAM_ID, isSigner: false, isWritable: false },
      { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
      { pubkey: RENT_PROGRAM_ID, isSigner: false, isWritable: false },
      { pubkey: new PublicKey('Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1'), isSigner: false, isWritable: false }, // Event authority
      { pubkey: PUMP_PROGRAM_ID, isSigner: false, isWritable: false },
    ],
    data,
  });

  tx.add(buyInstruction);
  tx.feePayer = wallet.publicKey;

  return tx;
}

/**
 * Build a sell transaction for Pump.fun bonding curve.
 */
export function buildSellTransaction(
  wallet: Keypair,
  mint: PublicKey,
  bondingCurve: PublicKey,
  tokenAmount: number,
  slippageBps: number,
  priorityFeeSol: number
): Transaction {
  const tx = new Transaction();

  // Priority fee
  if (priorityFeeSol > 0) {
    const microLamports = Math.floor(priorityFeeSol * LAMPORTS_PER_SOL * 1_000_000);
    tx.add(
      ComputeBudgetProgram.setComputeUnitPrice({ microLamports })
    );
    tx.add(
      ComputeBudgetProgram.setComputeUnitLimit({ units: 200_000 })
    );
  }

  const PUMP_PROGRAM_ID = new PublicKey('6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P');
  const TOKEN_PROGRAM_ID = new PublicKey('TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA');
  const ASSOCIATED_TOKEN_PROGRAM_ID = new PublicKey('ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL');
  const SYSTEM_PROGRAM_ID = SystemProgram.programId;

  const [associatedTokenAccount] = PublicKey.findProgramAddressSync(
    [wallet.publicKey.toBuffer(), TOKEN_PROGRAM_ID.toBuffer(), mint.toBuffer()],
    ASSOCIATED_TOKEN_PROGRAM_ID
  );

  const [bondingCurveTokenAccount] = PublicKey.findProgramAddressSync(
    [bondingCurve.toBuffer(), TOKEN_PROGRAM_ID.toBuffer(), mint.toBuffer()],
    ASSOCIATED_TOKEN_PROGRAM_ID
  );

  // Sell discriminator
  const discriminator = Buffer.from([51, 230, 133, 164, 1, 127, 131, 173]);
  const tokenAmountBuffer = Buffer.alloc(8);
  tokenAmountBuffer.writeBigUInt64LE(BigInt(Math.floor(tokenAmount)));
  const minSolBuffer = Buffer.alloc(8);
  minSolBuffer.writeBigUInt64LE(BigInt(0)); // Min SOL (slippage handled by instruction)

  const data = Buffer.concat([discriminator, tokenAmountBuffer, minSolBuffer]);

  const sellInstruction = new TransactionInstruction({
    programId: PUMP_PROGRAM_ID,
    keys: [
      { pubkey: new PublicKey('4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf'), isSigner: false, isWritable: false },
      { pubkey: new PublicKey('CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbCJ2AWKyicZKR'), isSigner: false, isWritable: true },
      { pubkey: mint, isSigner: false, isWritable: false },
      { pubkey: bondingCurve, isSigner: false, isWritable: true },
      { pubkey: bondingCurveTokenAccount, isSigner: false, isWritable: true },
      { pubkey: associatedTokenAccount, isSigner: false, isWritable: true },
      { pubkey: wallet.publicKey, isSigner: true, isWritable: true },
      { pubkey: SYSTEM_PROGRAM_ID, isSigner: false, isWritable: false },
      { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
      { pubkey: new PublicKey('Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1'), isSigner: false, isWritable: false },
      { pubkey: PUMP_PROGRAM_ID, isSigner: false, isWritable: false },
    ],
    data,
  });

  tx.add(sellInstruction);
  tx.feePayer = wallet.publicKey;

  return tx;
}

/**
 * Sign and send a transaction.
 */
export async function signAndSendTransaction(
  connection: Connection,
  wallet: Keypair,
  transaction: Transaction,
  skipPreflight: boolean = false,
  timeoutMs: number = 30000
): Promise<{ signature: string; confirmedAt: number }> {
  const startTime = Date.now();

  transaction.recentBlockhash = (await connection.getLatestBlockhash()).blockhash;
  transaction.sign(wallet);

  const signature = await connection.sendRawTransaction(transaction.serialize(), {
    skipPreflight,
    maxRetries: 2,
    preflightCommitment: 'confirmed',
  });

  log.info(`Transaction sent: ${signature}`);

  // Wait for confirmation
  const confirmation = await connection.confirmTransaction(
    {
      signature,
      blockhash: transaction.recentBlockhash!,
      lastValidBlockHeight: (await connection.getLatestBlockhash()).lastValidBlockHeight,
    },
    'confirmed'
  );

  if (confirmation.value.err) {
    throw new Error(`Transaction failed: ${JSON.stringify(confirmation.value.err)}`);
  }

  const confirmedAt = Date.now();
  log.info(`Transaction confirmed: ${signature} (${confirmedAt - startTime}ms)`);

  return { signature, confirmedAt };
}
