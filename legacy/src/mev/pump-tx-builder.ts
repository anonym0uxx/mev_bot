/**
 * @module mev/pump-tx-builder
 * PumpTxBuilder: constructs real Pump.fun buy/sell VersionedTransactions
 * using the on-chain program structure and correct IDL discriminators.
 *
 * Program ID: 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
 *
 * Key design notes:
 * - bondingCurveKey and associatedBondingCurve are taken directly from TokenTradeEvent
 *   (already parsed from the feed) — no PDA derivation needed
 * - ATA existence checked on-chain; create instruction prepended if missing
 * - All txs are VersionedTransaction (v0) — compatible with Jito bundles
 */

import {
  Connection,
  Keypair,
  PublicKey,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
  SystemProgram,
  ComputeBudgetProgram,
} from '@solana/web3.js';
import {
  getAssociatedTokenAddress,
  createAssociatedTokenAccountInstruction,
  TOKEN_PROGRAM_ID,
  ASSOCIATED_TOKEN_PROGRAM_ID,
} from '@solana/spl-token';
import { createLogger } from '../utils/logger';

const log = createLogger('mev:pump-tx-builder');

const PUMP_PROGRAM_ID   = new PublicKey('6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P');
const PUMP_FEE_RECIPIENT = new PublicKey('CebN5WGQ4jvEPvsVU4EoHEpgznyQHeP5R5wMA7iiMVJP');
const GLOBAL_STATE      = new PublicKey('4wTV81ej3eDXFRv9dFGc3bJBFNHqEMWCeUhFpEsLWEMZ');
const SYSTEM_PROGRAM    = SystemProgram.programId;
const TOKEN_PROGRAM     = TOKEN_PROGRAM_ID;
const RENT_PROGRAM      = new PublicKey('SysvarRent111111111111111111111111111111111');
const EVENT_AUTHORITY   = new PublicKey('Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1');

// Pump.fun discriminators: first 8 bytes of sha256("global:buy") and sha256("global:sell")
const BUY_DISCRIMINATOR  = Buffer.from([102, 6, 61, 18, 1, 218, 235, 234]);
const SELL_DISCRIMINATOR = Buffer.from([51, 230, 133, 164, 1, 127, 131, 173]);

export interface PumpBuyParams {
  mint: PublicKey;
  bondingCurve: PublicKey;
  associatedBondingCurve: PublicKey;
  buyer: Keypair;
  solAmountLamports: bigint;
  minTokensOut: bigint;
  priorityFeeMicroLamports: number;
  recentBlockhash: string;
}

export interface PumpSellParams {
  mint: PublicKey;
  bondingCurve: PublicKey;
  associatedBondingCurve: PublicKey;
  seller: Keypair;
  tokenAmount: bigint;
  minSolOut: bigint;
  priorityFeeMicroLamports: number;
  recentBlockhash: string;
}

export class PumpTxBuilder {
  private connection: Connection;

  constructor(connection: Connection) {
    this.connection = connection;
  }

  private encodeBuyData(solAmount: bigint, minTokens: bigint): Buffer {
    const buf = Buffer.alloc(8 + 8 + 8);
    BUY_DISCRIMINATOR.copy(buf, 0);
    // amount (u64 LE) — token amount we expect to receive (minTokens for slippage guard)
    buf.writeBigUInt64LE(minTokens, 8);
    // max_sol_cost (u64 LE) — maximum SOL we're willing to spend
    buf.writeBigUInt64LE(solAmount, 16);
    return buf;
  }

  private encodeSellData(tokenAmount: bigint, minSolOut: bigint): Buffer {
    const buf = Buffer.alloc(8 + 8 + 8);
    SELL_DISCRIMINATOR.copy(buf, 0);
    // amount (u64 LE)
    buf.writeBigUInt64LE(tokenAmount, 8);
    // min_sol_output (u64 LE)
    buf.writeBigUInt64LE(minSolOut, 16);
    return buf;
  }

  async buildBuyTx(params: PumpBuyParams): Promise<VersionedTransaction> {
    const {
      mint, bondingCurve, associatedBondingCurve, buyer,
      solAmountLamports, minTokensOut, priorityFeeMicroLamports, recentBlockhash,
    } = params;

    // Derive buyer's ATA for this mint
    const buyerAta = await getAssociatedTokenAddress(mint, buyer.publicKey);

    // Check if ATA exists on-chain — prepend create instruction if not
    const ataInfo = await this.connection.getAccountInfo(buyerAta).catch(() => null);
    const preInstructions: TransactionInstruction[] = [];
    if (!ataInfo) {
      log.debug(`ATA missing for buyer ${buyer.publicKey.toBase58().slice(0, 8)}, prepending create`);
      preInstructions.push(
        createAssociatedTokenAccountInstruction(buyer.publicKey, buyerAta, buyer.publicKey, mint)
      );
    }

    const buyIx = new TransactionInstruction({
      programId: PUMP_PROGRAM_ID,
      keys: [
        { pubkey: GLOBAL_STATE,            isSigner: false, isWritable: false },
        { pubkey: PUMP_FEE_RECIPIENT,       isSigner: false, isWritable: true  },
        { pubkey: mint,                     isSigner: false, isWritable: false },
        { pubkey: bondingCurve,             isSigner: false, isWritable: true  },
        { pubkey: associatedBondingCurve,   isSigner: false, isWritable: true  },
        { pubkey: buyerAta,                 isSigner: false, isWritable: true  },
        { pubkey: buyer.publicKey,          isSigner: true,  isWritable: true  },
        { pubkey: SYSTEM_PROGRAM,           isSigner: false, isWritable: false },
        { pubkey: TOKEN_PROGRAM,            isSigner: false, isWritable: false },
        { pubkey: RENT_PROGRAM,             isSigner: false, isWritable: false },
        { pubkey: EVENT_AUTHORITY,          isSigner: false, isWritable: false },
        { pubkey: PUMP_PROGRAM_ID,          isSigner: false, isWritable: false },
      ],
      data: this.encodeBuyData(solAmountLamports, minTokensOut),
    });

    const instructions = [
      ComputeBudgetProgram.setComputeUnitPrice({ microLamports: priorityFeeMicroLamports }),
      ComputeBudgetProgram.setComputeUnitLimit({ units: 200_000 }),
      ...preInstructions,
      buyIx,
    ];

    const msg = new TransactionMessage({
      payerKey: buyer.publicKey,
      recentBlockhash,
      instructions,
    }).compileToV0Message();

    const tx = new VersionedTransaction(msg);
    tx.sign([buyer]);
    log.debug(
      `Built buy tx: mint=${mint.toBase58().slice(0, 8)} ` +
      `sol=${Number(solAmountLamports) / 1e9} minTokens=${minTokensOut}`
    );
    return tx;
  }

  async buildSellTx(params: PumpSellParams): Promise<VersionedTransaction> {
    const {
      mint, bondingCurve, associatedBondingCurve, seller,
      tokenAmount, minSolOut, priorityFeeMicroLamports, recentBlockhash,
    } = params;

    const sellerAta = await getAssociatedTokenAddress(mint, seller.publicKey);

    const sellIx = new TransactionInstruction({
      programId: PUMP_PROGRAM_ID,
      keys: [
        { pubkey: GLOBAL_STATE,                isSigner: false, isWritable: false },
        { pubkey: PUMP_FEE_RECIPIENT,           isSigner: false, isWritable: true  },
        { pubkey: mint,                         isSigner: false, isWritable: false },
        { pubkey: bondingCurve,                 isSigner: false, isWritable: true  },
        { pubkey: associatedBondingCurve,       isSigner: false, isWritable: true  },
        { pubkey: sellerAta,                    isSigner: false, isWritable: true  },
        { pubkey: seller.publicKey,             isSigner: true,  isWritable: true  },
        { pubkey: SYSTEM_PROGRAM,               isSigner: false, isWritable: false },
        { pubkey: ASSOCIATED_TOKEN_PROGRAM_ID,  isSigner: false, isWritable: false },
        { pubkey: TOKEN_PROGRAM,                isSigner: false, isWritable: false },
        { pubkey: EVENT_AUTHORITY,              isSigner: false, isWritable: false },
        { pubkey: PUMP_PROGRAM_ID,              isSigner: false, isWritable: false },
      ],
      data: this.encodeSellData(tokenAmount, minSolOut),
    });

    const instructions = [
      ComputeBudgetProgram.setComputeUnitPrice({ microLamports: priorityFeeMicroLamports }),
      ComputeBudgetProgram.setComputeUnitLimit({ units: 150_000 }),
      sellIx,
    ];

    const msg = new TransactionMessage({
      payerKey: seller.publicKey,
      recentBlockhash,
      instructions,
    }).compileToV0Message();

    const tx = new VersionedTransaction(msg);
    tx.sign([seller]);
    log.debug(
      `Built sell tx: mint=${mint.toBase58().slice(0, 8)} ` +
      `tokens=${tokenAmount} minSol=${Number(minSolOut) / 1e9}`
    );
    return tx;
  }
}
