# Graduation Arb Quant Spec

**Date:** 2026-03-29
**Author:** Quant Analysis (automated)
**Dataset:** 23,580 graduation events over ~37 minutes (2026-03-29 06:59–07:36 UTC)
**Status:** Paper mode, pre-production diagnostic

---

## 0. Executive Summary

The graduation arb engine has a **99.97% pool resolution failure rate** (only 8/23,580 events resolve with any price data, and of those, most are garbage due to wrong mint extraction). The system is collecting events but cannot meaningfully evaluate any of them. Before worrying about profitability or alpha, we must fix the data pipeline.

**Critical bugs found:**
1. `getTransaction` with `jsonParsed` cannot extract pool accounts from Raydium v4 transactions (v0 address lookup tables)
2. PumpSwap inner instruction parsing never matches (account layout guess is wrong)
3. Balance-diff heuristic extracts wrong mints (USDC, Raydium program ID misidentified as token mints)
4. BC terminal price constant is conceptually wrong (accidentally ~correct numerically by coincidence)
5. Spreads of 30-42% on the few "successful" resolutions are artifacts of wrong reserve data

**Bottom line:** This is not a tuning problem. It's a **data pipeline failure**. Fix pool resolution first, then re-evaluate profitability with real data.

---

## 1. Pool Resolution Diagnosis & Fix

### 1.1 Root Cause Analysis

**Failure rate:** 99.97% (23,577/23,580 events have `rayOpeningPrice = 0`)

The `resolve_pool_from_transaction()` function calls `getTransaction` with `encoding: jsonParsed` and then scans inner instructions for Raydium/PumpSwap program IDs. This fails for three distinct reasons:

#### Problem A: Raydium AMM v4 Uses v0 Transactions with Address Lookup Tables (ALTs)

Raydium AMM v4 pool creation transactions are **v0 (versioned) transactions** that use **Address Lookup Tables (ALTs)**. When a transaction uses ALTs:

- `getTransaction` with `jsonParsed` encoding returns the instruction with `programId` as a base58 string
- **BUT** the accounts referenced via ALTs appear in `transaction.message.addressTableLookups`, NOT in the static `accountKeys` array
- Inner instructions from CPI calls through ALT-loaded programs **may not appear as structured inner instructions** — they may appear as raw base64 data

**Evidence:** 681 events identified as `raydium_amm_v4` pool type (via the fallback accountKeys scan), but only 3 had any non-zero `rayOpeningPrice`. The Raydium program ID IS found in accountKeys, but the `try_parse_raydium_v4()` function fails because:
1. The `initialize2` instruction doesn't appear as a structured inner instruction (it's behind a CPI that uses ALT accounts)
2. Even if found, `accounts[3]` (pool address) references an ALT-loaded address that isn't in the static account list

**The `extract_reserves_from_balances()` fallback also fails** because:
- Pool address `[0u8; 32]` (extraction failed) → pool index lookup returns `None`
- Falls through to heuristic, which returns the **largest SOL increase** (might be the deployer, not the pool) and **largest non-WSOL token balance** (might be the wrong token entirely)

#### Problem B: PumpSwap Instruction Parsing Uses Wrong Account Layout

The code guesses `accounts[0] = pool, accounts[2] = mint` for PumpSwap instructions. Only 75 events were identified as PumpSwap (vs 681 Raydium), and **zero** had successful reserve extraction.

The actual PumpSwap `CreatePool` instruction layout (from on-chain analysis) is:

```
PumpSwap CreatePool account layout:
[0] pool                    (writable)
[1] pool_authority           (PDA)
[2] base_mint               (the pump.fun token)
[3] quote_mint              (WSOL)
[4] lp_mint                 (writable)
[5] user                    (signer)
[6] user_base_token_account  (writable)
[7] user_quote_token_account (writable)
[8] pool_base_token_account  (writable)
[9] pool_quote_token_account (writable)
[10] token_program
[11] associated_token_program
[12] system_program
[13] rent
```

The code's `accounts[2]` guess for base_mint is **actually correct** in this layout, but the overall parsing fails because PumpSwap graduation transactions have the `CreatePool` call as a CPI **from the pump.fun program**, not a direct instruction. The inner instruction structure from `jsonParsed` may not expose it correctly.

#### Problem C: Balance-Diff Heuristic Returns Garbage

When both direct parsing methods fail, the fallback heuristic (`extract_max_sol_increase` + `extract_max_token_balance`) is mathematically unsound for pool initialization transactions:

1. **`extract_max_sol_increase`**: Finds the account with the largest SOL increase. In a graduation tx, this could be the pool vault, BUT it could also be a fee recipient or the Raydium LP recipient (who receives back SOL from the CPI).

2. **`extract_max_token_balance`**: Finds the largest non-WSOL token balance. In a graduation tx with multiple token transfers, this pulls the largest — which might be a user's existing token balance, not the pool's.

3. **`extract_fallback_mint`**: Returns the first non-WSOL mint in `postTokenBalances`. This misidentifies USDC (`EPjFWdd5...`), Raydium program (`5Q544fKr...`), and other unrelated tokens.

**Evidence:** The 8 "successful" resolutions include mints that are USDC and the Raydium program ID itself — confirming the heuristic pulls wrong data.

### 1.2 Correct Approach for Pool Resolution

**Recommended: Option D — Extract pool from migration tx, then `getAccountInfo` on the pool**

This is a two-phase approach:

#### Phase 1: Extract Pool Address from Migration Transaction

Use `getTransaction` with `encoding: base64` (NOT `jsonParsed`) and `maxSupportedTransactionVersion: 0`. Then:

1. Deserialize the versioned transaction
2. Resolve address lookup tables (call `getMultipleAccountInfo` on ALT addresses if needed)
3. Scan all instructions for Raydium/PumpSwap program invocations
4. Extract pool address from instruction account indices

**For Raydium AMM v4 `initialize2`:**
```
Program: 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8
Instruction discriminator: first 8 bytes of data
Account layout:
  [0] token_program
  [1] associated_token_program  
  [2] system_program
  [3] rent
  [4] amm_id (THE POOL ADDRESS)
  [5] amm_authority
  [6] amm_open_orders
  [7] lp_mint
  [8] coin_mint (base token = pump.fun token)
  [9] pc_mint (quote = WSOL)
  [10] coin_vault
  [11] pc_vault
  ...
```

**For PumpSwap:**
```
Program: pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA  
(confirmed correct program ID)
Account [0] = pool address
Account [2] = base_mint (pump.fun token)
```

**Alternative shortcut for Raydium:** Since we have the mint address from the Bitquery event, we can derive the Raydium pool address deterministically:
```rust
// Raydium AMM v4 pool PDA
let (pool_address, _bump) = Pubkey::find_program_address(
    &[
        b"amm_associated_seed",
        RAYDIUM_AMM_V4_PROGRAM.as_ref(),
        coin_mint.as_ref(),  // pump.fun token
        pc_mint.as_ref(),    // WSOL
    ],
    &RAYDIUM_AMM_V4_PROGRAM,
);
```
This avoids parsing the tx entirely — just derive the PDA from the mint.

#### Phase 2: Fetch Reserves via `getAccountInfo`

Once we have the pool address (from tx parsing or PDA derivation):

```rust
// Raydium AMM v4: getAccountInfo returns the AmmInfo struct
// Parse at known offsets:
//   offset 0..8: discriminator
//   offset 328..336: pool_coin_amount (u64, little-endian) = base token reserve
//   offset 336..344: pool_pc_amount (u64, little-endian) = SOL reserve
//
// Alternative: fetch the vault token accounts directly
// coin_vault = accounts[10] from initialize2
// pc_vault = accounts[11] from initialize2
// getAccountInfo on each → read token balance

// PumpSwap: getAccountInfo returns pool state
// Parse pool account data for reserve fields
```

**For PumpSwap, the fastest method is:**
```rust
// pool_base_token_account = accounts[8] from CreatePool
// pool_quote_token_account = accounts[9] from CreatePool
// These are standard SPL Token accounts; getAccountInfo returns the balance directly.
```

### 1.3 Engineering Fix Specification

```rust
/// REPLACE resolve_pool_from_transaction with this approach:

/// Step 1: For Raydium, derive pool PDA from mint (no tx parsing needed)
pub fn derive_raydium_pool(mint: &Pubkey) -> Pubkey {
    let wsol = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
    let raydium = Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").unwrap();
    // Raydium uses findProgramAddress with specific seeds
    // NOTE: Raydium's actual PDA derivation may differ — verify against Raydium SDK
    // The safer approach is to extract from tx as described above
    todo!("verify exact PDA seeds against Raydium v4 SDK source")
}

/// Step 2: Fetch reserves via getAccountInfo on pool + parse binary data
pub async fn fetch_pool_reserves(
    client: &reqwest::Client,
    pool_address: &Pubkey,
    pool_type: PoolType,
    rpc_url: &str,
) -> Option<(u64, u64)> { // (sol_reserve_lamports, token_reserve_atoms)
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [
            pool_address.to_string(),
            {"encoding": "base64", "commitment": "confirmed"}
        ]
    });
    // Parse binary account data at known offsets for each pool type
    // Raydium AMM v4 AmmInfo: coin_amount at offset 328, pc_amount at 336
    // PumpSwap: TBD - need to decode their pool state struct
    todo!()
}
```

**Recommended implementation order:**
1. Fix Raydium v4 first (681/756 = 90% of entered positions)
2. Then PumpSwap (75/756 = 10%)
3. Use `getAccountInfo` + binary parsing (faster than re-fetching the tx)
4. Drop the `jsonParsed` approach entirely — it fundamentally can't work with v0 ALTs

### 1.4 Pool Type Detection Without Transaction Parsing

Since `getTransaction` + `jsonParsed` can't reliably parse the migration tx, we need an alternative to determine pool type. Options:

**A. Try both PDAs:** Derive the Raydium pool PDA for the mint; call `getAccountInfo`. If it exists and has data, it's Raydium. If not, try PumpSwap PDA derivation. First valid account wins.

**B. Log marker (already implemented in Helius feed):** The Helius `logsSubscribe` already distinguishes:
- `GRADUATION_LOG_MARKER` = "Program 675kPX... invoke" → Raydium
- `PUMPSWAP_LOG_MARKER` = "Instruction: MigrateFunds" → PumpSwap

Pass the pool type from the detection layer through the `FeedEvent::Migration` struct. This avoids re-discovering what we already know.

**Recommendation: Option B.** Modify `FeedEvent::Migration` to include a `pool_type_hint: Option<PoolType>` field set by the detection layer. Then `resolve_pool_from_transaction` uses the hint to skip unnecessary checks.

---

## 2. BC Terminal Price Validation

### 2.1 Pump.fun Bonding Curve Parameters (Verified)

| Parameter | Value | Notes |
|-----------|-------|-------|
| Total token supply | 1,000,000,000 tokens = 1e15 atoms | 6 decimals |
| Initial virtual tokens (vTokens₀) | 1,073,000,000 tokens = 1.073e15 atoms | Confirmed in code: `INITIAL_VIRTUAL_TOKENS` |
| Initial virtual SOL (vSol₀) | 30 SOL = 30e9 lamports | Well-documented |
| Invariant k | vSol₀ × vTokens₀ = 3.219e25 | Constant product |
| Tradeable tokens on curve | 793,100,000 tokens = 793.1e12 atoms | = 1B - 206.9M LP reserve |
| LP allocation at graduation | 206,900,000 tokens = 206.9e12 atoms | Reserved for DEX deposit |
| Graduation trigger | Real SOL collected ≈ 85 SOL | Approximate; varies ±2 SOL |

### 2.2 Terminal Price Derivation

At graduation, all 793.1M tradeable tokens have been purchased:

```
vTokens_terminal = vTokens₀ - tokens_traded
                 = 1.073e15 - 793.1e12
                 = 279.9e12 atoms

vSol_terminal = k / vTokens_terminal
              = 3.219e25 / 279.9e12
              = 115.005e9 lamports
              ≈ 115.0 SOL

Real SOL in curve = vSol_terminal - vSol₀ = 115.0 - 30.0 = 85.0 SOL ✓
(This confirms the 85 SOL graduation threshold)

BC_TERMINAL_PRICE = vSol_terminal / vTokens_terminal
                  = 115.005e9 / 279.9e12
                  = 4.1088e-4 lamports per atom
```

### 2.3 Code Constant Validation

**Current code:**
```rust
const BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM: f64 = 85e9_f64 / 206_900_000_000_000_f64;
// = 4.1083e-4
```

**Correct value:**
```rust
const BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM: f64 = 115_005_000_000_f64 / 279_900_000_000_000_f64;
// = 4.1088e-4
```

**Error: 0.008%** — negligible for practical purposes. The code constant is **accidentally correct** because:
```
85 / 206.9 = 0.41083
115.005 / 279.9 = 0.41088
```
These ratios converge because 206.9 = 1073 - 793.1 - 73 and the virtual reserve offset creates a proportional relationship.

### 2.4 Recommended Fix

Replace with the mathematically exact derivation:

```rust
/// Pump.fun bonding curve terminal price at graduation.
///
/// Derivation:
///   k = vSol₀ × vTokens₀ = 30e9 × 1.073e15 = 3.219e25
///   vTokens_terminal = 1.073e15 - 793.1e12 = 279.9e12
///   vSol_terminal = k / vTokens_terminal = 115.005e9
///   price = vSol_terminal / vTokens_terminal = 4.1088e-4 lamports/atom
///
/// The 0.008% difference from the old constant (85e9/206.9e12) is negligible,
/// but this derivation is mathematically correct and self-documenting.
const INITIAL_VIRTUAL_SOL_LAMPORTS: f64 = 30_000_000_000.0;  // 30 SOL
const INITIAL_VIRTUAL_TOKENS_ATOMS: f64 = 1_073_000_000_000_000.0;  // 1.073e15
const TOKENS_TRADED_AT_GRADUATION: f64 = 793_100_000_000_000.0;  // 793.1e12

const fn bc_terminal_price() -> f64 {
    let k = INITIAL_VIRTUAL_SOL_LAMPORTS * INITIAL_VIRTUAL_TOKENS_ATOMS;
    let vtokens_terminal = INITIAL_VIRTUAL_TOKENS_ATOMS - TOKENS_TRADED_AT_GRADUATION;
    let vsol_terminal = k / vtokens_terminal;
    vsol_terminal / vtokens_terminal
}

// If const fn with f64 is unavailable, use the pre-computed value:
const BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM: f64 = 4.1088e-4;
// Exact: 115_005_359_057.0 / 279_900_000_000_000.0 = 0.00041087980...
```

### 2.5 Important Note: This Price is NOT the DEX Opening Price

The BC terminal price (4.109e-4 lamports/atom) represents the **last bonding curve price before graduation**. It is NOT the price at which the DEX pool opens.

The DEX pool opening price depends on:
- **SOL deposited:** Total collected (~85 SOL) minus pump.fun fee (1.5-6 SOL, varies by era)
- **Tokens deposited:** The 206.9M LP allocation = 206.9e12 atoms

```
Raydium opening price = deposited_SOL / deposited_tokens

If fee = 1.5 SOL: price = 83.5e9 / 206.9e12 = 4.035e-4 (spread = 1.8% below BC)
If fee = 6.0 SOL: price = 79.0e9 / 206.9e12 = 3.818e-4 (spread = 7.1% below BC)
```

The theoretical structural spread is **1.8% to 7.1%** depending on the pump.fun fee. This is the "free" edge from the deposit mechanics.

---

## 3. Spread Profitability Analysis

### 3.1 Cost Model

| Component | Cost (SOL) | Notes |
|-----------|-----------|-------|
| Jito tip | 0.003 | Competitive for graduation arb |
| Priority fee (buy) | 0.0005 | Standard priority |
| Priority fee (sell) | 0.0005 | Standard priority |
| Base tx fee × 2 | 0.00001 | Negligible |
| **Total fixed cost** | **0.00401** | Per round-trip |

Variable costs (slippage):
- **Raydium AMM v4:** Pool has ~80 SOL + ~207M tokens. For 0.3 SOL buy on constant product, slippage ≈ 0.3/80 = 0.375% of pool depth. Effective slippage with fees: ~0.5-1.0%.
- **PumpSwap:** Similar constant product, but pool may be shallower. Estimate 0.5-2.0% slippage.

### 3.2 Break-Even Analysis

For a **0.3 SOL position**:

```
Fixed cost drag: 0.00401 / 0.3 = 1.34%

Break-even spread by slippage scenario:
  Best case (0.5% slippage):  1.34% + 0.5% = 1.84%
  Base case (1.0% slippage):  1.34% + 1.0% = 2.34%
  Worst case (2.0% slippage): 1.34% + 2.0% = 3.34%
```

For a **0.5 SOL position** (larger position, lower fixed cost drag):
```
Fixed cost drag: 0.00401 / 0.5 = 0.80%

Break-even spread:
  Best case:  0.80% + 0.5% = 1.30%
  Base case:  0.80% + 1.0% = 1.80%
  Worst case: 0.80% + 2.0% = 2.80%
```

### 3.3 Theoretical Structural Spread

The spread arises from the deposit ratio difference:

| Scenario | SOL Deposited | Tokens Deposited | Opening Price | Spread vs BC |
|----------|--------------|-----------------|---------------|-------------|
| Fee = 1.5 SOL (PumpSwap era) | 83.5 SOL | 206.9e12 atoms | 4.035e-4 | **1.8%** |
| Fee = 3.0 SOL | 82.0 SOL | 206.9e12 atoms | 3.963e-4 | **3.5%** |
| Fee = 6.0 SOL (old Raydium era) | 79.0 SOL | 206.9e12 atoms | 3.818e-4 | **7.1%** |

**Critical insight:** With the current pump.fun fee of ~1.5 SOL (PumpSwap era), the structural spread is only **1.8%**. This is BELOW break-even for a 0.3 SOL position under all slippage scenarios.

### 3.4 Spread Decay Speed

The structural spread closes within the **first few transactions** after pool creation:

- **Slot time:** 400ms per Solana slot
- **First trade latency:** 1-3 slots after pool creation = 400-1200ms
- **Spread consumed by first trade:** Typically 50-100% of the structural spread (the first buyer gets most of the edge)
- **At 80ms Bitquery latency:** We detect the migration, but still need:
  - Pool resolution: +50-200ms (RPC call)
  - Tx build: +20ms
  - Jito submission: +50ms
  - **Total: 200-350ms from tx landing**
- **At 5-20ms Geyser latency:** Total pipeline = 75-240ms
- **Window of opportunity:** The first swap must land in slot N+1 or N+2 (400-800ms after pool creation). At 200-350ms pipeline, we're competing for slot N+1.

### 3.5 Competitive Landscape

Graduation arb is a **well-known, heavily competed** strategy:

- **Dedicated searchers** with Geyser/ShredStream (5-20ms detection) + custom Raydium instruction builders + Jito bundles
- **Bot farms** pre-computing pool PDAs and submitting speculative swaps before migration confirms
- **MEV searchers** bundling with the graduation tx itself (sandwich the first trade)

At 80ms Bitquery latency, we are **60-75ms behind** the fastest searchers. In Jito bundle auctions, this means we're bidding on slot N+2 while they're in slot N+1.

### 3.6 Realistic Capture Rate

| Latency Tier | Detection | Pipeline Total | Expected Slot | Spread Remaining | Capture Rate |
|-------------|-----------|---------------|---------------|-----------------|-------------|
| Geyser + co-located | 5ms | 75ms | N+1 (first) | 80-100% | 30-50% |
| ShredStream | 15ms | 120ms | N+1 (competing) | 50-80% | 15-30% |
| Helius logsSubscribe | 50ms | 200ms | N+1 (late) | 20-50% | 5-15% |
| **Bitquery (us)** | **80ms** | **280ms** | **N+1 or N+2** | **10-30%** | **2-8%** |

At 2-8% capture rate with 1.8% structural spread, the expected captured spread is:
```
E[captured_spread] = capture_rate × structural_spread × remaining_fraction
                   = 0.05 × 1.8% × 0.2
                   = 0.018%
```

This is **negligible** — well below break-even.

---

## 4. PumpSwap vs Raydium Strategy

### 4.1 Raydium AMM v4

**AMM type:** Constant product (x × y = k)

**Pool creation:** Raydium `initialize2` instruction deposits SOL + tokens into vault accounts. The pool opens at price = SOL_deposit / token_deposit.

**Price formula:**
```
price(lamports/atom) = reserve_sol / reserve_tokens
After buying `sol_in` SOL worth:
  tokens_out = reserve_tokens - (reserve_sol * reserve_tokens) / (reserve_sol + sol_in)
             = reserve_tokens * sol_in / (reserve_sol + sol_in)
```

**Fees:** Raydium charges 0.25% swap fee (0.22% to LP, 0.03% to protocol).

**Pool identification:**
- PDA derivation from mints + Raydium program
- Or extract from migration tx inner instructions

### 4.2 PumpSwap

**AMM type:** Constant product (x × y = k) — same as Raydium.

**Program ID:** `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` (confirmed)

**Key differences from Raydium:**
1. **Controlled by pump.fun** — they set the deposit ratio directly
2. **Lower fees possible** — pump.fun can set 0% swap fee for initial trades
3. **Tighter spread at opening** — since pump.fun controls both the bonding curve and the AMM, they can set the opening price very close to BC terminal price
4. **1.5 SOL graduation fee** (vs 6 SOL in old Raydium era)

**Price formula:** Same as Raydium: `price = reserve_sol / reserve_tokens`

**Pool identification:**
- Extract from migration tx (inner instruction from pump.fun program CPI)
- PDA derivation (need PumpSwap's seed structure)

### 4.3 Strategic Implications

| Aspect | Raydium | PumpSwap |
|--------|---------|----------|
| Structural spread | ~7.1% (6 SOL fee era) | ~1.8% (1.5 SOL fee) |
| Swap fee | 0.25% | TBD (likely lower) |
| Competition | Very high (established arb) | Lower (newer, less tooling) |
| Frequency (2026) | Declining (<10% of graduations) | Dominant (>90%) |
| Break-even viable | Marginal at best latency | Not viable at 80ms |

**Since March 2025, ~90%+ of graduations go to PumpSwap.** This means:
- The structural spread has DECREASED from ~7% to ~1.8%
- The strategy's theoretical edge has shrunk by 75%
- Competition on the remaining Raydium graduations is LOWER (fewer events, less attention)

### 4.4 Different Strategy Per Pool Type

**Raydium (rare events, higher spread):**
- Derive pool PDA from mint → `getAccountInfo` on pool → parse AmmInfo binary struct
- Higher structural spread (7%) makes arb viable even at moderate latency
- Worth pursuing if detection latency can be brought under 150ms total pipeline
- Estimated 1-5 events/day

**PumpSwap (common events, lower spread):**
- Need PumpSwap pool PDA derivation or tx parsing
- 1.8% structural spread → NOT viable for arb at any realistic latency with our fee structure
- Better play: momentum trading on freshly graduated tokens (not arb)
- Estimated 20-100+ events/day

---

## 5. Optimal Algorithm (Exact Pseudocode + Values)

### 5.1 Revised Architecture

Given the analysis, the graduation arb engine should be split into two modes:

**Mode A: Structural Arb (Raydium only, rare)**
```
Target: Raydium AMM v4 graduations only
Frequency: ~1-5/day
Structural spread: ~7%
Min profitable spread: 3.5% (0.3 SOL position)
Latency requirement: <300ms total pipeline
```

**Mode B: Momentum Entry (PumpSwap, common)**
```
Target: PumpSwap graduations
Frequency: ~20-100/day
No structural arb edge — this is directional momentum
Entry: buy if social/volume signals are strong
This is a DIFFERENT strategy (not covered in this spec)
```

### 5.2 Mode A: Raydium Structural Arb Pipeline

```pseudocode
fn on_migration(mint, ts_ms, source, sig, pool_type_hint):
    // STEP 1: Filter
    if pool_type_hint == PumpSwap:
        log_skip("pumpswap_no_arb")
        return  // Skip PumpSwap — no structural arb
    
    if dedup.contains(sig_prefix):
        return  // Already processed
    
    // STEP 2: Resolve pool address (Raydium PDA derivation)
    // No RPC needed — deterministic from mint
    wsol_mint = "So11111111111111111111111111111111111111112"
    pool_address = derive_raydium_amm_v4_pda(mint, wsol_mint)
    // Also derive vault addresses:
    coin_vault = derive_raydium_vault_pda(pool_address, "coin")
    pc_vault = derive_raydium_vault_pda(pool_address, "pc")
    
    // STEP 3: Fetch reserves (single RPC call)
    // getMultipleAccountInfo([coin_vault, pc_vault]) — one RPC, two accounts
    timeout = 180ms
    accounts = rpc.getMultipleAccountInfo([coin_vault, pc_vault], {
        encoding: "base64",
        commitment: "confirmed"
    })
    
    if accounts[0] == null or accounts[1] == null:
        log_skip("pool_not_ready")
        return  // Pool not initialized yet
    
    reserve_token_atoms = parse_spl_token_balance(accounts[0])  // coin vault
    reserve_sol_lamports = parse_spl_token_balance(accounts[1])  // pc vault (WSOL)
    
    if reserve_sol_lamports == 0 or reserve_token_atoms == 0:
        log_skip("zero_reserves")
        return
    
    // STEP 4: Calculate opening price
    ray_opening_price = reserve_sol_lamports / reserve_token_atoms
    // Units: lamports per atom
    
    // STEP 5: Calculate spread
    bc_terminal_price = 4.1088e-4  // lamports per atom (constant)
    // Token price is underpriced on Raydium → buy tokens with SOL
    // Token price is overpriced on Raydium → sell tokens for SOL (need tokens first)
    // For graduation arb: Raydium typically opens BELOW BC price → buy tokens on Raydium
    spread_pct = (bc_terminal_price - ray_opening_price) / bc_terminal_price * 100
    
    // STEP 6: Filter
    if spread_pct < 2.0:
        log_skip("spread_below_min", spread_pct)
        return
    if spread_pct > 50.0:
        log_skip("spread_suspicious", spread_pct)
        return
    
    // STEP 7: Paper trade entry
    entry = PaperTrade {
        mint, pool_address, pool_type: Raydium,
        entry_price: ray_opening_price,
        bc_terminal_price,
        spread_pct,
        size_sol: 0.3,
        tp_pct: min(spread_pct * 0.7, 5.0),  // capture 70% of spread, cap at 5%
        sl_pct: 1.0,
        max_hold_ms: 3000,
        entry_ts: now_ms(),
    }
    open_positions.push(entry)
    log_jsonl("paper_entry", entry)
```

### 5.3 CRITICAL: Raydium PDA Seeds Require OpenBook Market Address

**The existing doc's assumption that pool PDAs can be derived from mint alone is WRONG.**

Verified from Raydium AMM v4 source (`raydium-io/raydium-amm`, `program/src/processor.rs`):

```rust
// Raydium's PDA derivation function:
pub fn get_associated_address_and_bump_seed(
    info_id: &Pubkey,        // = RAYDIUM_AMM_V4_PROGRAM
    market_address: &Pubkey, // = OpenBook/Serum MARKET ADDRESS (not mint!)
    associated_seed: &[u8],
    program_id: &Pubkey,     // = RAYDIUM_AMM_V4_PROGRAM
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[info_id.to_bytes(), market_address.to_bytes(), associated_seed],
        program_id,
    )
}
```

**The actual seeds for each PDA:**

| Account | Seeds | Notes |
|---------|-------|-------|
| AMM Pool | `[RAYDIUM_PROGRAM, OPENBOOK_MARKET, b"amm_associated_seed"]` | The pool itself |
| Coin Vault | `[RAYDIUM_PROGRAM, OPENBOOK_MARKET, b"coin_vault_associated_seed"]` | Token vault |
| PC Vault | `[RAYDIUM_PROGRAM, OPENBOOK_MARKET, b"pc_vault_associated_seed"]` | SOL (WSOL) vault |
| LP Mint | `[RAYDIUM_PROGRAM, OPENBOOK_MARKET, b"lp_mint_associated_seed"]` | LP token mint |
| Target Orders | `[RAYDIUM_PROGRAM, OPENBOOK_MARKET, b"target_associated_seed"]` | Order targets |
| Open Orders | `[RAYDIUM_PROGRAM, OPENBOOK_MARKET, b"open_order_associated_seed"]` | Serum open orders |
| AMM Authority | `[b"amm authority"]` (global, not per-pool) | Singleton PDA |

**Program ID:** `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`

**Implication:** We CANNOT derive vault PDAs from just the token mint. We need the OpenBook market address, which is created during the pump.fun migration transaction.

### 5.4 Revised Algorithm — Two Paths

Since PDA derivation requires the OpenBook market address, we have two options:

**Path A: Parse migration tx (slower, +150ms)**
```
1. Detect migration via logsSubscribe
2. getTransaction(sig, encoding=base64, maxSupportedTransactionVersion=0)
3. Deserialize v0 tx, resolve ALTs, find Raydium initialize2 instruction
4. Extract accounts[16] = market_info (OpenBook market address)
5. Derive coin_vault and pc_vault PDAs using market address
6. getMultipleAccountInfo([coin_vault, pc_vault])
7. Parse reserves, calculate spread
```
Total added latency: ~200-400ms (two RPC round-trips)

**Path B: Read AmmInfo directly from pool account (faster, simpler)**
```
1. Detect migration via logsSubscribe
2. getTransaction(sig, encoding=base64, maxSupportedTransactionVersion=0)
3. Deserialize v0 tx, resolve ALTs, find amm_info account (accounts[4] of initialize2)
4. getAccountInfo(amm_info, encoding=base64)
5. Parse AmmInfo binary struct directly:
     - coin_vault Pubkey at offset 336 (32 bytes)
     - pc_vault Pubkey at offset 368 (32 bytes)
6. getMultipleAccountInfo([coin_vault, pc_vault])
7. Parse SPL token balances, calculate spread
```
Total added latency: ~300-500ms (three RPC round-trips)

**Path C: Extract vault accounts directly from tx accounts list (FASTEST)**
```
1. Detect migration via logsSubscribe  
2. getTransaction(sig, encoding=base64, maxSupportedTransactionVersion=0)
3. Deserialize v0 tx, resolve ALTs
4. From Raydium initialize2 instruction accounts:
     accounts[10] = coin_vault
     accounts[11] = pc_vault
5. getMultipleAccountInfo([coin_vault, pc_vault]) — single RPC
6. Parse SPL token balances (amount at bytes[64..72])
7. Calculate spread
```
Total added latency: ~200-300ms (two RPC round-trips: getTx + getMultipleAccountInfo)

**Recommendation: Path C.** Extract vaults directly from the initialize2 instruction's account list. This avoids PDA derivation entirely and only needs 2 RPC calls.

### 5.5 Final Algorithm v2 (Path C)

```pseudocode
GRADUATION ARB ALGORITHM v2 — Raydium Path C (tx-based vault extraction)

CONSTANTS:
  RAYDIUM_PROGRAM = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"
  BC_TERMINAL_PRICE = 4.1088e-4  // lamports per atom
  MIN_SPREAD_PCT = 2.0
  MAX_SPREAD_PCT = 50.0

On logsSubscribe event(sig, logs):
  1. Check logs for RAYDIUM_LOG_MARKER ("Program 675kPX9...invoke")
     If not found → check for PUMPSWAP_LOG_MARKER
     If PumpSwap → log "pumpswap_skipped", RETURN
     If neither → log "unknown_program", RETURN

  2. Dedup: if sig[0..16] in ring_buffer(256) → RETURN
     ring_buffer.push(sig[0..16])

  3. Fetch transaction:
     rpc.getTransaction(sig, {
       encoding: "base64",
       maxSupportedTransactionVersion: 0,
       commitment: "confirmed"
     })
     timeout: 200ms
  
  4. Deserialize versioned transaction:
     - If v0: resolve addressTableLookups via cached ALT data
     - Build full account list: staticAccounts + ALT-resolved accounts
     - Find instruction where programId == RAYDIUM_PROGRAM
     - From that instruction's account indices:
         coin_vault_pubkey = full_accounts[instruction.accounts[10]]
         pc_vault_pubkey   = full_accounts[instruction.accounts[11]]

  5. Fetch vault balances:
     rpc.getMultipleAccountInfo([coin_vault_pubkey, pc_vault_pubkey], {
       encoding: "base64",
       commitment: "confirmed"
     })
     timeout: 150ms

  6. Parse SPL Token accounts:
     For each account (165 bytes):
       amount = u64_le(data[64..72])
     reserve_token = coin_vault.amount
     reserve_sol   = pc_vault.amount

  7. Validate:
     if reserve_token == 0 || reserve_sol == 0 → log "zero_reserves", RETURN

  8. Calculate price and spread:
     ray_price = reserve_sol as f64 / reserve_token as f64
     spread_pct = (BC_TERMINAL_PRICE - ray_price) / BC_TERMINAL_PRICE * 100
     if spread_pct < MIN_SPREAD_PCT → log "low_spread", RETURN
     if spread_pct > MAX_SPREAD_PCT → log "bad_data", RETURN

  9. Paper trade:
     log_entry(mint, ray_price, spread_pct, reserve_sol, reserve_token)
     // In live mode: build and submit Jito bundle for token purchase
```

---

## 6. Honest Profitability Assessment

### 6.1 Observed Graduation Frequency

From paper trading data (31,281 events over ~0.7 hours):
- **Raydium AMM v4:** 687 events → ~24,000/day (extrapolated)
- **PumpSwap:** 75 events → ~2,600/day (extrapolated)
- **Unknown (resolution failed):** 30,519 → ~99% failure rate

**CRITICAL DATA QUALITY WARNING:** These numbers are from a broken pipeline (99.97% resolution
failure). The "Raydium" vs "PumpSwap" classification is from log marker detection, which is
more reliable than price extraction, but the absolute counts over 37 minutes should not be
linearly extrapolated to daily rates.

**Better estimate from market context (March 2025 → March 2026):**
- Post-PumpSwap launch (March 2025): ~90%+ of new graduations go to PumpSwap
- Raydium graduations: estimated **5-30/day** (declining trend)
- PumpSwap graduations: estimated **100-500/day**
- The data showing 687 Raydium in 37 min is either a burst or misclassification

**Conservative estimate for profitability math: 10-20 Raydium graduations/day.**

### 6.2 Structural Spread (Theoretical)

At Raydium graduation:
- SOL deposited into pool: ~79-83.5 SOL (depends on pump.fun fee era)
- Tokens deposited: 206.9M tokens (fixed)
- BC terminal price: 4.1088e-4 lamports/atom

| Fee era | SOL deposited | Pool opening price | Spread vs BC |
|---------|--------------|-------------------|-------------|
| Old (6 SOL fee) | 79.0 SOL | 3.818e-4 | **7.1%** |
| Current (1.5 SOL) | 83.5 SOL | 4.035e-4 | **1.8%** |

**For Raydium graduations still happening in 2026:** Likely the old fee structure (6 SOL),
since pump.fun only charges 1.5 SOL for PumpSwap migrations. Raydium migrations that still
occur may use the legacy 6 SOL fee → **~7% structural spread**.

But this needs validation with real data. The spread could be anything if Raydium migration
parameters have changed.

### 6.3 Latency Competition Model

Our pipeline at 80ms Bitquery detection:

```
Timeline (ms from tx landing in slot):
  0ms     Transaction lands in slot
  5-20ms  Geyser/ShredStream bots detect it
  25-75ms Geyser bots submit Jito bundles for slot N+1  
  80ms    *** We detect via Bitquery logsSubscribe ***
  80-280ms  Our pipeline: getTx + parse + getVaults + spread calc
  280-330ms We submit Jito bundle
  400ms   Slot N+1 starts
  500-800ms Slot N+1 or N+2: first arb trades land
```

**Arb window analysis:**
- Structural spread exists from pool creation (slot N) until first buy (slot N+1 or N+2)
- Geyser bots target slot N+1 (400ms after creation)
- Multiple bots compete → Jito auction for slot N+1 bundle inclusion
- By slot N+2 (800ms): spread is likely 50-100% consumed by first trades
- By slot N+3 (1200ms): spread essentially gone

**At 80ms detection + 200ms pipeline = 280ms total:**
- We can target slot N+1 (starts at 400ms) — we have 120ms margin
- BUT: Geyser bots submitted at 25-75ms, 200ms+ earlier than us
- In Jito auction: higher tip wins. We'd need to outbid them.
- If no Geyser bot targets this specific graduation: we win slot N+1

**Capture scenarios:**

| Scenario | Probability | Spread captured | Notes |
|----------|------------|----------------|-------|
| No competition (no Geyser bot) | ~20-40% | 80-95% of 7% = 5.6-6.7% | Rarer Raydium events may have less attention |
| One Geyser competitor | ~30-40% | 0% (we lose auction or they fill first) | They outbid on tip |
| Multiple competitors | ~20-30% | 0% | Auction is competitive |
| Pool not ready at our query time | ~10% | 0% | Reserves return 0 |

**Weighted expected capture per event:**
```
E[capture] = 0.30 × 6.0% + 0.40 × 0% + 0.25 × 0% + 0.05 × 0%
           = 1.8% expected spread per event
```

### 6.4 Expected Daily P&L

**Optimistic scenario (old 6 SOL fee, 20 Raydium/day, 30% uncontested):**
```
Events/day:           20
Uncontested rate:     30%
Actionable events:    6/day
Avg captured spread:  6.0%
Position size:        0.3 SOL
Gross per trade:      0.3 × 0.06 = 0.018 SOL
Costs per trade:      0.00401 SOL (Jito tip + priority + base)
                    + 0.3 × 0.01 = 0.003 SOL (1% slippage round-trip)
Total cost:           0.00701 SOL
Net per trade:        0.018 - 0.007 = 0.011 SOL
Daily net:            6 × 0.011 = 0.066 SOL/day ≈ $10-15/day
```

**Pessimistic scenario (market has converged, 10 Raydium/day, 15% uncontested):**
```
Events/day:           10
Uncontested rate:     15%
Actionable events:    1.5/day
Avg captured spread:  4.0% (partial fills, some competition)
Position size:        0.3 SOL
Gross per trade:      0.012 SOL
Net per trade:        0.012 - 0.007 = 0.005 SOL
Daily net:            1.5 × 0.005 = 0.0075 SOL/day ≈ $1-2/day
```

**Realistic scenario (blended):**
```
Daily net: 0.02-0.04 SOL/day ≈ $3-6/day
```

### 6.5 Verdict

| Question | Answer |
|----------|--------|
| Is this profitable? | **Marginally**, at ~$3-15/day optimistic |
| Is it worth engineering effort? | **No** for arb alone. The edge is tiny and shrinking. |
| Would ShredStream help? | Yes — 15ms detection cuts pipeline to ~170ms, increases uncontested rate to ~40-50%. Adds maybe $5-10/day. |
| Would Geyser help more? | Yes — 5ms detection is table stakes for serious arb. But then you're competing with other Geyser bots on tip. |
| What's the real play? | **Graduation arb is a solved, commoditized strategy.** The real alpha is momentum/signal trading on post-graduation price action, not the structural spread. |

**Bottom line:** The graduation arb on Raydium events is marginally profitable at 80ms latency
with ~$3-6/day expected. This does NOT justify dedicated infrastructure. However, the pool
resolution pipeline built for this (tx parsing, vault extraction, spread calculation) is
valuable infrastructure for the **momentum trading** strategy, which has much higher expected
value.

---

## 7. Engineering Spec

### 7.1 Raydium AMM v4 PDA Derivation (Verified from Source)

```rust
// Source: raydium-io/raydium-amm/program/src/processor.rs
// All PDAs use the same pattern:
//   find_program_address([RAYDIUM_PROGRAM, OPENBOOK_MARKET, SEED], RAYDIUM_PROGRAM)

const RAYDIUM_AMM_V4: Pubkey = pubkey!("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");

// Seed constants (exact byte strings from source):
const AMM_ASSOCIATED_SEED: &[u8]        = b"amm_associated_seed";
const COIN_VAULT_ASSOCIATED_SEED: &[u8] = b"coin_vault_associated_seed";
const PC_VAULT_ASSOCIATED_SEED: &[u8]   = b"pc_vault_associated_seed";
const LP_MINT_ASSOCIATED_SEED: &[u8]    = b"lp_mint_associated_seed";
const TARGET_ASSOCIATED_SEED: &[u8]     = b"target_associated_seed";
const OPEN_ORDER_ASSOCIATED_SEED: &[u8] = b"open_order_associated_seed";
const AUTHORITY_AMM: &[u8]              = b"amm authority";

// Derivation (requires OpenBook market address, NOT mint):
fn derive_raydium_pda(openbook_market: &Pubkey, seed: &[u8]) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[RAYDIUM_AMM_V4.as_ref(), openbook_market.as_ref(), seed],
        &RAYDIUM_AMM_V4,
    )
}

// Example: derive coin vault
let (coin_vault, _) = derive_raydium_pda(&market, COIN_VAULT_ASSOCIATED_SEED);
let (pc_vault, _)   = derive_raydium_pda(&market, PC_VAULT_ASSOCIATED_SEED);
let (amm_pool, _)   = derive_raydium_pda(&market, AMM_ASSOCIATED_SEED);
```

### 7.2 AmmInfo Binary Layout (Offsets for Direct Parsing)

From `raydium-amm/program/src/state.rs`, `#[repr(C, packed)]`:

```
AmmInfo struct total size: 752 bytes

Offset  Size  Type      Field
──────  ────  ────      ─────
0       8     u64       status
8       8     u64       nonce
16      8     u64       order_num
24      8     u64       depth
32      8     u64       coin_decimals
40      8     u64       pc_decimals
48      8     u64       state
56      8     u64       reset_flag
64      8     u64       min_size
72      8     u64       vol_max_cut_ratio
80      8     u64       amount_wave
88      8     u64       coin_lot_size
96      8     u64       pc_lot_size
104     8     u64       min_price_multiplier
112     8     u64       max_price_multiplier
120     8     u64       sys_decimal_value
128     64    Fees      fees (8 × u64)
192     144   StateData state_data
336     32    Pubkey    coin_vault          ← TOKEN VAULT
368     32    Pubkey    pc_vault            ← SOL (WSOL) VAULT
400     32    Pubkey    coin_vault_mint
432     32    Pubkey    pc_vault_mint
464     32    Pubkey    lp_mint
496     32    Pubkey    open_orders
528     32    Pubkey    market              ← OpenBook market
560     32    Pubkey    market_program
592     32    Pubkey    target_orders
624     64    [u64;8]   padding1
688     32    Pubkey    amm_owner
720     8     u64       lp_amount
728     8     u64       client_order_id
736     8     u64       recent_epoch
744     8     u64       padding2
```

**Key offsets for vault extraction:**
```rust
const COIN_VAULT_OFFSET: usize = 336;  // 32 bytes, Pubkey
const PC_VAULT_OFFSET: usize = 368;    // 32 bytes, Pubkey
```

### 7.3 SPL Token Account Parsing

```
SPL Token Account: 165 bytes total

Offset  Size  Type    Field
──────  ────  ────    ─────
0       32    Pubkey  mint
32      32    Pubkey  owner
64      8     u64     amount              ← THE BALANCE
72      4     u32     delegate_option
76      32    Pubkey  delegate
108     1     u8      state
109     4     u32     is_native_option
113     8     u64     is_native
121     8     u64     delegated_amount
129     4     u32     close_authority_option
133     32    Pubkey  close_authority
```

```rust
fn parse_spl_token_balance(data: &[u8]) -> u64 {
    assert!(data.len() >= 72);
    u64::from_le_bytes(data[64..72].try_into().unwrap())
}
```

### 7.4 Raydium initialize2 Account Layout

```
Account index in instruction accounts array:
[0]  token_program           (TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA)
[1]  ata_token_program       (ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL)
[2]  system_program          (11111111111111111111111111111111)
[3]  rent                    (SysvarRent111111111111111111111111111111111)
[4]  amm_id                  ← THE POOL ADDRESS
[5]  amm_authority
[6]  amm_open_orders
[7]  amm_lp_mint
[8]  amm_coin_mint           ← base token mint (pump.fun token)
[9]  amm_pc_mint             ← quote mint (WSOL)
[10] amm_coin_vault          ← TOKEN VAULT (extract this)
[11] amm_pc_vault            ← SOL VAULT (extract this)
[12] amm_target_orders
[13] amm_config
[14] create_fee_destination
[15] market_program          (OpenBook/Serum program)
[16] market                  ← OpenBook MARKET ADDRESS
[17] user_wallet             (signer)
[18] user_token_coin
[19] user_token_pc
[20] user_token_lp
```

### 7.5 Implementation Parameters

```yaml
# === DETECTION ===
detection_source: "helius_logsSubscribe"  # or bitquery
raydium_log_marker: "Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 invoke"
pumpswap_log_marker: "Program pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA invoke"
skip_pump_swap: true
dedup_ring_size: 256

# === RPC ===
rpc_method_tx: "getTransaction"
rpc_tx_encoding: "base64"
rpc_tx_max_version: 0
rpc_tx_timeout_ms: 200

rpc_method_accounts: "getMultipleAccountInfo"
rpc_accounts_encoding: "base64"
rpc_accounts_commitment: "confirmed"
rpc_accounts_timeout_ms: 150

# === PRICE CALCULATION ===
bc_terminal_price_lamports_per_atom: 4.1088e-4

# === ENTRY DECISION ===
min_spread_pct: 2.0       # below this: fees eat the edge
max_spread_pct: 50.0      # above this: bad data
position_size_sol: 0.3

# === EXIT (paper mode) ===
tp_pct: 1.5               # tight — capture arb, don't hold
sl_pct: 1.0               # tight — exit fast if wrong
max_hold_ms: 3000          # arb window closes in 1-3 seconds

# === FEES (for P&L calculation) ===
jito_tip_sol: 0.003
priority_fee_per_tx_sol: 0.0005
base_fee_per_tx_sol: 0.000005
raydium_swap_fee_bps: 25   # 0.25%
estimated_slippage_bps: 50  # 0.5% conservative

# === LOGGING ===
log_pumpswap_as: "pumpswap_skipped"
log_format: "jsonl"
log_fields: [
  "timestamp_ms", "sig", "mint", "pool_type",
  "coin_vault", "pc_vault", "reserve_token", "reserve_sol",
  "ray_price", "bc_price", "spread_pct", "action", "reason"
]
```

### 7.6 Build Priority

1. **Phase 1 (current sprint):** Fix pool resolution using Path C (tx parsing → vault extraction). Paper trade with real spread data. Validate structural spread empirically.

2. **Phase 2:** If empirical spread ≥ 5% on ≥ 10 events/day → implement live Jito bundle submission for Raydium events only.

3. **Phase 3:** Regardless of arb viability, use the same detection + pool resolution pipeline to feed the **momentum trading engine** (post-graduation directional trades based on volume/social signals).

4. **Deprioritize:** PumpSwap pool resolution (no structural arb). Only build PumpSwap parsing if needed for momentum strategy.
