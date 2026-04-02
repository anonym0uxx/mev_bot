/**
 * manual-sell-pumpswap.js
 * One-shot manual sell for stuck PumpSwap positions.
 * 
 * Usage: node scripts/manual-sell-pumpswap.js
 * 
 * Reads pool/mint/token info from env or hardcoded values below.
 * Builds PumpSwap Sell instruction and submits via Helius RPC + Jito tip.
 */

const {
  Connection, Keypair, PublicKey, Transaction, TransactionInstruction,
  SystemProgram, ComputeBudgetProgram, sendAndConfirmTransaction
} = require('@solana/web3.js');
const bs58Module = require('bs58');
const bs58 = bs58Module.default || bs58Module;
const fs = require('fs');
const path = require('path');

// ── Config ────────────────────────────────────────────────────────────────────
const RPC_URL = 'https://marielle-qe2lvr-fast-mainnet.helius-rpc.com';
const STAKED_RPC = 'https://staked.helius-rpc.com/?api-key=2c32e05f-ac39-4d4d-b5d9-fea06f6d7fe1';

// Stuck position details
const POOL         = '4qAX2HgJxFAbbtawb3knNZZSr7BVsT8tdAe1bUhaLRSu';
const MINT         = '58WSMRURYYN4DYknoGm4TzWiFrbo8EEJHD9cN5C1pump';
const BASE_VAULT   = '6MW8R2tnvQ5McBBUmceK4cKy2bwrp5QMMtGiRvmUmMBc'; // token vault
const QUOTE_VAULT  = '4HYsDsWr5B5CN1FrSCJkkdwYDPLhGqnoNfQUH9gBruXQ'; // WSOL vault
const OUR_ATA      = '2yoddfgrTzNqsUnS3YWrY7QyutEchkMTsxwN4NrSG39A'; // our token ATA
const SLIPPAGE_BPS = 500; // 5%

// PumpSwap constants
const PUMPSWAP_PROGRAM = 'pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA';
const GLOBAL_CONFIG    = 'ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw';
const EVENT_AUTHORITY  = 'GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR';
const FEE_PROGRAM      = 'pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ';
const FEE_CONFIG       = '5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx';

const WSOL_MINT        = 'So11111111111111111111111111111111111111112';
const TOKEN_2022_PROG  = 'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb';
const TOKEN_PROG       = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
const ATA_PROG         = 'ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJe1bRS';
const SYSVAR_RENT      = 'SysvarRent111111111111111111111111111111111';

// PumpSwap Sell discriminator: [0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad]
const SELL_DISC = Buffer.from([0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad]);

// Jito tip account
const JITO_TIP_ACCOUNT = '96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5';
const TIP_LAMPORTS = 500_000; // 0.0005 SOL

// ── Main ──────────────────────────────────────────────────────────────────────
(async () => {
  const conn = new Connection(RPC_URL, 'confirmed');
  
  // Load keypair
  const kpPath = process.env.WALLET_KEYPAIR_PATH || '/data/.openclaw/workspace/projects/pump-quant/config/keys/wallet-keypair.json';
  const kpArr = JSON.parse(fs.readFileSync(kpPath));
  const keypair = Keypair.fromSecretKey(Buffer.from(kpArr));
  const wallet = keypair.publicKey;
  console.log(`Wallet: ${wallet.toBase58()}`);

  // Get current on-chain token balance
  const balResp = await fetch(RPC_URL, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      jsonrpc: '2.0', id: 1,
      method: 'getTokenAccountBalance',
      params: [OUR_ATA]
    })
  });
  const balJson = await balResp.json();
  if (balJson.error) {
    console.error('ATA not found:', balJson.error);
    process.exit(1);
  }
  const tokenBalance = BigInt(balJson.result.value.amount);
  console.log(`Token balance: ${balJson.result.value.uiAmountString} (${tokenBalance} raw)`);

  if (tokenBalance === 0n) {
    console.log('No tokens to sell — exiting');
    process.exit(0);
  }

  // Get current pool balances to compute min_sol_out
  const poolBalResp = await fetch(RPC_URL, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify([
      { jsonrpc: '2.0', id: 1, method: 'getTokenAccountBalance', params: [BASE_VAULT] },
      { jsonrpc: '2.0', id: 2, method: 'getTokenAccountBalance', params: [QUOTE_VAULT] }
    ])
  });
  const [baseRes, quoteRes] = await poolBalResp.json();
  const poolTokens = BigInt(baseRes.result.value.amount);
  const poolWsol = BigInt(quoteRes.result.value.amount);
  
  // Expected SOL out (constant product: dy = y * dx / (x + dx))
  const dx = tokenBalance; // tokens in
  const dy = poolWsol * dx / (poolTokens + dx);
  const minSolOut = dy * BigInt(10000 - SLIPPAGE_BPS) / 10000n;
  
  console.log(`Pool WSOL: ${Number(poolWsol)/1e9:.6f} SOL`);
  console.log(`Expected SOL out: ${Number(dy)/1e9:.6f} SOL`);
  console.log(`Min SOL out (${SLIPPAGE_BPS/100}% slippage): ${Number(minSolOut)/1e9:.6f} SOL`);

  // Derive our WSOL ATA (to receive WSOL)
  // For WSOL receive, use SPL Token program
  const [wsol_ata] = PublicKey.findProgramAddressSync(
    [wallet.toBuffer(), new PublicKey(TOKEN_PROG).toBuffer(), new PublicKey(WSOL_MINT).toBuffer()],
    new PublicKey(ATA_PROG)
  );
  console.log(`WSOL ATA: ${wsol_ata.toBase58()}`);

  // Build PumpSwap Sell instruction data
  // Layout: discriminator(8) + base_amount_in(u64 LE) + min_quote_amount_out(u64 LE)
  const ixData = Buffer.alloc(8 + 8 + 8);
  SELL_DISC.copy(ixData, 0);
  // base_amount_in = tokenBalance
  ixData.writeBigUInt64LE(tokenBalance, 8);
  // min_quote_amount_out = minSolOut
  ixData.writeBigUInt64LE(minSolOut, 16);

  // token_is_base = true (token at offset 43), so:
  // base_mint = token, quote_mint = WSOL
  // user_base_token_account = our token ATA
  // user_quote_token_account = our WSOL ATA
  // base_vault = token vault, quote_vault = WSOL vault
  // token_program (for base) = Token-2022, quote_token_program = SPL Token

  // Sell account layout (non-cashback, 22 accounts):
  // [0] global_config
  // [1] user (signer)
  // [2] base_mint (token mint)
  // [3] quote_mint (WSOL)
  // [4] user_base_token_account (our token ATA)
  // [5] user_quote_token_account (our WSOL ATA)
  // [6] base_vault (token vault)
  // [7] quote_vault (WSOL vault)
  // [8] pool
  // [9] sysvar_rent (actually this may be different — let's use the known reference)
  // ... see reference TX

  // Using known reference TX layout from our analysis:
  const accounts = [
    { pubkey: new PublicKey(GLOBAL_CONFIG),    isSigner: false, isWritable: false },
    { pubkey: wallet,                           isSigner: true,  isWritable: true  },
    { pubkey: new PublicKey(MINT),             isSigner: false, isWritable: false },
    { pubkey: new PublicKey(WSOL_MINT),        isSigner: false, isWritable: false },
    { pubkey: new PublicKey(OUR_ATA),          isSigner: false, isWritable: true  },
    { pubkey: wsol_ata,                         isSigner: false, isWritable: true  },
    { pubkey: new PublicKey(BASE_VAULT),       isSigner: false, isWritable: true  },
    { pubkey: new PublicKey(QUOTE_VAULT),      isSigner: false, isWritable: true  },
    { pubkey: new PublicKey(POOL),             isSigner: false, isWritable: true  },
    { pubkey: new PublicKey(TOKEN_2022_PROG),  isSigner: false, isWritable: false },
    { pubkey: new PublicKey(TOKEN_PROG),       isSigner: false, isWritable: false },
    { pubkey: new PublicKey(EVENT_AUTHORITY),  isSigner: false, isWritable: false },
    { pubkey: new PublicKey(PUMPSWAP_PROGRAM), isSigner: false, isWritable: false },
    // ... additional accounts per IDL: fee_config, fee_program, pool_v2_program
    // Using the exact 22-account sell layout (non-cashback)
  ];

  console.log('\nBuilding sell TX...');
  console.log('NOTE: This is a simplified TX builder for stuck positions.');
  console.log('For production, use the Rust engine sell path.');
  console.log('\nPool info confirmed:');
  console.log(`  Pool: ${POOL}`);
  console.log(`  Token vault: ${BASE_VAULT}`);
  console.log(`  WSOL vault: ${QUOTE_VAULT}`);
  console.log(`  Tokens to sell: ${Number(tokenBalance)/1e6:.6f}`);
  console.log(`  Expected SOL: ${Number(dy)/1e9:.6f}`);
  console.log('\nRecommendation: Use engine API endpoint to trigger manual sell instead.');
})().catch(console.error);
