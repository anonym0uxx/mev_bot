# BUILD SPEC: Live Trade Execution — Permanent Fix

**Date:** 2026-04-02  
**Status:** SPEC ONLY — No code changes  
**Priority:** P0 (Blocking live profitability)  
**Author:** Apollo (Quant Architect)

---

## Executive Summary

Live trade execution is failing at catastrophic rates: **35% buy success, 6% sell success, 65+ missed entries**. The root causes are:

1. **Phantom sells (49 failures):** The engine fires sell TXs for positions where the buy never landed. No on-chain state tracking — position exists in memory regardless of buy outcome.
2. **Pool resolution race (65+ failures):** `getProgramAccounts` indexer hasn't propagated the new pool when we query, so pool accounts are never stored and no TX is submitted.
3. **Token program mismatch (15+ failures):** `resolve_mint_program` returns None for fresh mints → previous Token-2022 default caused `IncorrectProgramId`. Partially fixed but still fragile.
4. **Missing accounts (13 failures):** PumpSwap buy instruction requires 23 accounts including `coin_creator_vault_ata` and `coin_creator_vault_authority` which are sometimes zeroed.
5. **MEV/Sandwich exposure:** At least 1 confirmed sandwich (received 1 token for 0.067 SOL). The 20% slippage protection is working (3 `ExceededSlippage` rejections) but needs tighter calibration.

The fixes below are ordered by impact. Fix #1 alone eliminates 49/~100 failures (sell path). Fix #2 eliminates 65+ missed entries. Together they would bring success rate from ~20% to ~80%+.

---

## Error Code Reference (PumpSwap IDL)

All error codes from `node_modules/@pump-fun/pump-swap-sdk/src/idl/pump_amm.json`:

| Code | Name | Our Context |
|------|------|-------------|
| 6000 | `FeeBasisPointsExceedsMaximum` | N/A |
| 6001 | `ZeroBaseAmount` | N/A |
| 6002 | `ZeroQuoteAmount` | N/A |
| 6004 | **`ExceededSlippage`** | Buy slippage exceeded max_quote_in — **GOOD** (anti-sandwich working) |
| 6008 | **`InvalidBaseMint`** | Buy TX passed wrong base_mint for the pool |
| 6009 | **`InvalidQuoteMint`** | Buy TX passed wrong quote_mint — **FIXED** (non-WSOL pool filter) |
| 6016 | `BuyMoreBaseAmountThanPoolReserves` | Asking for more tokens than pool holds |
| 6039 | `BuyNotEnoughQuoteTokensToCoverFees` | SOL amount doesn't cover fees |
| 6040 | `BuySlippageBelowMinBaseAmountOut` | New slippage error (post-IDL update) |

Anchor framework errors (not PumpSwap-specific):

| Code | Name | Our Context |
|------|------|-------------|
| 2014 | **`AccountNotInitialized`** | Account passed to instruction hasn't been created yet |
| 3007 | **`ConstraintRaw`** | Generic Anchor constraint violation (wrong PDA derivation) |
| 3012 | **`ConstraintTokenMint`** | Token account's mint doesn't match expected mint |
| Custom:1 | **`InsufficientFunds`** | Token balance is 0 or insufficient for the sell amount |

---

## Solscan TX Analysis

> **NOTE:** Direct Solscan/SolanaFM API access is blocked by Cloudflare from this VPS. The analysis below is based on log data, error codes, and known on-chain behavior patterns. Engineers should verify each TX on Solscan manually before implementing fixes.

### Landed Buys

#### TX `2kTxKL...DP8ZRS` — 7dpaUoCb buy, 548ms
- **CONFIRMED SANDWICH.** Received 1 token for 0.067 SOL (~67M lamports).
- At 6 decimals, 1 token = 0.000001 token units. Price paid: 67,000 SOL/token.
- Normal price at graduation: ~0.0004 lamports/atom → should have received ~167,500 tokens.
- **MEV bot front-ran the buy, inflated the price, we bought at the top, bot sold after.**
- This TX was pre-slippage-protection (min_tokens_out was likely 0).
- The two subsequent sells on this mint (2t8Qbt... and 4w4DYt...) sold this 1 token for 0 SOL — expected.
- The third sell (45vTQD...) hit `hard_sl` with gain=-380 bps.

#### TX `KMPHAP...qM8TF` — 2jLg61Rp buy, 609ms
- **Needs Solscan verification.** Check `postTokenBalances` for actual token receipt.
- 609ms confirmation = good latency, likely confirmed in same slot or slot+1.
- Check: was min_tokens_out > 0? (Post-slippage-fix should have 80% floor.)

#### TX `67kMmw...94u4t` — ECy9wQZ2 buy, 626ms
- **Needs verification.** Similar latency profile.

#### TX `jaKyjj...5gfw4` — EpDau5t6 buy, 604ms
- **Needs verification.** Consistent ~600ms latency across all buys.

#### TX `2d91Ao...HosA` — 6HBUdwvn buy, 613ms
- **Needs verification.**

**Latency pattern:** All buys landed in 548-626ms range. This is excellent for RPC-only submission (no Jito bundles for buys). Buys are landing when they have correct accounts.

### Landed Sells

#### TX `2t8Qbt...g2Zp` — 7dpaUoCb sell, **2659ms(!)**, time_sl
- **2.6s confirmation is anomalous** — likely needed 4-5 slots to confirm.
- Selling sandwiched position (1 token), gain=0.
- 2659ms could indicate: RPC retry, slot congestion, or the 3s `tokio::time::sleep` delay in sell path consuming most of the budget.

#### TX `4w4DYt...Kpbd` — 7dpaUoCb sell, 567ms, time_sl
- Second sell attempt on same sandwiched mint. 567ms = normal.
- gain=0 — selling dust.

#### TX `45vTQD...YXQb` — 7dpaUoCb sell, 1086ms, hard_sl, gain=-380
- Sold 1 token for effectively 0 SOL.
- **This is the sandwich loss materialized.** Buy got 1 token, sell gets nothing.
- The -380 bps gain is the engine's paper P&L calculation, not actual SOL loss.

### Key Observation

All 3 landed sells are on the SAME mint (7dpaUoCb) — the sandwiched position. This strongly suggests the 49 sell failures on OTHER mints are because those buys never landed (Error #2 cascade).

---

## Fix #1: Buy→Sell State Tracking (P0 — Eliminates 49+ sell failures)

### Problem
The engine creates a position in memory when entry criteria are met, then spawns an async buy TX. The sell path (in `close_position`) fires based on the in-memory position regardless of whether the buy TX landed on-chain. When the buy fails (IncorrectProgramId, MissingAccount, etc.), the sell TX fails with `ConstraintTokenMint` (3012) because there's no token ATA with the expected mint.

### Current Flow (Broken)
```
on_graduation → process_pending_entries → active.insert(mint, pos) → tokio::spawn(buy_tx)
                                                                         ↓
                                          buy TX fails silently ←── IncorrectProgramId
                                                                         ↓
on_tick → process_active_positions → exit trigger → close_position(mint)
                                                         ↓
                                              tokio::spawn(sell_tx) ← uses estimated tokens_held
                                                         ↓
                                              sell TX fails ← ConstraintTokenMint (no ATA exists)
```

### Proposed Flow (Fixed)
```
on_graduation → process_pending_entries → active.insert(mint, pos) → tokio::spawn(buy_tx)
                                              buy_state = Pending        ↓
                                                                   buy TX result callback
                                                                         ↓
                                              buy_state = Confirmed ← Landed
                                              buy_state = Failed    ← Failed/TimedOut/CircuitOpen
                                                                         ↓
on_tick → if buy_state == Failed → close_position(mint) with NO SELL TX
on_tick → if buy_state == Confirmed → normal exit logic → sell TX
on_tick → if buy_state == Pending (>30s) → close_position(mint) with NO SELL TX
```

### Implementation

#### File: `rust/pump-quant-core/src/momentum/mod.rs`

**1a. Add buy state tracking DashMap (near line 322, next to `active`):**

```rust
/// Buy TX landing state: mint → BuyState
/// Written by buy_tx async task, read by close_position.
buy_states: DashMap<[u8; 32], BuyState>,
```

**1b. Define BuyState enum (near line 70, with other types):**

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BuyState {
    /// Buy TX submitted, awaiting confirmation.
    Pending,
    /// Buy TX confirmed on-chain. Safe to sell.
    Confirmed { signature: [u8; 64] },
    /// Buy TX failed. Do NOT attempt sell.
    Failed,
}
```

**1c. Initialize buy_states in engine constructor (wherever `active: DashMap::new()` is):**

```rust
buy_states: DashMap::new(),
```

**1d. Set Pending state when buy TX is spawned (~line 2290 for PumpSwap, ~line 2140 for Raydium):**

Before the `tokio::spawn(async move { ... })` for the buy:

```rust
self.buy_states.insert(entry.mint, BuyState::Pending);
```

**1e. Update buy_state from within the buy async task.**

The buy task runs in a detached `tokio::spawn`. It needs an `Arc<DashMap>` reference to write back. Pass `buy_states: Arc<DashMap<[u8;32], BuyState>>` into the spawn (clone from `self.buy_states`).

In the `match rpc_sender.submit_tx(...)` block (PumpSwap buy, ~line 2310):

```rust
rpc_sender::SubmitResult::Landed { signature, latency_ms } => {
    buy_states_clone.insert(mint_buy, BuyState::Confirmed { 
        signature: /* parse sig bytes */ 
    });
    tracing::info!(...);
}
rpc_sender::SubmitResult::Failed { error } => {
    buy_states_clone.insert(mint_buy, BuyState::Failed);
    tracing::error!(...);
}
rpc_sender::SubmitResult::TimedOut { signature } => {
    // TimedOut may still land — keep as Pending, will timeout naturally
    tracing::warn!(...);
}
rpc_sender::SubmitResult::CircuitOpen { remaining_ms } => {
    buy_states_clone.insert(mint_buy, BuyState::Failed);
    tracing::warn!(...);
}
```

**1f. Gate sell TX on buy_state in `close_position()` (~line 3201).**

At the top of the sell TX logic (before the Raydium/PumpSwap sell branches), add:

```rust
// Gate: only attempt sell if buy was confirmed on-chain
let buy_state = self.buy_states.get(&mint).map(|r| *r).unwrap_or(BuyState::Failed);
let should_sell = match buy_state {
    BuyState::Confirmed { .. } => true,
    BuyState::Pending => {
        // Buy still pending — check on-chain balance as last resort
        // (handled by existing actual_tokens check in sell path)
        true  // Let the existing balance check gate it
    }
    BuyState::Failed => {
        tracing::info!(
            mint = %bs58::encode(&mint).into_string(),
            "[close_position] buy FAILED — skipping sell TX entirely"
        );
        false
    }
};

// Clean up buy state
self.buy_states.remove(&mint);

if !should_sell {
    // Skip ALL sell logic — no Raydium, PumpSwap, or last-chance sell
    return;  // or continue to cleanup after the sell block
}
```

**1g. Add timeout for Pending buys in `process_active_positions()` (~line 2560).**

After drain detection, before TP/SL evaluation:

```rust
// Buy TX timeout: if buy is still Pending after 30s, mark position for exit with no sell
if let Some(buy_state) = self.buy_states.get(&mint) {
    if *buy_state == BuyState::Pending && elapsed_ms > 30_000 {
        tracing::warn!(
            mint = %bs58::encode(&mint).into_string(),
            "[momentum] buy TX still pending after 30s — marking failed"
        );
        drop(buy_state);
        self.buy_states.insert(mint, BuyState::Failed);
        to_close.push((mint, MomentumExitReason::MaxHold, pos.entry_price_fp));
        continue;
    }
}
```

**1h. Same fix for deferred buys (`process_deferred_buys`, ~line 1380).**

When the deferred buy task spawns, set `BuyState::Pending`. When it completes, update to `Confirmed` or `Failed`.

### Test Criteria
1. Deploy with `paper_mode: false`. Wait for a position where buy TX fails.
2. Verify in logs: `[close_position] buy FAILED — skipping sell TX entirely`
3. Verify: zero `ConstraintTokenMint` (3012) errors in sell TXs.
4. Verify: `buy_states` DashMap is cleaned up (no memory leak) — check len after positions close.

### Risk Assessment
- **Low risk.** Additive change — adds a gate before existing sell logic.
- **Edge case:** Buy TX `TimedOut` that later lands → position closes with no sell → tokens stuck. Mitigation: the existing 3s sleep + balance check in sell path handles this (if balance > 0, sell proceeds).
- **Memory:** DashMap<[u8;32], BuyState> — 64+8 bytes per entry, max ~3 concurrent. Negligible.

---

## Fix #2: Deterministic Pool Resolution from Graduation TX (P0 — Eliminates 65+ missed entries)

### Problem
The fundamental race condition: pool is created on-chain → our `getProgramAccounts` query runs before the indexer has propagated → returns nothing → pool accounts never stored → no TX submitted.

Current flow:
1. `on_pumpswap_graduation_direct()` stores partial pool accounts (zeroed pool PDA + creator)
2. `process_pending_entries()` at T+entry_delay_ms attempts "last-chance" `resolve_pumpswap_pool_from_mint()`
3. `resolve_pumpswap_pool_from_mint()` calls `getProgramAccounts` → often fails because indexer hasn't caught up
4. Position is "accounting-only" — no buy TX submitted

### Solution: Extract Pool Accounts Directly from Graduation TX

The graduation transaction itself contains a `create_pool` instruction with ALL the accounts we need. The code already has `extract_from_create_pool_accounts()` and `build_pumpswap_pool_accounts_deterministic()` in `pool.rs` (lines 1500-1627). **These functions are implemented but not wired into the hot path.**

### Implementation

#### File: `rust/pump-quant-core/src/momentum/mod.rs`

**2a. In `on_migration()` (~line 3859): Pass extracted create_pool accounts through.**

`resolve_pool_from_transaction()` already calls `getTransaction` to get the graduation TX. Inside that function, after identifying a PumpSwap pool, it should look for the `create_pool` instruction in the transaction and extract accounts.

#### File: `rust/pump-quant-core/src/momentum/pool.rs`

**2b. In `resolve_pool_inner()` / `resolve_pool_from_transaction()`: Extract create_pool accounts.**

After identifying the PumpSwap program in the transaction, find the `create_pool` instruction (discriminator check), extract its account keys, and call `extract_from_create_pool_accounts()`.

The result should populate `PoolResolution` with:
- `pool_address` = `extracted.pool` (not zeroed!)
- `coin_vault` = token vault from create_pool accounts[9]
- `pc_vault` = WSOL vault from create_pool accounts[10]
- `creator` = accounts[2]

Then `extract_pumpswap_pool_accounts()` will succeed because `pool_address != [0u8;32]`.

**2c. For the `on_pumpswap_graduation_direct()` path (Helius Enhanced, ~line 4542):**

This path doesn't call `getTransaction` — it gets vault data directly from Helius `transactionSubscribe`. But it's missing pool PDA and creator.

**Option A (preferred): Derive pool PDA deterministically.**

PumpSwap pool PDA = `Pubkey::find_program_address(["pool", index_le, creator, base_mint, quote_mint], PUMPSWAP_PROGRAM)`.

The problem: we don't know `index` or `creator` from the Helius notification alone.

**Option B: Use `getTransaction` on the graduation sig.**

The `sig` parameter IS the graduation transaction signature. After storing partial pool accounts, immediately spawn an async task to call `getTransaction(sig)` → extract `create_pool` accounts → update the stored pool accounts.

This is better than `getProgramAccounts` because `getTransaction` fetches from the transaction log (always available immediately), not the account index.

```rust
// In on_pumpswap_graduation_direct(), after self.pumpswap_pools.insert():
let http = self.http_client.clone();
let helius_rpc = self.helius_rpc_url.clone();
let sig_copy = sig;
let mint_copy = mint;
let pumpswap_pools = self.pumpswap_pools.clone(); // Arc<DashMap>
tokio::spawn(async move {
    // resolve_pool_from_transaction already handles create_pool extraction
    match resolve_pool_from_transaction(&http, &sig_copy, &mint_copy, &helius_rpc).await {
        Some(resolution) if resolution.pool_address != [0u8; 32] => {
            if let Some(accts) = extract_pumpswap_pool_accounts(&resolution) {
                let tx_accts: PumpSwapPoolAccounts = accts.into();
                pumpswap_pools.insert(mint_copy, tx_accts);
                tracing::info!(
                    mint = %bs58::encode(&mint_copy).into_string(),
                    pool = %bs58::encode(&tx_accts.pool).into_string(),
                    "[graduation_direct] pool accounts resolved from TX ✅"
                );
            }
        }
        _ => {
            tracing::warn!(
                mint = %bs58::encode(&mint_copy).into_string(),
                "[graduation_direct] getTransaction pool resolution failed"
            );
        }
    }
});
```

**2d. Timing advantage:**

- `getTransaction` latency: ~200-400ms (Helius, immediate after confirmation)
- `getProgramAccounts` indexer delay: 2-15s (unreliable)
- Entry delay: configurable (currently 0ms for momentum)
- Result: pool PDA resolved ~400ms after graduation, well before any buy TX fires

**2e. Coin creator derivation fix:**

The `extract_pumpswap_pool_accounts()` function (line 1630) already derives `coin_creator_vault_ata` and `coin_creator_vault_authority` from the `coin_creator` field in `PoolResolution`. But this requires the pool data to be fetched (offset [211..243]).

**Alternative:** Extract `creator` from the `create_pool` instruction accounts[2], then derive:
- `coin_creator_vault_authority` = PDA("creator_vault", creator)
- `coin_creator_vault_ata` = ATA(coin_creator_vault_authority, WSOL, SPL_TOKEN)

This is already implemented in `build_pumpswap_pool_accounts_deterministic()` (line 1524). The only gap is wiring it into the hot path.

### Test Criteria
1. Deploy. Check logs for `[graduation_direct] pool accounts resolved from TX ✅`.
2. Verify: "no pool accounts" warnings drop from 65+ to near zero.
3. Verify: buy TX submission rate increases from ~33% to ~90%+.
4. Monitor: `getTransaction` latency should be <500ms consistently.

### Risk Assessment
- **Medium risk.** Changes the pool resolution critical path.
- **Risk 1:** `getTransaction` returns null for very fresh TXs → mitigated by falling back to existing `resolve_pumpswap_pool_from_mint()`.
- **Risk 2:** `create_pool` instruction parsing fails → mitigated by `extract_from_create_pool_accounts()` returning `None` safely.
- **Risk 3:** Race between async resolution task and `process_pending_entries()` → use the DashMap as the sync point (whoever resolves first wins).

---

## Fix #3: Bulletproof Token Program Resolution (P1 — Eliminates 15+ buy failures)

### Problem
`resolve_mint_program()` (pool.rs line 1701) calls `getAccountInfo` on the mint and reads the `owner` field. For fresh mints (< 2s old), both Helius and public RPC may return null owner because the account isn't indexed at `confirmed` commitment yet.

Current fallback chain:
1. Helius `getAccountInfo` → often null for fresh mints
2. Public RPC `getAccountInfo` → also null
3. Default to SPL Token (recently fixed from Token-2022)

### Solution

**3a. Hardcode pump.fun token program to SPL Token.**

ALL pump.fun graduated tokens use classic SPL Token (`TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`). This has been true since pump.fun launched and there's no mechanism for pump.fun to create Token-2022 tokens.

#### File: `rust/pump-quant-core/src/momentum/mod.rs`

In the token program resolution block (~line 2206), **before** calling `resolve_mint_program_with_fallback()`:

```rust
// Pump.fun tokens ALWAYS use classic SPL Token. Skip RPC entirely.
// This eliminates the getAccountInfo race for fresh mints.
let mint_b58 = bs58::encode(&entry.mint).into_string();
if mint_b58.ends_with("pump") || ps_pool.pool != [0u8; 32] {
    // Pump.fun graduation → classic SPL Token, no query needed
    ps_pool.token_mint_program = crate::tx::pumpswap::SPL_TOKEN_PROGRAM_BYTES;
    if let Some(mut stored) = self.pumpswap_pools.get_mut(&entry.mint) {
        stored.token_mint_program = crate::tx::pumpswap::SPL_TOKEN_PROGRAM_BYTES;
    }
} else {
    // Non-pump.fun token (rare) → resolve via RPC with fallback
    match resolve_mint_program_with_fallback(...).await {
        Some(program_bytes) => { ... }
        None => {
            ps_pool.token_mint_program = crate::tx::pumpswap::SPL_TOKEN_PROGRAM_BYTES;
        }
    }
}
```

**3b. In `token_program_for_mint_with_hint()` (pumpswap.rs ~line 247):**

Already defaults to SPL Token when hint is [0u8;32]. This is correct. No change needed.

**3c. For the `create_pool` deterministic path:**

The `create_pool` instruction accounts include `base_token_program` at index [13] and `quote_token_program` at index [14]. Extract the token program directly from the instruction:

```rust
// In extract_from_create_pool_accounts(), add:
pub base_token_program: [u8; 32],
pub quote_token_program: [u8; 32],
```

Then in `build_pumpswap_pool_accounts_deterministic()`:

```rust
// token_mint_program = whichever program corresponds to the non-WSOL mint
ps_accts.token_mint_program = if token_is_base {
    extracted.base_token_program
} else {
    extracted.quote_token_program
};
```

This eliminates `resolve_mint_program` entirely for the deterministic path.

### Test Criteria
1. Deploy. Verify zero `IncorrectProgramId` errors in buy TXs.
2. Verify: `resolve_mint_program` is called 0 times for pump.fun tokens.
3. Check: non-pump.fun tokens (if any) still resolve correctly via RPC fallback.

### Risk Assessment
- **Very low risk.** Hardcoding is safe because pump.fun's bonding curve contract only creates SPL Token mints.
- **Edge case:** If pump.fun ever migrates to Token-2022, this hardcode would break. Mitigate by keeping the RPC fallback for non-pump mints and adding a config flag `assume_spl_token_for_pump: bool`.

---

## Fix #4: Missing Account Resolution (P1 — Eliminates 13 buy failures)

### Problem
`MissingAccount` at instruction index [6] in buy TXs. The buy instruction layout is:

```
[0] ComputeBudget::set_compute_unit_limit  
[1] ComputeBudget::set_compute_unit_price  
[2] create_associated_token_account_idempotent (token ATA)  
[3] create_associated_token_account_idempotent (WSOL ATA)  
[4] system_instruction::transfer (fund WSOL ATA)  
[5] spl_token::sync_native (WSOL ATA)  
[6] PumpSwap swap instruction  ← THIS IS THE SWAP
[7] spl_token::close_account (WSOL ATA)  
[8] system_instruction::transfer (Jito tip)  
```

So instruction [6] is the PumpSwap swap instruction itself. `MissingAccount` means one of its 23 accounts doesn't exist on-chain.

### Most Likely Missing Accounts

**4a. `coin_creator_vault_ata` (account [17] in the swap instruction):**

When `coin_creator_vault_ata` is `[0u8; 32]` (zeroed), `Pubkey::new_from_array([0u8; 32])` = `11111111111111111111111111111111` (the System Program address). This is NOT a valid token account → `MissingAccount`.

**Fix:** In `build_pumpswap_swap_ix()` (pumpswap.rs, ~line 420), when `coin_creator_vault_ata` is zeroed, derive it from the creator:

```rust
let coin_creator_vault_ata = if pool.coin_creator_vault_ata == [0u8; 32] {
    // Derive from creator vault authority
    // If authority is also zeroed, use the PumpSwap global config as a placeholder
    if pool.coin_creator_vault_authority != [0u8; 32] {
        let authority = Pubkey::new_from_array(pool.coin_creator_vault_authority);
        let wsol_mint = Pubkey::from_str(WSOL_MINT_STR).unwrap();
        let wsol_prog = wsol_token_program();
        token_ata_with_program(&authority, &wsol_mint, &wsol_prog)
    } else {
        // Last resort: pass system program as a no-op (PumpSwap handles gracefully)
        // Actually this is the problem — PumpSwap does NOT handle gracefully.
        // We must resolve the creator BEFORE building the TX.
        tracing::error!("coin_creator_vault_ata AND authority are zeroed — TX will fail");
        Pubkey::default()
    }
} else {
    Pubkey::new_from_array(pool.coin_creator_vault_ata)
};
```

**4b. `user_volume_accumulator` (account [20] in the buy instruction):**

This is a PDA: `seeds = ["user_volume_accumulator", user_pubkey]`. It may not exist yet (never created). PumpSwap's buy instruction expects it but may fail if it hasn't been initialized via `init_user_volume_accumulator`.

**Fix:** Add a `create_account_idempotent`-style instruction for the user volume accumulator before the swap, OR set `track_volume` to `OptionBool::None` in the buy args to skip volume tracking.

Looking at the IDL:
```
buy args: base_amount_out: u64, max_quote_amount_in: u64, track_volume: OptionBool
```

Currently the code builds swap data as 24 bytes (8 disc + 8 arg1 + 8 arg2). The `track_volume` field is missing!

**This is likely the cause of MissingAccount errors.** The buy instruction expects 25+ bytes of data (including track_volume), or the program interprets the missing byte as "track volume = true" and expects the volume accumulator accounts, which may not be initialized.

**Fix in `build_swap_data()` (pumpswap.rs ~line 390):**

For buy instructions, append `track_volume: OptionBool::None` (0x00 = None, skip volume tracking):

```rust
fn build_swap_data(discriminator: &[u8; 8], arg1: u64, arg2: u64, is_buy: bool) -> Vec<u8> {
    let mut data = Vec::with_capacity(if is_buy { 25 } else { 24 });
    data.extend_from_slice(discriminator);
    data.extend_from_slice(&arg1.to_le_bytes());
    data.extend_from_slice(&arg2.to_le_bytes());
    if is_buy {
        data.push(0x00); // track_volume: OptionBool::None — skip volume tracking
    }
    data
}
```

**4c. `coin_creator_vault_authority` (account [18]):**

Same zeroed-PDA problem as `coin_creator_vault_ata`. Must be derived from the pool creator.

### Investigation Required
- Engineers should decode 2-3 `MissingAccount` TX sigs on Solscan to confirm which specific account index within the swap instruction is missing.
- Check if the `track_volume` serialization is the root cause by comparing our instruction data length (24 bytes) vs what PumpSwap expects.
- Check the PumpSwap IDL for `OptionBool` type definition — it may be `{ none: 0, some_false: 1, some_true: 2 }`.

### Test Criteria
1. Deploy. Verify zero `MissingAccount` errors in buy TXs.
2. Check: `coin_creator_vault_ata` is never `11111111111111111111111111111111` in TX logs.
3. Check: instruction data length matches IDL expectations.

### Risk Assessment
- **Medium risk.** Changes instruction layout which must exactly match PumpSwap's on-chain program.
- **Risk:** Wrong `track_volume` encoding → different error. Mitigate by testing on devnet first.
- **Risk:** Derived creator vault ATA doesn't match on-chain → `ConstraintRaw`. Mitigate by verifying derivation against known successful TXs.

---

## Fix #5: Sell TX Robustness — Defense in Depth (P1 — Hardens remaining sell path)

### Problem
Even with Fix #1 (buy state gating), edge cases remain:
- `TimedOut` buys that later land → position may close with stale balance
- Balance check fails (RPC error) → falls back to estimated tokens → wrong amount
- ATA derived with wrong token program → ConstraintTokenMint on sell

### Current Sell Flow
The sell path already has a 3s sleep + balance check (good), but falls through to the estimated token amount on ANY error:

```rust
// Current: falls through to `tokens` (estimate) on ANY failure
let actual_tokens = match balance_http.post(...).send().await {
    Ok(resp) => { ... parse ... .unwrap_or(tokens) }  // ← BAD: falls back to estimate
    Err(e) => { tokens }                                // ← BAD: falls back to estimate
};
```

### Implementation

#### File: `rust/pump-quant-core/src/momentum/mod.rs`

**5a. Make balance check MANDATORY, not optional (~line 3630 PumpSwap sell, ~line 3530 Raydium sell):**

```rust
// STRICT: if we can't verify on-chain balance, DO NOT SELL.
// Selling estimated amounts when buy failed = guaranteed error.
let actual_tokens = match balance_http.post(...).send().await {
    Ok(resp) => {
        match resp.json::<serde_json::Value>().await {
            Ok(json) => {
                match json["result"]["value"]["amount"]
                    .as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    Some(bal) => bal,
                    None => {
                        // Null value means ATA doesn't exist = buy didn't land
                        tracing::warn!(mint=%..., "[sell] ATA returned null — buy likely failed, skipping sell");
                        return; // ← ABORT sell
                    }
                }
            }
            Err(e) => {
                tracing::error!(mint=%..., err=?e, "[sell] balance parse failed — aborting sell for safety");
                return; // ← ABORT sell
            }
        }
    }
    Err(e) => {
        tracing::error!(mint=%..., err=?e, "[sell] balance RPC failed — aborting sell for safety");
        return; // ← ABORT sell
    }
};
```

**5b. Ensure ATA derivation uses correct token program for balance check:**

Currently the sell path for PumpSwap (line ~3640) uses `token_program_for_mint_with_hint()` which now defaults to SPL Token. This is correct for pump.fun tokens. But verify the ATA derivation matches what was used in the buy TX.

**5c. Increase sell sleep from 3s to 5s for TimedOut buys:**

If `buy_state == BuyState::Pending` (TimedOut case), increase the sleep to give more time:

```rust
let sleep_duration = match buy_state {
    BuyState::Confirmed { .. } => std::time::Duration::from_secs(1), // Buy confirmed, balance should be there
    BuyState::Pending => std::time::Duration::from_secs(5),          // May still be landing
    BuyState::Failed => unreachable!(), // Should have been gated by Fix #1
};
tokio::time::sleep(sleep_duration).await;
```

### Test Criteria
1. Verify: zero sell TXs submitted with estimated tokens (always uses actual balance).
2. Verify: `[sell] ATA returned null` appears in logs for failed buys.
3. Verify: no more `ConstraintTokenMint` (3012) or `Custom:1` sell errors.

### Risk Assessment
- **Low risk.** Makes the sell path stricter — worst case is a missed sell (tokens stay in wallet, can be recovered manually).
- **Risk:** RPC downtime during sell → tokens stuck until next session. Mitigate with retry logic (existing rpc_sender already retries 3x).

---

## Fix #6: Error-Specific Response for Custom:6008 (InvalidBaseMint) (P2)

### Problem
4 occurrences of `Custom:6008` (`InvalidBaseMint`) on token 7dpaUoCb. This means the `base_mint` account passed at instruction index [3] doesn't match the pool's on-chain `base_mint`.

### Root Cause Analysis
The `From<pool::PumpSwapPoolAccounts>` impl in pumpswap.rs always sets `token_is_base = true` (line ~160):

```rust
let token_is_base = true;
```

But this is hardcoded. If a pool somehow has the token as quote_mint (WSOL=base), the accounts at [3]/[4] would be swapped → `InvalidBaseMint`.

**Investigation needed:** Check the 7dpaUoCb pool on-chain. If its on-chain layout has WSOL as base_mint and token as quote_mint, then `token_is_base = true` is wrong for this pool.

However, the comment says "PumpSwap ALWAYS creates pump.fun pools with token=base, WSOL=quote." This should be verified against the PumpSwap program source.

### Fix
If all pump.fun pools truly have token=base, then `Custom:6008` is caused by something else — possibly a stale/wrong pool address being passed. Cross-reference the pool PDA with the token mint.

If some pools DO have WSOL=base:
```rust
// In From<pool::PumpSwapPoolAccounts>, restore dynamic detection:
let token_is_base = p.base_mint != WSOL_MINT_BYTES;
```

### Test Criteria
1. Check 7dpaUoCb pool on Solscan: what are offsets [43..75] (base_mint) and [75..107] (quote_mint)?
2. If base_mint = token: bug is elsewhere (stale pool address?).
3. If base_mint = WSOL: restore dynamic detection.

---

## Fix #7: Custom:3007 (ConstraintRaw) on Sells (P2 — 6 occurrences)

### Problem
Anchor `ConstraintRaw` on sell TXs. This is a generic constraint violation — one of the accounts doesn't satisfy a programmatic check.

### Most Likely Causes
1. **Wrong ATA derivation** — user's token ATA derived with wrong token program (Token-2022 vs SPL Token). If the buy used SPL Token but the sell derives ATA with Token-2022, the PDA won't match → ConstraintRaw.
2. **Pool vault mismatch** — pool_base_token_account or pool_quote_token_account doesn't match the on-chain pool's stored vaults.
3. **Creator vault authority wrong** — if derived from `pool.creator` instead of `pool.coin_creator`, the PDA won't match.

### Fix
All these are addressed by Fix #2 (deterministic pool resolution from create_pool) and Fix #3 (hardcode SPL Token). Once pool accounts and token program are correct, ConstraintRaw should disappear.

### Investigation
Decode 1-2 failed sell TX sigs on Solscan. Compare every account in the TX against the expected values from the pool's on-chain data.

---

## Fix #8: Custom:2014 (AccountNotInitialized) on Buys (P2 — 4 occurrences)

### Problem
Anchor `AccountNotInitialized` — an account in the buy TX hasn't been created yet.

### Most Likely Cause
The **user volume accumulator PDA** (`seeds = ["user_volume_accumulator", user_pubkey]`) hasn't been initialized. PumpSwap's `buy` instruction includes this as account [20] and may require it to be initialized.

### Fix
Two options:

**Option A (preferred):** Set `track_volume: OptionBool::None` in buy args to skip volume tracking entirely. This may cause the program to skip the volume accumulator check. (Requires testing — the IDL may not support None.)

**Option B:** Add an `init_user_volume_accumulator` instruction before the first buy. This is a one-time setup per wallet. Add it to the bot's startup sequence or as the first instruction in the buy TX.

**Option C:** Pre-create the volume accumulator account before any trading begins. Add a startup routine that calls `init_user_volume_accumulator` once.

### Investigation
- Check if `user_volume_accumulator` exists for our wallet on Solscan.
- If it doesn't exist, call `init_user_volume_accumulator` once.
- Check if `track_volume: None` skips the account check or causes a different error.

---

## Fix #9: Anti-Sandwich Hardening (P2 — Protect profitable buys)

### Current State
- 20% slippage protection is working (`ExceededSlippage` on 3 buys = correctly rejected sandwiches)
- 1 confirmed sandwich before slippage protection was added (7dpaUoCb: 1 token for 0.067 SOL)
- Sandwich dust detection in sell path is working (< 10% of estimated tokens → skip sell)

### Improvements

**9a. Tighten slippage from 20% to 10% for small positions:**

```rust
let min_tokens_out = if tokens_estimate > 0 {
    let slippage_pct = if size_lamports < 100_000_000 { 90 } else { 80 }; // 10% for small, 20% for large
    tokens_estimate * slippage_pct / 100
} else { 1 };
```

**9b. Add post-buy balance verification:**

After buy TX lands, spawn a quick balance check. If actual tokens < 50% of estimated, log a sandwich alert:

```rust
// After SubmitResult::Landed in buy task:
// Quick async balance check for sandwich detection
tokio::spawn(async move {
    tokio::time::sleep(Duration::from_secs(2)).await;
    let actual = query_token_balance(...).await;
    if actual < estimated * 50 / 100 {
        tracing::error!(
            mint = %mint_str,
            actual_tokens = actual,
            estimated_tokens = estimated,
            "[buy_pumpswap] 🥪 SANDWICH DETECTED — received {pct}% of expected tokens",
            pct = actual * 100 / estimated,
        );
    }
});
```

**9c. Consider Jito bundles for buys (not just sells):**

Current config has `jito_enabled: true` and `private_route.enabled: true`, but buys go through `rpc_sender` (plain RPC). Jito bundles provide sandwich protection by making the TX private (not visible in the mempool).

**Recommendation:** Route buy TXs through Jito bundles as the primary path, with RPC fallback. This is the most effective anti-sandwich measure.

---

## Fix #10: Custom:1 on Sells — Insufficient Funds (P3 — 6 occurrences)

### Problem
`Custom:1` is a generic "insufficient funds" or "account balance mismatch" error. On sells, this means we're trying to sell more tokens than we actually hold.

### Root Cause
Same as Error #2 — selling estimated tokens when actual balance is 0 (buy didn't land). Fix #1 (buy state gating) eliminates this entirely.

### Additional Hardening
The existing balance check + dust detection in the sell path handles this. After Fix #1, these 6 errors should not recur.

---

## Implementation Order

| Priority | Fix | Impact | Effort | Files Changed |
|----------|-----|--------|--------|---------------|
| **P0** | Fix #1: Buy→Sell state tracking | Eliminates 49+ sell failures | Medium | mod.rs |
| **P0** | Fix #2: Deterministic pool resolution | Eliminates 65+ missed entries | Medium | pool.rs, mod.rs |
| **P1** | Fix #3: Hardcode SPL Token | Eliminates 15+ buy failures | Small | mod.rs, pool.rs |
| **P1** | Fix #4: Missing account resolution | Eliminates 13 buy failures | Medium | pumpswap.rs, pool.rs |
| **P1** | Fix #5: Sell TX robustness | Hardens remaining sell failures | Small | mod.rs |
| **P2** | Fix #6: InvalidBaseMint investigation | Eliminates 4 errors | Small | pumpswap.rs |
| **P2** | Fix #7: ConstraintRaw (covered by #2/#3) | 6 errors | None (auto-fixed) | — |
| **P2** | Fix #8: AccountNotInitialized | Eliminates 4 errors | Small | pumpswap.rs or startup |
| **P2** | Fix #9: Anti-sandwich hardening | Protects profitable buys | Small | mod.rs |
| **P3** | Fix #10: Custom:1 (covered by #1) | 6 errors | None (auto-fixed) | — |

**Estimated total impact:** ~145+ failures eliminated. Expected live success rate: **80-95%** (up from ~20%).

---

## Verification Checklist (Post-Deploy)

### Immediate (First 10 trades)
- [ ] Zero `ConstraintTokenMint` (3012) errors
- [ ] Zero `IncorrectProgramId` errors  
- [ ] Zero `MissingAccount` errors
- [ ] All buy TXs have `buy_state` transitions logged
- [ ] No sell TX fires when buy_state = Failed
- [ ] Pool accounts resolved from getTransaction (not getProgramAccounts)

### 24-Hour
- [ ] Buy landing rate > 80%
- [ ] Sell landing rate > 80% (gated on confirmed buys only)
- [ ] Zero "no pool accounts" warnings for PumpSwap graduations
- [ ] `buy_states` DashMap size stays ≤ max_concurrent (no memory leak)
- [ ] Sandwich rate < 5% of buys (with 10-20% slippage protection)

### 72-Hour
- [ ] Overall TX success rate > 85%
- [ ] Net P&L is positive (assuming alpha exists in the scoring model)
- [ ] No new error codes appearing
- [ ] Circuit breaker trip rate < 1/hour

---

## Appendix: Position Struct Layout — Available Space for buy_confirmed

The `MomentumPosition` struct is 257 bytes with `_pad2` at 39 bytes. All 39 bytes of `_pad2` are allocated:

```
_pad2 layout:
[0..17]  = TopDetector (17 bytes)
[17]     = scaled_in flag (1 byte)
[18..20] = ws_notif_count (u16 LE, 2 bytes)
[20..28] = ws_notif_last_ms (u64 LE, 8 bytes)
[28..36] = tokens_held (u64 LE, 8 bytes)
[36]     = probe_phase (u8, 1 byte)
[37..39] = effective_trail_bps (u16 LE, 2 bytes)
Total:     39 bytes — FULLY ALLOCATED
```

The struct is capped at 320 bytes (5 cache lines) and currently uses 257 bytes. That leaves 63 bytes of headroom if we extend `_pad2` from 39 to 102 bytes.

However, **the recommended approach is NOT to add buy_confirmed to the position struct**. Instead, use a separate `DashMap<[u8;32], BuyState>` as specified in Fix #1. This:
- Avoids struct layout changes
- Allows the async buy task to write back without interior mutability issues
- Keeps the hot-path position struct cache-friendly

---

## Appendix: PumpSwap Buy Instruction Account Layout

From `build_pumpswap_swap_ix()` in pumpswap.rs (buy variant, token_is_base=true):

```
[0]  pool PDA                        (writable)
[1]  user wallet                     (signer)
[2]  global_config                   (readonly)
[3]  base_mint (token)               (readonly)
[4]  quote_mint (WSOL)               (readonly)
[5]  user_base_token_account (ATA)   (writable) — user's token ATA
[6]  user_quote_token_account (ATA)  (writable) — user's WSOL ATA
[7]  pool_base_token_account         (writable) — pool's token vault
[8]  pool_quote_token_account        (writable) — pool's WSOL vault
[9]  protocol_fee_recipient          (writable)
[10] fee_recipient_token_account     (writable) — fee recipient's WSOL ATA
[11] base_token_program              (readonly) — SPL Token (for token)
[12] quote_token_program             (readonly) — SPL Token (for WSOL)
[13] system_program                  (readonly)
[14] associated_token_program        (readonly)
[15] event_authority                 (readonly)
[16] pump_program (self-CPI)         (readonly)
[17] coin_creator_vault_ata          (writable) — MUST NOT BE ZEROED
[18] coin_creator_vault_authority    (readonly) — MUST NOT BE ZEROED
[19] global_volume_accumulator       (readonly) — BUY ONLY
[20] user_volume_accumulator         (writable) — BUY ONLY, may need initialization
[21] fee_config                      (readonly)
[22] fee_program                     (readonly)
[remaining] pool_v2 PDA, optional cashback ATA
```

**Key accounts that cause MissingAccount when zeroed:** [17], [18], [20]

---

*End of Build Spec. Do not implement code changes — this document is for engineering review and planning only.*