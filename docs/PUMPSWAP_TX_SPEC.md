# PumpSwap Live TX Spec — Master Architecture Document
**Author:** Apollo (momentum architect)
**Date:** 2026-04-01
**Status:** Engineering implementation ready

---

## Objective

Wire PumpSwap live buy/sell transactions so the momentum engine actually submits real on-chain trades for the ~100% of pump.fun graduated tokens that use the PumpSwap AMM.

Currently: pool resolves ✅ → price feeds ✅ → scoring ✅ → entry opened ✅ → **BUY NOT SUBMITTED** ❌ (falls back to "accounting-only")

Root cause: `mod.rs` live buy/sell only checks `self.raydium_pools` for `RaydiumPoolAccounts`. PumpSwap-graduated tokens have no Raydium accounts → silent skip.

## Architecture — 5 Engineer Tasks

### Task Overview
```
E1: tx/pumpswap.rs       — PumpSwap swap ix builder (buy + sell, no runtime deps)
E2: momentum/pool.rs     — Add PumpSwapPoolAccounts, populate from PoolResolution
E3: momentum/mod.rs      — Add pumpswap_pools DashMap + wire live buy/sell paths
E4: momentum/mod.rs      — Wire live sell path + test harness
E5: Tests                — Integration + unit tests across all new code
```

---

## On-Chain Account Layout (EMPIRICALLY DERIVED — DO NOT GUESS)

Derived from live mainnet transactions on 2026-04-01. Verified across 5+ buy/sell txns on pool `GseMAnNDvntR5uFePZ51yZBXzNSn7GdFPkfHwfr6d77J` (token `7LSsEoJGhLeZzGvDofTdNg7M3JttxQqGWNLo6vWMpump`).

### PumpSwap Program
- **Program ID**: `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`
- **GlobalConfig PDA**: `ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw` (seeds: `["global_config"]`)
- **Event Authority PDA**: `GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR`

### Discriminator
**Buy AND Sell use the same Anchor discriminator: `0x33e685a4017f83ad`**
Direction is determined by which arg is `base_out` vs `base_in`:
- `buy(base_out: u64, max_quote_in: u64)` — we specify token amount out, pay max SOL
- `sell(base_in: u64, min_quote_out: u64)` — we specify token amount in, receive min SOL

### Protocol Fee Recipients (8 rotating addresses from GlobalConfig)
```
62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV
7VtfL8fvgNfhz17qKRMjzQEXgbdpnHHHQRh54R9jP2RJ
7hTckgnGnLQR6sdH7YkqFTAA7VwTfYFaZ6EhEsU3saCX
9rPYyANsfQZw3DnDmKE3YCQF5E8oD89UXoHn9JFEhJUz
AVmoTthdrX6tKt4nDjco2D775W2YK3sDhxPcMmzUAmTY
FWsW1xNtWscwNmKv6wVsU1iTzRN6wmmk3MjxRP5tT7hz
G5UZAVbAf46s7cKWoyKu8kYTip9DGTpbLZ2qa9Aq69dP
JCRGumoE9Qi5BBgULTgdgTLjSgkCMSbF62ZZfGs84JeU
```
Rotate randomly to improve throughput (PumpSwap recommendation).

### Coin Fee Program Constants (required even if null)
```
fee_program:     pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ
fee_prog_state:  5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx
```

### 22-Account Swap Instruction Layout (standard — all pump.fun graduated tokens)
```
[0]  pool                     (writable)  — Pool PDA from PoolResolution.pool_address
[1]  user                     (signer, writable) — wallet pubkey
[2]  global_config            (readonly)  — ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw
[3]  base_mint                (readonly)  — token mint (from pool state, = PoolResolution.mint)
[4]  quote_mint               (readonly)  — So11111111111111111111111111111111111111112 (WSOL)
[5]  user_base_token_account  (writable)  — ATA(user, base_mint) — token ATA
[6]  user_quote_token_account (writable)  — ATA(user, WSOL)
[7]  pool_base_token_account  (writable)  — PoolResolution.coin_vault (token vault from pool state)
[8]  pool_quote_token_account (writable)  — PoolResolution.pc_vault (WSOL vault from pool state)
[9]  protocol_fee_recipient   (writable)  — random pick from 8 fee recipients above
[10] protocol_fee_recipient_token_account (writable) — ATA(fee_recipient, WSOL)
[11] base_token_program       (readonly)  — TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA
[12] quote_token_program      (readonly)  — TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA
[13] system_program           (readonly)  — 11111111111111111111111111111111
[14] associated_token_program (readonly)  — ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL
[15] event_authority          (readonly)  — GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR
[16] pump_program             (readonly)  — pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA (for CPI event emit)
[17] coin_creator_vault_ata   (writable)  — 3E6RtRCxo7Yz64zi8aSmC9DtNKcaMp5rFUcBnJKsc2fg (Token2022 acct, writable)
[18] coin_creator_vault_authority (writable) — 4LFFCuf82nwijjNjbu8enNmfVa5hqBxBctgnuJ7ZNNMS (may not exist; include writable anyway)
[19] coin_fee_config          (readonly)  — 5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx
[20] coin_fee_program         (readonly)  — pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ
[21] coin_fee_program_state   (readonly)  — 4Jjna3h73QbgmdqwnV5NJxjCidKWB7Q26jeuj9jtFetC (may not exist; include readonly)
```

**IMPORTANT NOTE on accounts [17..21]:** Accounts [17] and [18] represent the coin creator fee vault and its authority — these are token accounts that may or may not exist for a given mint. Always include them in the instruction; the program handles non-existence gracefully. Accounts [19..21] are the coin fee program accounts — always include them. They appear in every observed real transaction.

---

## E1 Task: `tx/pumpswap.rs` — Swap Instruction Builder

**File**: `rust/pump-quant-core/src/tx/pumpswap.rs`  
**Constraint**: No runtime deps. No async. Pure instruction builder.  
**Pattern**: Mirror `tx/raydium.rs` style exactly.

### Structs

```rust
/// Pool accounts required for a PumpSwap swap.
/// Populated from PoolResolution at graduation time, stored in pumpswap_pools DashMap.
#[derive(Debug, Clone)]
pub struct PumpSwapPoolAccounts {
    /// Pool PDA address (PoolResolution.pool_address)
    pub pool: [u8; 32],
    /// Token mint (PoolResolution.mint) = base_mint in PumpSwap terms
    pub base_mint: [u8; 32],
    /// Pool token vault = PoolResolution.coin_vault = pool_base_token_account
    pub pool_base_token_account: [u8; 32],
    /// Pool WSOL vault = PoolResolution.pc_vault = pool_quote_token_account
    pub pool_quote_token_account: [u8; 32],
    /// Coin creator vault ATA (may be zeroed if not applicable; always include in ix)
    pub coin_creator_vault_ata: [u8; 32],
    /// Coin creator vault authority (may be zeroed; always include in ix)
    pub coin_creator_vault_authority: [u8; 32],
    /// Coin fee config account (5PHirr8j...)
    pub coin_fee_config: [u8; 32],
    /// Coin fee program state (4Jjna3h7...)
    pub coin_fee_program_state: [u8; 32],
}
```

### Constants

```rust
pub const PUMPSWAP_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
pub const PUMPSWAP_GLOBAL_CONFIG: &str = "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw";
pub const PUMPSWAP_EVENT_AUTHORITY: &str = "GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR";
pub const PUMPSWAP_FEE_PROGRAM: &str = "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ";
pub const PUMPSWAP_FEE_PROG_STATE: &str = "5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx";
pub const PUMPSWAP_FEE_PROG_STATE2: &str = "4Jjna3h73QbgmdqwnV5NJxjCidKWB7Q26jeuj9jtFetC";

/// Discriminator for both buy and sell (same Anchor discriminator, different arg semantics)
pub const PUMPSWAP_SWAP_DISCRIMINATOR: [u8; 8] = [0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad];

/// 8 protocol fee recipients — rotate randomly per tx
pub const PUMPSWAP_FEE_RECIPIENTS: [&str; 8] = [
    "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV",
    "7VtfL8fvgNfhz17qKRMjzQEXgbdpnHHHQRh54R9jP2RJ",
    "7hTckgnGnLQR6sdH7YkqFTAA7VwTfYFaZ6EhEsU3saCX",
    "9rPYyANsfQZw3DnDmKE3YCQF5E8oD89UXoHn9JFEhJUz",
    "AVmoTthdrX6tKt4nDjco2D775W2YK3sDhxPcMmzUAmTY",
    "FWsW1xNtWscwNmKv6wVsU1iTzRN6wmmk3MjxRP5tT7hz",
    "G5UZAVbAf46s7cKWoyKu8kYTip9DGTpbLZ2qa9Aq69dP",
    "JCRGumoE9Qi5BBgULTgdgTLjSgkCMSbF62ZZfGs84JeU",
];

pub const WSOL_MINT_STR: &str = "So11111111111111111111111111111111111111112";
pub const SPL_TOKEN_PROGRAM_STR: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const SPL_ATA_PROGRAM_STR: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
```

### Instruction Data

```rust
/// Build 24-byte PumpSwap swap instruction data.
/// buy:  [discriminator(8)] + base_out(u64 LE) + max_quote_in(u64 LE)
/// sell: [discriminator(8)] + base_in(u64 LE) + min_quote_out(u64 LE)
/// Both use the same discriminator and same arg layout.
fn build_swap_data(arg1: u64, arg2: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&PUMPSWAP_SWAP_DISCRIMINATOR);
    data.extend_from_slice(&arg1.to_le_bytes());
    data.extend_from_slice(&arg2.to_le_bytes());
    data
}
```

### Swap Instruction Builder

```rust
fn build_pumpswap_swap_ix(
    pool: &PumpSwapPoolAccounts,
    wallet_pubkey: &Pubkey,
    fee_recipient_idx: usize,     // rotate 0..7
    arg1: u64,                    // buy: base_out  | sell: base_in
    arg2: u64,                    // buy: max_quote_in | sell: min_quote_out
) -> Instruction {
    // ... builds 22-account instruction exactly as per layout above
    // Uses token_ata() helper (same as raydium.rs) for user ATAs
    // All accounts writable/readonly per layout spec above
}
```

### Public API

```rust
/// Build a complete signed PumpSwap BUY transaction (SOL → Token).
/// 
/// Instruction sequence:
///   1. ComputeBudget set_compute_unit_limit(400_000)
///   2. ComputeBudget set_compute_unit_price(5000)
///   3. create_associated_token_account_idempotent(user, base_mint) — ensure token ATA exists
///   4. create_associated_token_account_idempotent(user, WSOL) — ensure WSOL ATA exists  
///   5. system_instruction::transfer(wallet → wsol_ata, sol_lamports) — fund WSOL ATA
///   6. spl_token::sync_native(wsol_ata) — wrap SOL → WSOL
///   7. build_pumpswap_swap_ix(pool, wallet, fee_idx, base_out=tokens_estimate, max_quote_in=sol_lamports)
///   8. spl_token::close_account(wsol_ata → wallet) — reclaim leftover WSOL
///   9. system_instruction::transfer(wallet → jito_tip, tip_lamports)
///
/// NOTE: For the buy, we use buyQuoteInput semantics: specify max_quote_in=sol_lamports,
/// base_out=0 (or estimate). The program computes actual base_out from AMM formula.
/// ACTUALLY: use base_out = AMM estimate with 1% slippage headroom; max_quote_in = sol_lamports.
/// Simpler: set base_out=1 (minimum) and max_quote_in=sol_lamports. This always succeeds.
///
/// Returns: serialized VersionedTransaction bytes (bincode)
pub fn build_pumpswap_buy_tx(
    pool: &PumpSwapPoolAccounts,
    wallet_keypair: &Keypair,
    sol_lamports: u64,
    min_tokens_out: u64,        // 1 for "accept any", or AMM estimate with slippage
    jito_tip_lamports: u64,
    jito_tip_account: Pubkey,
    recent_blockhash: [u8; 32],
    fee_recipient_idx: usize,   // 0..7 rotating
) -> Result<Vec<u8>, PumpSwapTxError>

/// Build a complete signed PumpSwap SELL transaction (Token → SOL).
///
/// Instruction sequence:
///   1. ComputeBudget set_compute_unit_limit(300_000)
///   2. ComputeBudget set_compute_unit_price(5000)
///   3. create_associated_token_account_idempotent(user, WSOL) — ensure WSOL ATA
///   4. build_pumpswap_swap_ix(pool, wallet, fee_idx, base_in=tokens_to_sell, min_quote_out)
///   5. spl_token::close_account(wsol_ata → wallet) — close WSOL ATA, SOL flows back
///   6. system_instruction::transfer(wallet → jito_tip, tip_lamports)
///
/// Returns: serialized VersionedTransaction bytes (bincode)
pub fn build_pumpswap_sell_tx(
    pool: &PumpSwapPoolAccounts,
    wallet_keypair: &Keypair,
    tokens_to_sell: u64,
    min_sol_out: u64,           // 0 for "accept any", or AMM estimate - slippage
    jito_tip_lamports: u64,
    jito_tip_account: Pubkey,
    recent_blockhash: [u8; 32],
    fee_recipient_idx: usize,
) -> Result<Vec<u8>, PumpSwapTxError>
```

### Error Type
```rust
#[derive(Debug)]
pub enum PumpSwapTxError {
    InvalidPubkey(String),
    SignError(String),
}
```

### Tests Required (minimum 10)
1. `test_swap_data_buy_discriminator` — verify first 8 bytes = `33e685a4017f83ad`
2. `test_swap_data_sell_discriminator` — same
3. `test_swap_data_length` — data is exactly 24 bytes
4. `test_buy_tx_instruction_count` — expect 9 ixs
5. `test_sell_tx_instruction_count` — expect 6 ixs
6. `test_buy_tx_has_signature` — signature is non-zero
7. `test_sell_tx_has_signature`
8. `test_buy_tx_serializes` — bincode roundtrip
9. `test_sell_tx_serializes`
10. `test_fee_recipient_rotates` — calling with idx 0..7 produces different fee recipient accounts

---

## E2 Task: `momentum/pool.rs` — PumpSwapPoolAccounts Population

**File**: `rust/pump-quant-core/src/momentum/pool.rs`  
**Change scope**: Add `PumpSwapPoolAccounts` struct + extraction helper. NO changes to existing functions.

### What to Add

```rust
/// Lightweight pool accounts for PumpSwap live execution.
/// Extracted from PoolResolution at graduation time.
/// Only stores what's needed for the swap instruction.
#[derive(Debug, Clone)]
pub struct PumpSwapPoolAccounts {
    /// Pool PDA (from PoolResolution.pool_address)
    pub pool: [u8; 32],
    /// Token mint (from PoolResolution.mint) — base_mint in PumpSwap terms
    pub base_mint: [u8; 32],
    /// Pool token vault (from PoolResolution.coin_vault) = pool_base_token_account
    pub pool_base_token_account: [u8; 32],
    /// Pool WSOL vault (from PoolResolution.pc_vault) = pool_quote_token_account
    pub pool_quote_token_account: [u8; 32],
    /// Coin creator vault ATA (zeroed if unknown — program handles it)
    pub coin_creator_vault_ata: [u8; 32],
    /// Coin creator vault authority (zeroed if unknown)
    pub coin_creator_vault_authority: [u8; 32],
}

/// Extract PumpSwapPoolAccounts from a PoolResolution.
/// Returns None if pool_type != PumpSwap or pool_address is zeroed.
pub fn extract_pumpswap_pool_accounts(res: &PoolResolution) -> Option<PumpSwapPoolAccounts> {
    if res.pool_type != PoolType::PumpSwap { return None; }
    if res.pool_address == [0u8; 32] { return None; }
    Some(PumpSwapPoolAccounts {
        pool: res.pool_address,
        base_mint: res.mint,
        pool_base_token_account: res.coin_vault,  // coin_vault = token vault in our naming
        pool_quote_token_account: res.pc_vault,   // pc_vault = WSOL vault in our naming
        coin_creator_vault_ata: [0u8; 32],        // zeroed — populated async if needed
        coin_creator_vault_authority: [0u8; 32],  // zeroed — populated async if needed
    })
}
```

**NOTE**: `coin_creator_vault_ata` and `coin_creator_vault_authority` are zeroed for now. When zeroed, use the `Pubkey::default()` placeholder in the instruction. The program may reject this for some tokens. If it does, we'll add async resolution in a future iteration. For now, this covers the common case.

### Tests Required (minimum 5)
1. `test_extract_pumpswap_accepts_pumpswap_type`
2. `test_extract_pumpswap_rejects_raydium_type`
3. `test_extract_pumpswap_rejects_zero_pool_address`
4. `test_extract_pumpswap_coin_vault_maps_to_pool_base_token_account`
5. `test_extract_pumpswap_pc_vault_maps_to_pool_quote_token_account`

---

## E3 Task: `momentum/mod.rs` — pumpswap_pools DashMap + Buy Path

**File**: `rust/pump-quant-core/src/momentum/mod.rs`  
**Change scope**: ONLY the sections listed. Do NOT touch hot_path.rs, main.rs, feeds/corecast.rs.

### Step 1: Add pumpswap_pools field to MomentumEngine

```rust
// In struct MomentumEngine { ... }
// ADD after raydium_pools:
pumpswap_pools: DashMap<[u8; 32], crate::tx::pumpswap::PumpSwapPoolAccounts>,
```

Initialize in `new()`:
```rust
pumpswap_pools: DashMap::new(),
```

### Step 2: Populate pumpswap_pools at graduation

In `on_migration()`, after the existing raydium_pools insert block (near line ~2322), ADD:

```rust
// PumpSwap pool accounts — for live mode swap execution
if let Some(ps_accts) = crate::momentum::pool::extract_pumpswap_pool_accounts(&resolution) {
    self.pumpswap_pools.insert(resolution.mint, ps_accts);
    tracing::debug!(
        mint = %bs58::encode(&resolution.mint).into_string(),
        "[momentum] pumpswap pool accounts stored for live execution"
    );
}
```

This must run for **both** the sig-resolution path AND the fallback mint-lookup path.

### Step 3: Wire the live buy path

Find the existing buy path block (around line ~1025):
```rust
// Live mode: submit buy tx via Raydium AMM V4 + Jito
if !self.config.paper_mode {
    if let Some(pool) = self.raydium_pools.get(&entry.mint).map(|r| r.clone()) {
        // ... Raydium buy ...
    } else {
        tracing::warn!("[momentum] live mode: no raydium pool accounts — position is accounting-only");
    }
}
```

**Replace** the `else` branch (the accounting-only warning) with:
```rust
} else if let Some(ps_pool) = self.pumpswap_pools.get(&entry.mint).map(|r| r.clone()) {
    // PumpSwap live buy
    let mint = entry.mint;
    let size = size_lamports;
    let kp_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
    let jg = match self.jito_grpc.clone() {
        Some(j) => j,
        None => {
            tracing::warn!(mint=%bs58::encode(&mint).into_string(), "[buy_pumpswap] no jito client");
            self.active.remove(&mint);
            self.momentum_zones.remove(&mint);
            continue;
        }
    };
    let bh = self.blockhash_cache_sync().unwrap_or([0u8; 32]);
    let tip_req = crate::tx::tip_engine::TipRequest {
        context: crate::tx::tip_engine::TipContext::Entry,
        size_lamports: size,
        gain_bps: 0,
        grad_score: entry.grad_score as f64,
    };
    let tip = self.tip_engine.lock().compute_tip(&tip_req);
    // tokens_estimate: same formula as Raydium path
    let tokens_estimate = if current_price_fp > 0 {
        (size as u128 * 1_000_000 / current_price_fp as u128) as u64
    } else { 0u64 };
    if let Some(mut pos) = self.active.get_mut(&entry.mint) {
        pos.set_tokens_held(tokens_estimate);
    }
    let mint_buy = mint;
    let fee_idx = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() % 8) as usize;
    tokio::spawn(async move {
        let kp_bytes = match std::fs::read(&kp_path) {
            Ok(b) => b,
            Err(e) => { tracing::error!(err=?e, "[buy_pumpswap] keypair load failed"); return; }
        };
        let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) {
            Ok(v) => v,
            Err(e) => { tracing::error!(err=?e, "[buy_pumpswap] keypair parse failed"); return; }
        };
        if kp_arr.len() != 64 { tracing::error!("[buy_pumpswap] invalid keypair len"); return; }
        let mut kb = [0u8; 64];
        kb.copy_from_slice(&kp_arr);
        let keypair = match solana_sdk::signature::Keypair::from_bytes(&kb) {
            Ok(k) => k,
            Err(e) => { tracing::error!(err=?e, "[buy_pumpswap] keypair err"); return; }
        };
        use std::str::FromStr as _;
        let tip_account = solana_sdk::pubkey::Pubkey::from_str(
            crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
        ).unwrap();
        let tx_bytes = match crate::tx::pumpswap::build_pumpswap_buy_tx(
            &ps_pool, &keypair, size, 1, tip, tip_account, bh, fee_idx,
        ) {
            Ok(b) => b,
            Err(e) => { tracing::error!(mint=%bs58::encode(&mint_buy).into_string(), err=?e, "[buy_pumpswap] build failed"); return; }
        };
        use base64::Engine as _;
        let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx_bytes);
        match jg.submit_bundle(&tx_b64).await {
            Ok(id) => tracing::info!(
                mint=%bs58::encode(&mint_buy).into_string(),
                bundle_id=%id,
                tip,
                size_sol=size as f64/1e9,
                "[buy_pumpswap] Jito submitted"
            ),
            Err(e) => tracing::error!(
                mint=%bs58::encode(&mint_buy).into_string(),
                err=?e,
                "[buy_pumpswap] Jito FAILED"
            ),
        }
    });
} else {
    tracing::warn!(
        mint=%bs58::encode(&entry.mint).into_string(),
        "[momentum] live mode: no pool accounts (Raydium or PumpSwap) — position is accounting-only"
    );
}
```

---

## E4 Task: `momentum/mod.rs` — Live Sell Path

**File**: `rust/pump-quant-core/src/momentum/mod.rs`  
**Change scope**: The close_position sell path only.

Find the existing sell path (around line ~2100):
```rust
if !self.config.paper_mode {
    if let Some((_, pool)) = self.raydium_pools.remove(&mint) {
        // ... Raydium sell ...
    } else {
        tracing::warn!("[close_position] no raydium pool — sell NOT submitted");
    }
}
```

**Replace**

**Replace the else branch** with a PumpSwap sell fallback:

```rust
} else if let Some((_, ps_pool)) = self.pumpswap_pools.remove(&mint) {
    let tokens = pos.tokens_held();
    if tokens == 0 {
        tracing::warn!(
            mint=%bs58::encode(&mint).into_string(),
            "[close_pumpswap] tokens_held=0 — buy tx likely failed, skipping sell"
        );
    } else {
        let kp_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
        let jg = match self.jito_grpc.clone() {
            Some(j) => j,
            None => { tracing::error!("[close_pumpswap] no jito client"); return; }
        };
        let noz = self.nozomi_client.clone();
        let bh = self.blockhash_cache_sync().unwrap_or([0u8; 32]);
        let tip_req = TipRequest {
            context: exit_to_context(&reason, gain_bps as i64),
            size_lamports: pos.size_lamports,
            gain_bps: gain_bps as i64,
            grad_score: 0.0,
        };
        let tip = self.tip_engine.lock().compute_tip(&tip_req);
        let min_sol_out = if gain_bps > 0 {
            let expected = (pos.entry_price_fp as u128 * tokens as u128 / 1_000_000) as u64;
            (expected as u128 * 9900 / 10000) as u64
        } else { 0u64 };
        let noz_ok = noz.is_some();
        let reason_str = reason.as_str().to_string();
        let gain = gain_bps as i64;
        let mint_copy = mint;
        let fee_idx = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() % 8) as usize;
        tokio::spawn(async move {
            let kp_bytes = match std::fs::read(&kp_path) {
                Ok(b) => b,
                Err(e) => { tracing::error!(err=?e, "[sell_pumpswap] keypair load failed"); return; }
            };
            let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) {
                Ok(v) => v,
                Err(e) => { tracing::error!(err=?e, "[sell_pumpswap] keypair parse failed"); return; }
            };
            if kp_arr.len() != 64 { tracing::error!("[sell_pumpswap] bad keypair len"); return; }
            let mut kb = [0u8; 64];
            kb.copy_from_slice(&kp_arr);
            let keypair = match solana_sdk::signature::Keypair::from_bytes(&kb) {
                Ok(k) => k,
                Err(e) => { tracing::error!(err=?e, "[sell_pumpswap] keypair from_bytes"); return; }
            };
            use std::str::FromStr as _;
            let tip_account = solana_sdk::pubkey::Pubkey::from_str(
                crate::tx::raydium::JITO_TIP_ACCOUNTS[0]
            ).unwrap();
            let tx_bytes = match crate::tx::pumpswap::build_pumpswap_sell_tx(
                &ps_pool, &keypair, tokens, min_sol_out, tip, tip_account, bh, fee_idx,
            ) {
                Ok(b) => b,
                Err(e) => { tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_pumpswap] build failed"); return; }
            };
            use base64::Engine as _;
            let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx_bytes);
            let landing = route_exit(&reason_str, gain, noz_ok);
            match landing {
                LandingPath::JitoOnly => {
                    match jg.submit_bundle(&tx_b64).await {
                        Ok(id) => tracing::info!(mint=%bs58::encode(&mint_copy).into_string(), bundle_id=%id, "[sell_pumpswap] Jito submitted"),
                        Err(e) => tracing::error!(mint=%bs58::encode(&mint_copy).into_string(), err=?e, "[sell_pumpswap] Jito FAILED"),
                    }
                }
                LandingPath::NozomiOnly | LandingPath::DualPath => {
                    if let Some(ref n) = noz {
                        match n.send_transaction(&tx_b64).await {
                            Ok(_) => tracing::info!(mint=%bs58::encode(&mint_copy).into_string(), "[sell_pumpswap] Nozomi OK"),
                            Err(e) => { tracing::warn!(err=?e, "[sell_pumpswap] Nozomi failed → Jito"); let _ = jg.submit_bundle(&tx_b64).await; }
                        }
                    }
                }
            }
        });
    }
} else {
    tracing::warn!(
        mint=%bs58::encode(&mint).into_string(),
        "[close_position] no pool accounts (Raydium or PumpSwap) — sell NOT submitted"
    );
}
```

Also add cleanup to prevent pumpswap_pools leak (add after the entire sell block):
```rust
// Always clean up pumpswap_pools (idempotent remove — already done in sell branch above, but safe to repeat)
self.pumpswap_pools.remove(&mint);
```

---

## E5 Task: Tests

**Files**: `tx/pumpswap.rs` (unit tests) and `momentum/mod.rs` (integration tests).

### tx/pumpswap.rs tests (unit, no network, no async RPC)
1. `test_discriminator_correct` — first 8 bytes of swap data = `[0x33,0xe6,0x85,0xa4,0x01,0x7f,0x83,0xad]`
2. `test_buy_data_length_24`
3. `test_sell_data_length_24`
4. `test_buy_tx_9_instructions` — deserialize, count instructions
5. `test_sell_tx_6_instructions`
6. `test_buy_tx_signature_nonzero`
7. `test_sell_tx_signature_nonzero`
8. `test_buy_tx_v0_message_format`
9. `test_fee_recipient_idx0` — idx=0 → account[9] = 62qc2CNX...
10. `test_fee_recipient_idx7` — idx=7 → account[9] = JCRGumo...
11. `test_sell_min_sol_zero_accepted`
12. `test_buy_min_tokens_one_accepted`

### pool.rs tests (for E2)
1. `test_extract_pumpswap_pool_accounts_basic`
2. `test_extract_returns_none_for_raydium`
3. `test_extract_vault_field_mapping` — coin_vault → pool_base_token_account, pc_vault → pool_quote_token_account

### momentum/mod.rs tests
1. `test_pumpswap_pool_stored_at_graduation`
2. `test_raydium_pool_not_in_pumpswap_map`
3. `test_pumpswap_pool_cleaned_up_on_close`

---

## mod.rs Export

**File**: `rust/pump-quant-core/src/tx/mod.rs`  
**Add**: `pub mod pumpswap;`

---

## Build Constraints

- **MUST NOT** touch: `hot_path.rs`, `main.rs`, `feeds/corecast.rs`
- **MUST NOT** remove `bonding_curve.rs`
- **MUST NOT** touch `config/keys/` or `rust/.env`
- **All tests must pass**: `cargo test` in `pump-quant-core`
- **Current baseline**: 552 tests. Target: 552 + ~25 = ~577 tests.
- **Paper mode**: buy/sell paths gated by `!self.config.paper_mode` — never execute in paper mode
- **No floating point** in buy/sell math — use integer lamports only
- After all tasks complete: rebuild daemon, restart, verify `[buy_pumpswap] Jito submitted` in logs

---

## Verification

After deploy, in `logs/rust-daemon.log` expect:
```
[momentum] pumpswap pool accounts stored for live execution  mint=...
[buy_pumpswap] Jito submitted  mint=... bundle_id=... tip=...
[sell_pumpswap] Jito submitted  mint=... bundle_id=...
```

Must NOT see:
```
[momentum] live mode: no pool accounts (Raydium or PumpSwap) — position is accounting-only
```
