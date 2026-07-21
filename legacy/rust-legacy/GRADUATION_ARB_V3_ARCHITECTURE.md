# Graduation Arb V3 — Bare-Metal Architecture Document

**Author:** Apollo, Master Bare-Metal Rust Solana MEV Architect  
**Date:** 2026-03-30  
**Status:** APPROVED FOR BUILD — Phase 1 Only  
**Test baseline:** 413 tests passing, zero regressions allowed

---

## EXECUTIVE SUMMARY

We have a **working graduation arb engine** (2,672 lines of Rust in `src/arb/`). It detects migrations, resolves Raydium pools via RPC, calculates spreads, and manages paper positions. But it's PAPER-ONLY and missing 5 critical components needed for live operation with competitive edge.

This document specifies **exactly** what to build, in what order, with zero ambiguity. No Phase 2. No nice-to-haves. Just the 5 components that turn our paper engine into the fastest graduation arb bot on Solana.

---

## EXISTING INVENTORY (DO NOT REWRITE)

| File | Lines | Status | Function |
|------|-------|--------|----------|
| `arb/graduation.rs` | 1,686 | ✅ Working | Core engine: config, positions, pool resolution, spread calc, paper logging |
| `arb/grad_dex_backrun.rs` | 447 | ✅ Working | DEX backrun engine (complementary strategy) |
| `arb/dedup.rs` | 281 | ✅ Working | Migration event dedup with TTL |
| `arb/pool_resolver.rs` | 247 | ✅ Working | postTokenBalances vault extraction |
| `persistence/grad_arb_logger.rs` | ~200 | ✅ Working | JSONL paper trade logger |
| `main.rs` | Wired | ✅ Working | GradArbEngine wired into event loop + API |
| `feeds/shredstream.rs` | 1,206 | ✅ Working | gRPC client + Pump.fun TX parser |

**Total existing:** ~4,000+ lines of working, tested code. We are EXTENDING, not rewriting.

---

## 5 COMPONENTS TO BUILD

### Component 1: ShredStream Graduation Detection
**File:** `feeds/shredstream.rs` (modify `parse_pump_transaction()`)  
**Priority:** CRITICAL — this is our 80-200ms speed advantage  
**Estimated lines:** ~80 new lines  

**Problem:** Currently `parse_pump_transaction()` only parses BUY and SELL discriminators. Graduation/migration events have a DIFFERENT discriminator that we're ignoring.

**What to add:**
```rust
// Pump.fun migrate instruction discriminator
// This fires when a token reaches 85 SOL on the bonding curve
const MIGRATE_DISCRIMINATOR: [u8; 8] = [155, 234, 231, 146, 236, 158, 162, 30];

// In parse_pump_transaction(), after checking BUY and SELL:
// Check for MIGRATE discriminator → emit FeedEvent::Migration
```

The migration instruction includes the mint in the account keys. Extract it and emit `FeedEvent::Migration { mint, sig, slot, ts_ms, source: FeedSource::ShredStream }`.

**Integration:** Add `Migration` variant to `FeedEvent` enum in `feeds/mod.rs`. The EventJoiner already forwards all events to the engine_tx channel. In `main.rs`, the event loop dispatches `FeedEvent::Migration` to `grad_arb_engine.on_migration()`.

**Performance target:** Parse migration instruction in <1µs. Zero allocation. Stack only.

### Component 2: Raydium CPMM Swap Math (On-Chain Constant-Product)
**File:** NEW `arb/raydium_math.rs`  
**Priority:** CRITICAL — needed for Brent optimization and live TP/SL  
**Estimated lines:** ~200 lines  

**What to build:** Pure-function Raydium CPMM swap calculator. No RPC, no async. Given (reserve_sol, reserve_token, amount_in, fee_bps) → exact amount_out.

```rust
/// Raydium CPMM constant-product swap: SOL → Token
/// 
/// formula: amount_out = (reserve_token * amount_in_after_fee) / (reserve_sol + amount_in_after_fee)
/// where amount_in_after_fee = amount_in * (10000 - fee_bps) / 10000
///
/// All arithmetic in u128 to prevent overflow. Final result fits u64.
#[inline(always)]
pub fn swap_sol_to_token(
    reserve_sol: u64,      // lamports
    reserve_token: u64,    // atoms  
    amount_in: u64,        // lamports (our SOL)
    fee_bps: u16,          // Raydium CPMM fee (typically 25 = 0.25%)
) -> u64 {
    let amount_in_after_fee = (amount_in as u128) * (10000 - fee_bps as u128) / 10000;
    let numerator = (reserve_token as u128) * amount_in_after_fee;
    let denominator = (reserve_sol as u128) + amount_in_after_fee;
    (numerator / denominator) as u64
}

/// Raydium CPMM constant-product swap: Token → SOL (for exit)
#[inline(always)]
pub fn swap_token_to_sol(
    reserve_sol: u64,
    reserve_token: u64,
    amount_in: u64,        // token atoms
    fee_bps: u16,
) -> u64 {
    let amount_in_after_fee = (amount_in as u128) * (10000 - fee_bps as u128) / 10000;
    let numerator = (reserve_sol as u128) * amount_in_after_fee;
    let denominator = (reserve_token as u128) + amount_in_after_fee;
    (numerator / denominator) as u64
}

/// Compute round-trip PnL for a given amount_in (SOL lamports).
/// Returns net profit/loss in lamports (signed).
#[inline(always)]
pub fn round_trip_pnl(
    reserve_sol: u64,
    reserve_token: u64,
    amount_in: u64,
    fee_bps: u16,
    jito_tip_lamports: u64,
) -> i64 {
    // Buy: SOL → Token (reserves shift)
    let tokens_bought = swap_sol_to_token(reserve_sol, reserve_token, amount_in, fee_bps);
    // New reserves after our buy
    let new_reserve_sol = reserve_sol + amount_in; // simplified (ignoring fee routing)
    let new_reserve_token = reserve_token - tokens_bought;
    // Sell: Token → SOL (on shifted reserves)
    let sol_received = swap_token_to_sol(new_reserve_sol, new_reserve_token, tokens_bought, fee_bps);
    // Net: received - spent - jito_tip
    (sol_received as i64) - (amount_in as i64) - (jito_tip_lamports as i64)
}
```

**Constants:**
```rust
pub const RAYDIUM_CPMM_FEE_BPS: u16 = 25;  // 0.25%
pub const RAYDIUM_AMM_V4_FEE_BPS: u16 = 25; // 0.25%
```

**Tests:** 10+ unit tests covering edge cases (zero reserves, overflow, max u64, fee rounding).

### Component 3: Brent Method Optimal Position Sizing
**File:** NEW `arb/brent_sizing.rs`  
**Priority:** HIGH — replaces fixed `max_sol` with profit-maximizing amount  
**Estimated lines:** ~120 lines  

**What to build:** Brent's method to find the SOL amount that maximizes `round_trip_pnl()`. This is a root-finding algorithm that converges in 5-8 iterations.

```rust
/// Find the SOL amount that maximizes profit for a given Raydium pool state.
///
/// Uses Brent's method on the derivative of round_trip_pnl w.r.t. amount_in.
/// Convergence: typically 5-8 iterations for 1-lamport precision.
///
/// Returns (optimal_amount_lamports, expected_pnl_lamports) or None if no profitable amount exists.
#[inline(never)] // cold path — called once per arb opportunity
pub fn optimal_arb_size(
    reserve_sol: u64,
    reserve_token: u64,
    fee_bps: u16,
    jito_tip_lamports: u64,
    min_amount: u64,       // minimum position (e.g. 0.01 SOL = 10_000_000)
    max_amount: u64,       // maximum position (Kelly cap or bankroll limit)
    epsilon: u64,          // precision (e.g. 1_000_000 = 0.001 SOL)
) -> Option<(u64, i64)>
```

**Integration with Kelly:** The Brent output gives us `optimal_amount`. We then CAP it at `min(brent_optimal, kelly_fraction * bankroll)`. Kelly provides the risk limit; Brent provides the profit-maximizing size within that limit.

### Component 4: Jito Bundle Submission
**File:** NEW `arb/jito_bundle.rs`  
**Priority:** CRITICAL — required for live execution  
**Estimated lines:** ~250 lines  

**What to build:** REST client for Jito Bundle API. Constructs and submits bundles containing our swap transaction.

```rust
/// Jito bundle submission via Block Engine REST API.
/// 
/// Endpoint: https://mainnet.block-engine.jito.wtf/api/v1/bundles
/// Format: POST with JSON body containing base64-encoded serialized transactions.

pub struct JitoBundleSubmitter {
    client: reqwest::Client,
    block_engine_url: String,
    tip_accounts: [Pubkey; 8],
    keypair: Arc<Keypair>,
}

impl JitoBundleSubmitter {
    /// Submit a graduation arb bundle: [buy_tx] with tip.
    /// Returns bundle_id on success.
    pub async fn submit_arb_bundle(
        &self,
        swap_ix: Instruction,      // Raydium swap instruction
        tip_lamports: u64,
        recent_blockhash: Hash,
    ) -> Result<String, BundleError>
    
    /// Submit an exit bundle: [sell_tx] with minimal tip.
    pub async fn submit_exit_bundle(
        &self,
        sell_ix: Instruction,
        recent_blockhash: Hash,
    ) -> Result<String, BundleError>
}
```

**Tip accounts (from competitive analysis):**
```rust
const JITO_TIP_ACCOUNTS: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt", 
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
];
```

**Adaptive tip (from academic research):**
```rust
fn compute_tip(expected_profit: u64, slots_since_graduation: u8) -> u64 {
    let competition_mult: u64 = match slots_since_graduation {
        0 => 1,   // We're first — low tip OK
        1 => 2,   // Others may see — higher tip
        _ => 5,   // Competitive — need to win inclusion
    };
    let base_tip: u64 = 500_000; // 0.0005 SOL
    let max_tip = expected_profit / 10; // Never > 10% of profit
    (base_tip * competition_mult).min(max_tip).max(500_000)
}
```

**Raydium swap instruction builder:** Build the actual Raydium CPMM `swap_base_in` instruction with proper account layout. Requires:
- Pool state account (from pool resolution)
- Authority PDA
- Token vaults (coin_vault, pc_vault from pool resolution)
- User token accounts (our wallet ATAs)
- amount_in, minimum_amount_out (from swap math with slippage)

### Component 5: Nozomi Dual-Submit
**File:** Extend `arb/jito_bundle.rs` with Nozomi fallback  
**Priority:** HIGH — increases landing rate  
**Estimated lines:** ~60 lines  

**What to build:** After submitting to Jito, simultaneously submit the same transaction via Nozomi's fast lane.

```rust
const NOZOMI_TIP_ACCOUNTS: [&str; 17] = [...]; // from competitive analysis

impl JitoBundleSubmitter {
    /// Dual-submit: Jito bundle + Nozomi fast lane.
    /// Returns first successful result.
    pub async fn dual_submit(
        &self,
        swap_ix: Instruction,
        tip_lamports: u64,
        recent_blockhash: Hash,
    ) -> Result<String, BundleError> {
        let (jito_result, nozomi_result) = tokio::join!(
            self.submit_arb_bundle(swap_ix.clone(), tip_lamports, recent_blockhash),
            self.submit_nozomi(swap_ix, recent_blockhash),
        );
        jito_result.or(nozomi_result)
    }
}
```

---

## BUILD ORDER

```
1. Component 2: raydium_math.rs       (pure functions, fully testable, zero deps)
2. Component 3: brent_sizing.rs       (depends on raydium_math)
3. Component 1: ShredStream migration  (extend existing parser)
4. Component 4: jito_bundle.rs        (async, needs reqwest + solana-sdk)  
5. Component 5: Nozomi dual-submit    (extends Component 4)
```

Each component compiles and tests independently. No component depends on a later one.

---

## PERFORMANCE TARGETS

| Operation | Target | Baseline (public bots) |
|-----------|--------|------------------------|
| ShredStream migration parse | <1µs | N/A (they don't have ShredStream) |
| Raydium swap math | <100ns | ~500ns (f64-heavy) |
| Brent optimization (8 iters) | <1µs | ~5µs (golden section, 20 iters) |
| Pool resolution (RPC) | <150ms | ~200ms |
| Jito bundle construction | <5ms | ~10ms |
| Bundle submission (network) | <50ms | ~50ms |
| **TOTAL: shred → bundle submitted** | **<210ms** | **>350ms** |

---

## INTEGRATION INTO EXISTING ENGINE

### Event Flow (updated)

```
ShredStream gRPC
    │
    ├── FeedEvent::Trade → hot_path.on_trade() [existing]
    │
    └── FeedEvent::Migration → [NEW dispatch in main.rs]
            │
            └── tokio::spawn(async {
                    grad_arb_engine.on_migration_v3(mint, sig, slot, ts_ms)
                })
                    │
                    ├── pool_resolution (RPC, <150ms)
                    ├── spread_calc + brent_sizing (<1µs)
                    ├── if profitable: build_swap_ix + dual_submit (<55ms)
                    └── position tracking + TP/SL/MaxHold
```

### Config Changes

Add to `canary.json`:
```json
{
    "graduation_arb_enabled": true,
    "graduation_arb_max_sol": 0.50,
    "graduation_arb_min_spread_pct": 2.0,
    "graduation_arb_tp_pct": 0.05,
    "graduation_arb_sl_pct": 0.02,
    "graduation_arb_max_hold_ms": 5000,
    "graduation_arb_jito_tip_sol": 0.001
}
```

### Paper Mode Behavior

When `paper_mode: true` (default):
- Components 1-3 run live (detection, math, sizing)
- Component 4 constructs the bundle but DOES NOT submit
- Logs the "would-have-submitted" bundle with timing data
- Position management uses pool resolution data for TP/SL

This gives us real performance data without risking SOL.

---

## ZERO-REGRESSION GUARANTEES

1. All 413 existing tests pass
2. Existing bonding curve engine continues running (feeds still active)
3. ShredStream feed continues emitting Trade events (migration detection is additive)
4. No changes to Kelly LUT, Bayesian model, or existing scoring
5. Graduation arb engine is opt-in via `graduation_arb_enabled` config flag
6. New modules compile with `cargo check -p pump-quant-core`

---

## FILE MANIFEST

```
NEW FILES:
  src/arb/raydium_math.rs     (~200 lines) — CPMM swap calculator
  src/arb/brent_sizing.rs     (~120 lines) — Brent method optimizer
  src/arb/jito_bundle.rs      (~310 lines) — Jito + Nozomi submission

MODIFIED FILES:
  src/feeds/mod.rs             — add Migration variant to FeedEvent
  src/feeds/shredstream.rs     — add MIGRATE_DISCRIMINATOR parsing
  src/arb/mod.rs               — register new modules
  src/arb/graduation.rs        — integrate brent_sizing + jito_bundle into on_migration
  src/main.rs                  — dispatch Migration events to grad_arb_engine
  Cargo.toml                   — add `rand` (for tip account selection)
```

**Total new code:** ~630 lines  
**Total modified:** ~100 lines of changes across 6 files  
**Estimated build time:** 2-3 hours for a focused Opus 4.6 engineer

---

## CONSTANTS REFERENCE (from competitive intelligence)

```rust
// Raydium
pub const RAYDIUM_CPMM_PROGRAM: Pubkey = pubkey!("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
pub const RAYDIUM_AMM_V4_PROGRAM: Pubkey = pubkey!("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
pub const RAYDIUM_AUTHORITY: Pubkey = pubkey!("GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL");
pub const CPMM_POOL_DISC: [u8; 8] = [247, 237, 227, 245, 215, 195, 222, 70];
pub const CPMM_FEE_BPS: u16 = 25;

// Pump.fun  
pub const PUMP_PROGRAM: Pubkey = pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
pub const MIGRATE_DISC: [u8; 8] = [155, 234, 231, 146, 236, 158, 162, 30];
pub const BUY_DISC: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
pub const SELL_DISC: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

// Jito Block Engine
pub const JITO_MAINNET_BLOCK_ENGINE: &str = "https://mainnet.block-engine.jito.wtf";
pub const JITO_BUNDLE_ENDPOINT: &str = "/api/v1/bundles";
pub const MIN_JITO_TIP: u64 = 10_000; // 10K lamports minimum

// Nozomi
pub const NOZOMI_RPC_URL: &str = "https://pit-rpc.nozomi.temporal.xyz"; 
```
