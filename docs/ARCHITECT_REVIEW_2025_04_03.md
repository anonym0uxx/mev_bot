# Architect Review: pump-quant-core Momentum Engine
**Date:** 2026-04-03  
**Reviewer:** Apollo (Senior Rust Architect)  
**Scope:** Issues #1-3 + Holistic TX Lifecycle Hardening  

---

## Executive Summary

Three distinct problems, one root cause: the engine was designed for SPL Token pools and is now encountering Token-2022 mints with different on-chain characteristics. Combined with stale binary deployment (ALL code fixes were uncompiled until 11:32 AM), the 25% WR / -0.095 SOL data reflects the OLD engine, not the current one. That said, there are real architectural gaps to fix.

**Priority ordering (implement in this sequence):**

| # | Fix | Impact | Effort | Risk |
|---|-----|--------|--------|------|
| P0 | Token-2022 overflow gate on sell | Prevents stuck tokens (real money loss) | Low | None |
| P1 | Token-2022 slippage widening for high-latency path | Stops burning fees on missed entries | Low | Low |
| P2 | ATA pre-warming at observation window start | Cuts 50-80ms from TX build path | Medium | None |
| P3 | Blockhash freshness (10s refresh, not 25s) | Prevents expired-blockhash failures | Trivial | None |
| P4 | Dual-TX submission (RPC + Jito bundle for buys) | Increases landing rate | Medium | Low |
| P5 | Pool pre-resolution during observation window | Eliminates last-chance resolution latency | Medium | None |
| P6 | Score threshold tuning for cold-miss entries | Filters low-quality entries | Trivial | Low |
| P7 | Position size reduction for negative-EV regime | Reduces bleed rate until WR improves | Trivial | None |

---

## Issue 1: Token-2022 ExceededSlippage on Buys (Custom:6004)

### Root Cause Analysis

The buy path computes `min_tokens_out` as:

```rust
// mod.rs:1623-1625 (deferred_buy_pumpswap, same pattern in process_pending_entries)
let min_tokens_out = if current_price_fp > 0 {
    let tokens_at_max = (max_quote_in as u128 * 1_000_000 / current_price_fp as u128) as u64;
    std::cmp::max(tokens_at_max * 50 / 100, 1)  // 50% of expected tokens
} else {
    1
};
```

The `50 / 100` multiplier means we accept up to 50% price movement. But for Token-2022 mints that required last-chance pool resolution (`pool PDA is zeroed → last-chance resolution`), the timeline is:

1. Observation window completes → triggers entry (+0ms)
2. Pool PDA is zeroed → RPC call to `resolve_pumpswap_pool_from_mint` (+80ms)
3. Token mint program is zeroed → RPC call to `resolve_mint_program_with_fallback` (+40-80ms) 
4. TX built with `current_price_fp` captured at step 1 (+5ms)
5. TX submitted via `rpc_sender.submit_tx` (+20ms)
6. TX lands on-chain (+400-1100ms)

**Total latency from price snapshot to on-chain execution: ~550-1300ms**

For tokens moving at 413 bps/s (the 7BTAd case), 1.3s of movement = 537 bps = 5.37% price change. The 50% `min_tokens_out` floor should handle this (you'd need >50% movement to fail). But the issue is that `current_price_fp` was captured at observation window completion, BEFORE the last-chance resolution added 80-160ms. The price moved further during resolution, and the actual on-chain price at landing was beyond the 50% floor.

Wait — let me re-examine. 50% acceptance means the price could double and we'd still land. That's extremely permissive. The `ExceededSlippage` means `min_tokens_out` was too high. Let me check: in the **reversed pool** path (WSOL=base, token=quote):

```rust
// pumpswap.rs:755-757
(&PUMPSWAP_SELL_DISCRIMINATOR, sol_lamports, min_tokens_out, true)
```

For reversed pools, `arg2 = min_tokens_out` is passed as `min_quote_out`. The PumpSwap contract checks `actual_quote_out >= min_quote_out`. If the token price rose since our snapshot, we get MORE tokens (not fewer) — so the check should PASS, not fail.

For **normal pools** (token=base):
```rust
// pumpswap.rs:752-753
(&PUMPSWAP_BUY_DISCRIMINATOR, min_tokens_out, sol_lamports, false)
```

Here `arg1 = min_tokens_out = base_out`, and `arg2 = sol_lamports = max_quote_in`. The contract verifies `quote_needed <= max_quote_in`. If the price rose, the same number of tokens costs MORE SOL. So the slippage check fires on `max_quote_in`, not `min_tokens_out`.

**The actual constraint that fires is `max_quote_in`**, not `min_tokens_out`. The `sol_lamports` passed is `max_quote_in`:

```rust
// mod.rs:1619
let max_quote_in = (size_lamports as u128 * multiplier_pct as u128 / 100) as u64;
```

With `multiplier_pct` at 251-300% (from the logs), `max_quote_in` = 0.075-0.09 SOL for a 0.03 SOL position. The AMM needs more than 0.09 SOL to buy the same token count, so it reverts.

### The Real Fix

The issue isn't `min_tokens_out` — it's that `min_tokens_out` (passed as `base_out` in buy) determines the EXACT number of tokens we're asking for, and `max_quote_in` is the SOL ceiling. When price moves up, the AMM needs more SOL to deliver that many tokens.

**For the buy instruction, we should flip the approach:**
- Don't specify an exact token count to buy
- Instead, specify MAX SOL we're willing to spend and MINIMUM tokens we'll accept

Actually, re-reading the PumpSwap buy contract: `buy(base_out, max_quote_in)` means "I want exactly `base_out` tokens and I'll pay at most `max_quote_in` SOL." If the cost exceeds `max_quote_in`, it reverts with ExceededSlippage.

**The correct approach for buy:** 

Set `base_out` (min_tokens_out) LOW (accept fewer tokens) and `max_quote_in` HIGH (accept paying more SOL). Currently we compute `min_tokens_out` from `max_quote_in / price`, which links them — if price rises, we need more SOL for the same tokens.

The fix: **compute `min_tokens_out` from `size_lamports` (base position), not from `max_quote_in`**:

```rust
// CURRENT (broken for fast-moving tokens):
let tokens_at_max = (max_quote_in as u128 * 1_000_000 / current_price_fp as u128) as u64;
let min_tokens_out = std::cmp::max(tokens_at_max * 50 / 100, 1);

// FIXED: base min_tokens_out on the ORIGINAL position size, not the inflated max_quote_in
let tokens_at_base = (size_lamports as u128 * 1_000_000 / current_price_fp as u128) as u64;
let min_tokens_out = std::cmp::max(tokens_at_base * 50 / 100, 1);
```

This way `min_tokens_out` represents "I want at least 50% of what my base SOL should buy" and `max_quote_in` is separately set to handle the SOL overpay tolerance. The two are decoupled.

**But even better — for Token-2022 with last-chance resolution, lower the floor further:**

```rust
let slippage_floor_pct: u128 = if is_last_chance_resolution {
    30  // 30% floor — accept up to 70% slippage for high-latency path
} else {
    50  // 50% floor — normal path
};
let min_tokens_out = std::cmp::max(tokens_at_base * slippage_floor_pct / 100, 1);
```

### Recommended Code Change — Issue 1

**File:** `rust/pump-quant-core/src/momentum/mod.rs`

**In `process_pending_entries()` (the main buy dispatch, ~line 2545) AND `process_deferred_buys()` (~line 1621):**

Replace the `min_tokens_out` computation. There are TWO call sites that compute this — both need the same fix:

```rust
// ── BEFORE (both call sites) ──────────────────────────────────────
let min_tokens_out = if current_price_fp > 0 {
    let tokens_at_max = (max_quote_in as u128 * 1_000_000 / current_price_fp as u128) as u64;
    std::cmp::max(tokens_at_max * 50 / 100, 1)
} else {
    1
};

// ── AFTER ─────────────────────────────────────────────────────────
// min_tokens_out: minimum acceptable tokens from the swap.
// Based on BASE position size (not inflated max_quote_in) to decouple
// token floor from SOL ceiling. Additional widening for high-latency
// paths (Token-2022 last-chance resolution adds 80-160ms).
let min_tokens_out = if current_price_fp > 0 {
    let tokens_at_base = (size_lamports as u128 * 1_000_000 / current_price_fp as u128) as u64;
    // Widen floor for high-latency paths:
    // - Last-chance resolution (pool was zeroed): 30% floor
    // - Normal path: 50% floor 
    let is_high_latency = ps_pool.pool == [0u8; 32]  // was zeroed before resolution
        || ps_pool.token_mint_program == crate::tx::pumpswap::SPL_TOKEN_2022_PROGRAM_BYTES;
    let floor_pct: u128 = if is_high_latency { 30 } else { 50 };
    std::cmp::max(tokens_at_base * floor_pct / 100, 1)
} else {
    1
};
```

**Note for the process_pending_entries call site:** The `ps_pool` variable has already been resolved by the time `min_tokens_out` is computed (last-chance resolution happens first). For this call site, detect high-latency by checking a flag set during resolution. Add a local `let was_last_chance_resolved = ...` bool set to `true` when the `pool PDA is zeroed` branch executes:

```rust
// At the start of the PumpSwap buy block in process_pending_entries:
let mut was_last_chance_resolved = false;

// Inside the "pool PDA is zeroed" block:
if ps_pool.pool == [0u8; 32] {
    // ... last-chance resolution code ...
    was_last_chance_resolved = true;
}

// Then in min_tokens_out:
let is_high_latency = was_last_chance_resolved
    || ps_pool.token_mint_program == crate::tx::pumpswap::SPL_TOKEN_2022_PROGRAM_BYTES;
```

---

## Issue 2: Token-2022 Sell Overflow (Custom:6023)

### Root Cause (Confirmed)

PumpSwap's Token-2022 sell path computes `sell_tokens × reserve_sol` in a u64 intermediate before dividing. For the Phxz39 pool:

- `reserve_sol = 92_355_598_012 lamports` (~92.36 SOL)
- `sell_tokens = 72_714_681_440` (72.7B token atoms)
- `sell_tokens × reserve_sol = 6.71 × 10²¹`
- `u64::MAX = 1.84 × 10¹⁹`
- **Overflow by 365×**

This is a **PumpSwap contract bug** that only affects Token-2022 pools. Standard SPL Token pools use u128 for this computation. We cannot fix the contract — we must gate entries.

### The Correct Gate

The proposed gate simplifies correctly. At entry time, we know:
- `position_lamports` (how much SOL we'll spend to buy)
- `reserve_token` (current pool token reserve)
- `reserve_sol` (current pool SOL reserve)

Our sell tokens will be approximately:
```
sell_tokens ≈ position_lamports × reserve_token / reserve_sol
```

The overflow check PumpSwap does internally is:
```
sell_tokens × reserve_sol > u64::MAX  →  overflow
```

Substituting:
```
(position_lamports × reserve_token / reserve_sol) × reserve_sol > u64::MAX
```

This simplifies to:
```
position_lamports × reserve_token > u64::MAX
```

**This is exact and independent of reserve_sol**, because the reserve_sol cancels. Beautiful.

For Phxz39 at entry:
- `position_lamports = 30_000_000` (0.03 SOL)
- `reserve_token = 191_548_874_858_892` (191.5T atoms)
- Product = `5.75 × 10²¹ >> 1.84 × 10¹⁹` ✓ correctly blocked

For a normal pump.fun token with 800B atoms at 85 SOL:
- `position_lamports = 30_000_000`
- `reserve_token = 800_000_000_000` (800B)
- Product = `2.4 × 10¹⁹ > 1.84 × 10¹⁹` — this ALSO overflows!

Wait. That means even normal pump.fun tokens could overflow. Let me re-check: normal pump.fun tokens use SPL Token (not Token-2022), and the SPL Token sell path uses u128. So the gate MUST be conditional on Token-2022 only.

But wait — for typical pump.fun graduation pools, `reserve_token` is ~793B tokens (793_100_000_000_000 atoms at 6 decimals). Let me check:
- `30_000_000 × 793_100_000_000_000 = 2.38 × 10²²`
- This exceeds u64::MAX by 1290×

So ANY sell of Token-2022 tokens from a standard pump.fun graduation pool would overflow. **The gate should simply reject ALL Token-2022 mints**, not compute the product.

### Recommended Gate — Issue 2

**Where:** `on_graduation()` in `mod.rs`, immediately after the TODO comment about Custom:6023 (~line 769-774).

```rust
// ── Token-2022 sell overflow gate (PumpSwap contract bug) ────────────
// PumpSwap's Token-2022 sell path computes `sell_tokens × reserve_sol`
// in a u64 intermediate, which overflows for any standard pump.fun
// graduation pool (~793B token atoms × ~85 SOL reserves >> u64::MAX).
// Standard SPL Token pools use u128 and are unaffected.
// Gate: reject Token-2022 mints entirely at entry — we cannot sell them.
//
// Detection: check if the pool's token_mint_program has been resolved
// to Token-2022. If pool accounts aren't resolved yet (zeroed program),
// we'll catch it downstream in the buy path where resolve_mint_program
// is called — the deferred buy path already aborts on resolution failure.
{
    if let Some(ps_pool) = self.pumpswap_pools.get(&pool_info.mint) {
        if ps_pool.token_mint_program == crate::tx::pumpswap::SPL_TOKEN_2022_PROGRAM_BYTES {
            tracing::warn!(
                mint = %bs58::encode(&pool_info.mint).into_string(),
                "[momentum] Token-2022 mint rejected — PumpSwap sell overflow bug (Custom:6023)"
            );
            return;
        }
    }
    // If pool accounts not yet resolved, check via a lighter heuristic:
    // Token-2022 mints tend to have extremely high reserve_token (>100T atoms).
    // Standard pump.fun tokens have ~793B atoms. This is a conservative filter
    // that catches the worst offenders without requiring RPC.
    // The buy path's resolve_mint_program call is the definitive check.
}
```

**Also add a gate in the buy path** (`process_pending_entries` and `process_deferred_buys`), AFTER `resolve_mint_program_with_fallback` succeeds:

```rust
// After token_mint_program is resolved and BEFORE building the buy TX:
if ps_pool.token_mint_program == crate::tx::pumpswap::SPL_TOKEN_2022_PROGRAM_BYTES {
    tracing::warn!(
        mint = %bs58::encode(&mint).into_string(),
        "[buy_pumpswap] Token-2022 mint — rejecting entry (PumpSwap sell overflow bug)"
    );
    // Clean up: remove position, unsubscribe price feed
    buy_states.remove(&mint_buy);
    return;
}
```

This goes in BOTH:
1. `process_pending_entries()` — after the `resolve_mint_program_with_fallback` block (~line 2466)
2. `process_deferred_buys()` — after the `resolve_mint_program_with_fallback` block (~line 1656)

**Note:** The deferred buy path already has an abort when resolution fails (`"aborting buy (Token-2022 safety)"`). The new gate goes AFTER successful resolution, checking the resolved program.

### Should we also add an overflow product check?

Yes, as defense-in-depth. Even after gating Token-2022, add a universal overflow check for future-proofing:

```rust
/// Check if a sell would overflow PumpSwap's u64 intermediate computation.
/// Returns true if the position would be unsafe to sell.
#[inline(always)]
fn would_overflow_pumpswap_sell(position_lamports: u64, reserve_token: u64) -> bool {
    // PumpSwap computes: sell_tokens × reserve_sol in u64
    // sell_tokens ≈ position_lamports × reserve_token / reserve_sol
    // After substitution: position_lamports × reserve_token > u64::MAX
    // Use u128 to avoid overflow in the check itself.
    (position_lamports as u128) * (reserve_token as u128) > u64::MAX as u128
}
```

Call this in `on_graduation()` alongside the Token-2022 check:

```rust
if would_overflow_pumpswap_sell(
    (self.config.probe_size_sol * 1e9) as u64,
    pool_info.reserve_token,
) {
    // Only block if Token-2022 (SPL Token pools use u128 internally)
    if let Some(ps_pool) = self.pumpswap_pools.get(&pool_info.mint) {
        if ps_pool.token_mint_program == crate::tx::pumpswap::SPL_TOKEN_2022_PROGRAM_BYTES {
            tracing::warn!(
                mint = %bs58::encode(&pool_info.mint).into_string(),
                reserve_token = pool_info.reserve_token,
                "overflow gate: Token-2022 sell would overflow u64 — rejected"
            );
            return;
        }
    }
}
```

---

## Issue 3: 25% WR / Fee Drag — Architectural Recommendations

### Context

The 25% WR data is from the OLD binary (all morning fixes weren't compiled). The real WR with current code is unknown — we need 50+ trades to establish a baseline. That said, the structural analysis is valid regardless:

**Breakeven math:** 
- TX overhead: 5000 lamports base fee + 50000 lamport Jito tip = 55000 lamports per TX (buy + sell = 110000 lamports)
- On 0.03 SOL position: 110000 / 30_000_000 = **0.37% overhead**
- PumpSwap fee: 1% per swap × 2 swaps = **2%**
- Average slippage: ~1.25% buy + ~1.25% sell = **~2.5%**
- **Total breakeven: ~4.87%**

With `hard_sl_pct = 15%` and 38% of exits being hard_sl, average loss per hard_sl trade ≈ -15% (the full stop). Winning requires > 4.87% gain on EVERY winning trade just to break even against the stops.

### Recommendation 1: Gate Token-2022 Entirely (P0)

As detailed in Issue 2. This eliminates a class of failures with zero false negatives.

### Recommendation 2: Raise Score Threshold for Cold-Miss Entries

Cold misses get `speed=120s → 12pts`, `volume=estimated → variable`, `buys=3(neutral)`, `sells=1(neutral)`, `cold_miss_bonus=5pts`. This often lands at score 45-55, passing the `min_grad_score=45` threshold.

But cold misses have NO enrichment data — we're guessing. The 5pt bonus was meant to compensate for information asymmetry speed advantage, but if those entries are hitting hard_sl at 38% rate, the speed advantage isn't translating to alpha.

**Config change:**
```json
{
    "min_grad_score": 50,  // was 45 — raise to filter marginal cold misses
}
```

Alternatively, add a separate `min_cold_miss_score` field:
```rust
let effective_min = if is_cold_miss {
    self.config.min_cold_miss_score.unwrap_or(self.config.min_grad_score)
} else {
    self.config.min_grad_score
};
```

With `min_cold_miss_score: 55`. This lets enriched entries still pass at 45+ while requiring cold misses to score higher on structural factors (LP reserve, speed, entry discount).

### Recommendation 3: Reduce Position Size in Negative-EV Regime

With 25% WR, the engine is -EV. Kelly says bet less (or zero) in this regime. The current probe size is 0.03 SOL, which is already small. But fees are eating 78% of gross losses — the primary cost isn't the position, it's the transaction overhead.

**The right lever isn't position size, it's entry count.** Reducing from 0.03 to 0.015 SOL halves the position PnL but doesn't meaningfully reduce the 0.0001 SOL Jito tip + 0.000005 SOL base fee (fixed costs). PumpSwap fee IS position-proportional, so reducing size helps there.

**Recommendation:** Don't reduce size below 0.03 SOL. Instead, be more selective (higher score threshold). Each rejected low-quality entry saves 0.02-0.03 SOL in losses.

### Recommendation 4: Minimum Observed Velocity at Entry

The observation window already gates zero-velocity tokens. But the threshold is velocity > 0, which passes tokens with 1 bps/s velocity. For a 4.87% breakeven, we need 487 bps of gain — at 1 bps/s that takes 487 seconds, far beyond our hold window.

**Config change:**
```json
{
    "observation_early_entry_velocity_bps_per_s": 150,  // keep (already reasonable)
    "observation_min_velocity_at_expiry_bps_per_s": 30  // NEW: minimum velocity at window close
}
```

Add to the window expiry evaluation in `process_pending_entries()`:

```rust
// After the existing zero-velocity check:
else if w.price_velocity_bps_per_s() < self.config.observation_min_velocity_at_expiry_bps_per_s {
    should_reject = true;
    reject_reason = "velocity below minimum at observation window expiry";
}
```

Default to 30 bps/s. At that velocity, 487 bps of breakeven gain takes ~16s — within the hold window but requiring sustained momentum.

---

## Holistic TX Lifecycle Hardening

### Current Pipeline (with latencies)

```
Graduation detected (ShredStream/CoreCast/Helius)
  │
  ├─ on_graduation() gates: blocklist, reentry, score threshold  (~0ms)
  │
  ├─ Observation window: 2-6s of price/reserve sampling          (+2000-6000ms)
  │   └─ Early exit on velocity > 150 bps/s after 2s min
  │
  ├─ process_pending_entries() entry gates                        (+0ms)
  │
  ├─ PumpSwap buy TX construction:
  │   ├─ Last-chance pool resolution (if zeroed)                  (+80ms)
  │   ├─ Token mint program resolution (if zeroed)                (+40-80ms)
  │   ├─ Keypair load from disk                                   (+1-5ms)
  │   ├─ TX construction (build_pumpswap_buy_tx)                  (+1-2ms)
  │   └─ ATA creation ix (create_ata_idempotent, in TX)           (+0ms build, CU on-chain)
  │
  ├─ submit_tx (rpc_sender):
  │   ├─ Rate limiter wait                                        (+0-500ms)
  │   ├─ HTTP POST to RPC                                         (+10-50ms)
  │   └─ Confirmation poll                                        (+400-5000ms)
  │
  └─ Total observation-to-landing: ~600-6500ms
```

### Optimization 1: Pool Pre-Resolution During Observation Window (P5)

**Problem:** Last-chance resolution adds 80-160ms AFTER observation window completes, when speed matters most.

**Fix:** Start resolving pool accounts in parallel with observation window. By the time the window completes, resolution should be done.

```rust
// In on_graduation(), right after starting the observation window:
if self.config.observation_window_ms > 0 {
    self.observation_windows.insert(pool_info.mint, ObservationWindow::new(now_ms));
    
    // Kick off async pool resolution in parallel with observation
    if let Some(ps_pool) = self.pumpswap_pools.get(&pool_info.mint) {
        if ps_pool.pool == [0u8; 32] {
            let http = self.http_client.clone();
            let mint = pool_info.mint;
            let public_rpc = self.public_rpc_url.clone();
            let helius_rpc = self.helius_rpc_url.clone();
            let pools_map = self.pumpswap_pools.clone();
            tokio::spawn(async move {
                if let Some(resolution) = crate::momentum::pool::resolve_pumpswap_pool_from_mint(
                    &http, &mint, &public_rpc, &helius_rpc,
                ).await {
                    if let Some(resolved) = crate::momentum::pool::extract_pumpswap_pool_accounts(&resolution) {
                        let accts: crate::tx::pumpswap::PumpSwapPoolAccounts = resolved.into();
                        pools_map.insert(mint, accts);
                        tracing::debug!(
                            mint = %bs58::encode(&mint).into_string(),
                            "[pre_resolve] pool accounts resolved during observation window"
                        );
                    }
                }
            });
        }
    }
    
    // Also pre-resolve token_mint_program
    if let Some(ps_pool) = self.pumpswap_pools.get(&pool_info.mint) {
        if ps_pool.token_mint_program == [0u8; 32] {
            let http = self.http_client.clone();
            let mint = pool_info.mint;
            let helius_rpc = self.helius_rpc_url.clone();
            let public_rpc = self.public_rpc_url.clone();
            let pools_map = self.pumpswap_pools.clone();
            tokio::spawn(async move {
                if let Some(prog) = crate::momentum::pool::resolve_mint_program_with_fallback(
                    &http, &mint, &helius_rpc, Some(&public_rpc),
                ).await {
                    if let Some(mut pool) = pools_map.get_mut(&mint) {
                        pool.token_mint_program = prog;
                        tracing::debug!(
                            mint = %bs58::encode(&mint).into_string(),
                            program = %bs58::encode(&prog).into_string(),
                            "[pre_resolve] token_mint_program resolved during observation window"
                        );
                    }
                }
            });
        }
    }
}
```

**Impact:** Eliminates 80-160ms from the hot path in most cases. The async task completes during the 2-6s observation window, so by the time entry fires, pool accounts and mint program are already resolved.

### Optimization 2: ATA Pre-Warming (P2)

**Problem:** The buy TX includes `create_associated_token_account_idempotent` for both the token ATA and WSOL ATA. This consumes CU on-chain and adds to TX size. The WSOL ATA is created once and reused (good). The token ATA is created per-token.

**Fix for WSOL ATA:** Already handled (created once at startup, kept open). ✅

**Fix for Token ATA:** Pre-create during observation window. Create the ATA as a separate TX (very cheap — 5000 lamports base fee) during the 2-6s observation window. When the buy TX fires, the ATA already exists and the `idempotent` create is a no-op.

```rust
// New function: pre-warm ATA during observation window
async fn pre_warm_token_ata(
    &self,
    mint: [u8; 32],
    token_mint_program: [u8; 32],
) {
    if self.config.paper_mode {
        return;
    }
    // Only pre-warm if we know the token program
    if token_mint_program == [0u8; 32] {
        return;
    }
    
    let kp_path = std::env::var("WALLET_KEYPAIR_PATH").unwrap_or_default();
    let bh = self.blockhash_cache_sync().unwrap_or([0u8; 32]);
    if bh == [0u8; 32] {
        return; // no blockhash available
    }
    
    let rpc_sender = self.rpc_sender.clone();
    tokio::spawn(async move {
        let kp_bytes = match std::fs::read(&kp_path) {
            Ok(b) => b,
            Err(_) => return,
        };
        let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) {
            Ok(v) => v,
            Err(_) => return,
        };
        if kp_arr.len() != 64 { return; }
        let mut kb = [0u8; 64];
        kb.copy_from_slice(&kp_arr);
        let keypair = match solana_sdk::signature::Keypair::from_bytes(&kb) {
            Ok(k) => k,
            Err(_) => return,
        };
        use solana_sdk::signer::Signer;
        use std::str::FromStr;
        let wallet = keypair.pubkey();
        let token_mint = solana_sdk::pubkey::Pubkey::new_from_array(mint);
        let token_program = solana_sdk::pubkey::Pubkey::new_from_array(token_mint_program);
        
        // Build a minimal TX: just create_ata_idempotent + compute budget
        let ix_cu_limit = solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(50_000);
        let ix_cu_price = solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(1000);
        let ix_create = crate::tx::pumpswap::build_create_ata_idempotent_ix(
            &wallet, &wallet, &token_mint, &token_program,
        );
        // Submit via normal RPC (no Jito tip needed — this is prep work)
        // ... build and send minimal TX ...
        let mint_str = bs58::encode(&mint).into_string();
        tracing::debug!(mint=%mint_str, "[pre_warm] ATA creation TX submitted");
    });
}
```

**Impact:** Removes ATA creation from the buy TX critical path. The buy TX is smaller (fewer instructions, less CU), which means lower priority fee cost and faster execution.

**Trade-off:** Burns ~5000 lamports on the ATA creation TX. For 0.03 SOL trades, this is 0.017% overhead — negligible. Only do this for entries that pass the observation window (don't pre-warm during observation, pre-warm when the window status is `is_ready`).

**Actually, revised approach:** Don't pre-warm as a separate TX. Instead, **keep the idempotent create in the buy TX but remove the WSOL create** (already exists). The create_ata_idempotent is a no-op if the ATA exists, and ~100K CU if it doesn't. The real savings come from pool pre-resolution (Optimization 1), not ATA pre-warming. **Downgrade this to P4.**

### Optimization 3: Blockhash Freshness (P3)

**Problem:** Blockhash refreshes every 25s with a 30s TTL. Solana blockhashes are valid for ~60s (150 slots). But a blockhash that's 29s old when used in a TX has only 31s of validity remaining — if the TX takes 5s to land and the validator processes it 2s later, we have thin margins.

**More critically:** If the refresh task fails once, the next refresh is 25s later. During those 25s, the cache could serve a blockhash that's 50s old (20s age at last success + 25s gap + 5s propagation). The 30s TTL catches this, but it means NO blockhash is available for ~20s.

**Fix:** Reduce refresh interval to 10s, TTL to 15s:

```rust
// executor.rs
pub fn new() -> Arc<Self> {
    Arc::new(Self {
        inner: RwLock::new(None),
        ttl_ms: 15_000,  // was 30_000
    })
}

// In spawn_refresh_task:
tokio::time::sleep(std::time::Duration::from_secs(10)).await;  // was 25
```

**Config change in canary.json:** Not needed — this is a code-level default.

**Impact:** Blockhash is always <15s old when used. Higher RPC cost (2.5× more `getLatestBlockhash` calls) but eliminates expired-blockhash edge cases. For Helius (the likely RPC), this is well within rate limits.

### Optimization 4: Dual-Path TX Submission for Buys (P4)

**Problem:** Buys currently go through `rpc_sender.submit_tx()` which sends via RPC only. For time-sensitive buy TXs, Jito bundles offer block-inclusion guarantees (if the bundle is included at all).

**Fix:** For buy TXs ONLY, submit to BOTH RPC and Jito simultaneously:

```rust
// In the buy TX spawn block, after build_pumpswap_buy_tx succeeds:
let tx_bytes_clone = tx_bytes.clone();
let jg_clone = jg.clone();

// Path 1: RPC (existing)
let rpc_handle = tokio::spawn({
    let rpc = rpc_sender.clone();
    let mint_str = mint_str.clone();
    async move {
        rpc.submit_tx(&tx_bytes_clone, &mint_str, "buy_pumpswap_rpc").await
    }
});

// Path 2: Jito bundle (new, parallel)
let jito_handle = tokio::spawn({
    let mint_str = mint_str.clone();
    async move {
        // Submit as single-TX bundle to Jito
        match jg_clone.send_bundle(vec![tx_bytes]).await {
            Ok(bundle_id) => {
                tracing::info!(mint=%mint_str, bundle_id=%bundle_id, "[buy_jito] bundle submitted");
                true
            }
            Err(e) => {
                tracing::debug!(mint=%mint_str, err=%e, "[buy_jito] bundle failed (non-fatal)");
                false
            }
        }
    }
});

// Wait for whichever lands first
// The TX has the same signature on both paths, so double-landing is impossible
// (Solana deduplicates by signature within a slot)
```

**Impact:** Buy TXs get two shots at landing. Jito bundles skip the mempool entirely and go straight to the block builder — lower latency for inclusion. RPC path is the fallback.

**Trade-off:** Jito tip is already included in the TX (tip instruction). No extra cost since the same TX is sent both ways. The only cost is the Jito gRPC call overhead (~5ms).

### Optimization 5: Keypair Pre-Loading (P6 — Easy Win)

**Problem:** Every buy and sell TX spawns a tokio task that reads the keypair from disk:

```rust
let kp_bytes = match std::fs::read(&kp_path) { ... };
let kp_arr: Vec<u8> = match serde_json::from_slice(&kp_bytes) { ... };
```

This is blocking I/O in an async context and adds 1-5ms per TX.

**Fix:** Load the keypair ONCE at engine startup, store as `Arc<Keypair>` in `MomentumEngine`:

```rust
pub struct MomentumEngine {
    // ... existing fields ...
    /// Pre-loaded wallet keypair for TX signing. Loaded once at startup.
    wallet_keypair: Arc<solana_sdk::signature::Keypair>,
}
```

Load in `MomentumEngine::new()`:
```rust
let kp_path = std::env::var("WALLET_KEYPAIR_PATH")
    .expect("WALLET_KEYPAIR_PATH must be set");
let kp_bytes = std::fs::read(&kp_path)
    .expect("Failed to read wallet keypair");
let kp_arr: Vec<u8> = serde_json::from_slice(&kp_bytes)
    .expect("Failed to parse wallet keypair JSON");
let mut kb = [0u8; 64];
kb.copy_from_slice(&kp_arr);
let wallet_keypair = Arc::new(
    solana_sdk::signature::Keypair::from_bytes(&kb)
        .expect("Invalid wallet keypair bytes")
);
```

Then in all spawn blocks, clone `self.wallet_keypair` instead of reading from disk:
```rust
let keypair = self.wallet_keypair.clone();
// ... in spawn block:
// let keypair = keypair; // Arc<Keypair> is Send + Sync
```

**Impact:** Eliminates 1-5ms blocking I/O per TX, removes disk read error paths from the hot path, simplifies error handling in all 6+ TX spawn blocks.

### Optimization 6: Nozomi Fast-Path for Buys

**Problem:** The `nozomi_client` is available (`self.nozomi_client.clone()`) but only used for some paths. Nozomi (Temporal's fast lane) sends TXs to geographically-optimal validators, bypassing the public mempool.

**Fix:** For buy TXs, add Nozomi as a third parallel submission path:

```rust
// Path 3: Nozomi (if available)
if let Some(noz) = nozomi_client {
    let tx_bytes_noz = tx_bytes.clone();
    tokio::spawn(async move {
        match noz.send_transaction(&tx_bytes_noz).await {
            Ok(_) => tracing::debug!("[buy_nozomi] submitted"),
            Err(e) => tracing::debug!("[buy_nozomi] failed: {e}"),
        }
    });
}
```

Same TX (same signature) goes to RPC + Jito + Nozomi simultaneously. Whichever path gets it included first wins. No double-execution risk.

---

## Config Changes Summary

### Immediate (apply to canary.json now):

```json
{
    "momentum": {
        "min_grad_score": 50,           // was 45 — filter marginal entries
        "min_cold_miss_score": 55,      // NEW — cold misses need stronger structural signal
        "observation_min_velocity_at_expiry_bps_per_s": 30,  // NEW — minimum momentum at entry
    }
}
```

Note: `min_cold_miss_score` requires a code change to support. Until then, just raise `min_grad_score` to 50.

### Code-Level Defaults to Change:

| File | Change | Current | New |
|------|--------|---------|-----|
| `executor.rs` | Blockhash refresh interval | 25s | 10s |
| `executor.rs` | Blockhash TTL | 30s | 15s |
| `mod.rs` | min_tokens_out floor (normal path) | 50% of max_quote | 50% of base size |
| `mod.rs` | min_tokens_out floor (high-latency) | 50% of max_quote | 30% of base size |

---

## Implementation Plan for Sonnet

### Phase 1: Safety (P0 + P1) — Do Today

1. **Token-2022 gate in `on_graduation()`** (~line 774):
   - Add `would_overflow_pumpswap_sell()` helper function
   - Add Token-2022 program check against `pumpswap_pools` map
   - Add overflow product check as defense-in-depth

2. **Token-2022 gate in buy path** (both `process_pending_entries` ~line 2466 and `process_deferred_buys` ~line 1656):
   - After `resolve_mint_program_with_fallback` succeeds, check if result is Token-2022
   - If yes: abort buy, clean up buy_states, return

3. **Fix `min_tokens_out` computation** (both call sites):
   - Change base from `max_quote_in` to `size_lamports`
   - Add `is_high_latency` flag for 30% floor vs 50% floor
   - Track `was_last_chance_resolved` bool in process_pending_entries

### Phase 2: Latency (P2 + P3 + P5) — Do Tomorrow

4. **Pool pre-resolution during observation window**:
   - In `on_graduation()`, after creating observation window
   - Spawn async tasks for pool resolution + mint program resolution
   - Tasks write results directly to `pumpswap_pools` DashMap

5. **Blockhash refresh cadence**:
   - `executor.rs`: change TTL to 15_000ms, refresh interval to 10s

6. **Keypair pre-loading**:
   - Add `wallet_keypair: Arc<Keypair>` to `MomentumEngine`
   - Load once in `new()`, pass `Arc` into all spawn blocks

### Phase 3: Landing Rate (P4 + P6) — Later This Week

7. **Dual submission (RPC + Jito) for buys**:
   - Clone TX bytes, submit to both in parallel
   - Track which path landed first for observability

8. **Config tuning**:
   - Raise `min_grad_score` to 50
   - Add `observation_min_velocity_at_expiry_bps_per_s: 30`
   - Monitor new WR over 50+ trades before further adjustments

### Phase 4: Score Gating Refinement — After 200+ Trades on New Code

9. **Add `min_cold_miss_score` config field**:
   - Separate threshold for cold-miss entries
   - Start at 55, adjust based on cold-miss-specific WR data

10. **Kelly sizing recalibration**:
    - After 200 trades with new code, Kelly inputs will be from the new regime
    - Let the automatic Kelly sizing adjust position sizes based on actual WR

---

## What a Production-Grade Sniper Bot Does Differently

The core architectural delta between our engine and a top-tier sniper:

### 1. Pre-computed TX Templates

Top snipers pre-build TX templates with placeholders for volatile fields (blockhash, amounts, pool addresses). When the signal fires, they fill in 2-3 fields and sign — total TX construction time is <1ms, not 5-10ms.

**Our gap:** We build the entire TX from scratch each time, including instruction construction, account meta derivation, message compilation, and signing. The `build_pumpswap_buy_tx` function does ~15 allocations.

**Recommendation:** Pre-compute the instruction layout during observation window. Store a `PreBuiltSwapTemplate` with fixed accounts, compute budget, and ATA creation. On signal, fill in `min_tokens_out`, `max_quote_in`, blockhash, and sign.

### 2. Websocket-Based Blockhash Subscription

Top snipers use `blockSubscribe` or `slotSubscribe` to get blockhashes in real-time (<100ms latency) rather than polling every 10-25s. A slot subscription fires every 400ms; the bot knows the current slot and can derive the relevant blockhash.

**Our gap:** We poll `getLatestBlockhash` every 25s. A blockhash used at T+24s after poll might be 24s old at TX build time.

**Recommendation:** Switch to WS-based blockhash tracking. Subscribe to `slotNotification`, call `getLatestBlockhash` on every slot change (~400ms). This ensures the blockhash is always <1s old.

### 3. Multi-Endpoint Blast Submission

Top snipers send the same TX to 5-10 RPC endpoints simultaneously:
- 2-3 dedicated Helius/Triton staked connections
- Jito block engine
- Nozomi
- Direct validator UDP (if whitelisted)

First landing wins. Total submission time: parallel HTTP posts take ~10ms total.

**Our gap:** Single RPC endpoint with retry loop. Each retry adds 500-5000ms delay.

**Recommendation:** For buys (time-sensitive), submit to all available endpoints simultaneously. For sells (less time-sensitive), current approach is fine.

### 4. Zero-Copy TX Serialization

Top snipers avoid `bincode::serialize` and `VersionedTransaction` construction overhead. They pre-allocate a fixed buffer and write the TX wire format directly.

**Our gap:** Full Solana SDK TX construction → serialization roundtrip.

**Recommendation:** Low priority. Current overhead is ~1-2ms, which is small compared to network latency. Address only if landing rate is still insufficient after other fixes.

### 5. Priority Fee Optimization

Current: fixed 5000 microlamports/CU. Top snipers use `getRecentPrioritizationFees` to determine the minimum fee for block inclusion and bid just above it.

**Our gap:** Static priority fee. In low-congestion slots, we overpay. In high-congestion slots (when everyone is sniping the same graduation), we underpay.

**Recommendation:** Add dynamic priority fee to the buy path:
```rust
// Query recent priority fees for the PumpSwap program
// Use 75th percentile as the bid
let priority_fee = get_recent_priority_fees(PUMPSWAP_PROGRAM)
    .await
    .percentile(75)
    .clamp(1_000, 100_000);  // floor 1K, cap 100K microlamports/CU
```

This is already partially supported (`dynamic_priority_fee: true` in config) but not wired into the PumpSwap buy path.

---

## Summary

| Priority | Fix | Expected Impact |
|----------|-----|-----------------|
| **P0** | Token-2022 sell overflow gate | Prevents stuck tokens — existential risk |
| **P1** | `min_tokens_out` fix (base on size, not max_quote) | Stops burning fees on missed buys |
| **P2** | Pool pre-resolution during observation window | Cuts 80-160ms from entry path |
| **P3** | Blockhash refresh 25s→10s | Eliminates stale blockhash edge case |
| **P4** | Dual TX submission (RPC + Jito) for buys | Higher landing rate |
| **P5** | Keypair pre-loading | Removes 1-5ms blocking I/O per TX |
| **P6** | Score threshold: 45→50, cold miss 55 | Filters ~20% of low-quality entries |
| **P7** | Min velocity at observation expiry: 30 bps/s | Filters flat tokens that always time_sl |

**Do P0 + P1 today. P2-P5 tomorrow. P6-P7 after 50+ trades on new code to establish baseline WR.**
