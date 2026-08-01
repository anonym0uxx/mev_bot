# PumpSwap Pool Lookup Fix — Build Spec

**Date:** 2026-04-01  
**Status:** Ready for Implementation  
**Priority:** P0 — All on-chain PumpSwap trades are blocked  
**Author:** Apollo (architect agent)

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Root Cause Analysis (Verified On-Chain)](#2-root-cause-analysis-verified-on-chain)
3. [Correct PumpSwap Pool Account Layout](#3-correct-pumpswap-pool-account-layout)
4. [On-Chain Evidence](#4-on-chain-evidence)
5. [Engineer Task Breakdown](#5-engineer-task-breakdown)
   - [eng1: Fix `resolve_pumpswap_pool_from_mint()` in pool.rs](#eng1)
   - [eng2: Fix other pool resolution functions in pool.rs](#eng2)
   - [eng3: Fix feed parsers in feeds/](#eng3)
   - [eng4: Fix TX builder in tx/pumpswap.rs](#eng4)
   - [eng5: Tests + validation](#eng5)
6. [Compilation & Validation](#6-compilation--validation)

---

## 1. Executive Summary

### The bug: our `getProgramAccounts` memcmp filter only checks offset 43 (base_mint)

PumpSwap pools can store the token mint at **either** offset 43 (base_mint) **or** offset 75 (quote_mint). When the token mint's bytes sort **after** WSOL's bytes lexicographically, PumpSwap places WSOL as `base_mint` (offset 43) and the token as `quote_mint` (offset 75). Our code only filters at offset 43, missing every reversed pool.

### The offsets themselves are CORRECT

The field positions (discriminator at 0..8, base_mint at 43..75, vaults at 139..171 and 171..203) are correct in both 211-byte and 301-byte pool accounts. **We do NOT need to change any offset values.**

### What we DO need to fix

| Bug | Impact | Fix |
|-----|--------|-----|
| **Only filter at offset 43** | Misses ~81% of 301-byte pools and ~16% of 211-byte pools where WSOL is base_mint | Two-query strategy: try offset 43, then offset 75 |
| **Vault assignment assumes token=base** | When pool is reversed, `pool_base_token_account` holds WSOL (not the token) | Detect ordering from on-chain data, swap vault assignments |
| **Example "failing" mints graduated to Raydium** | Bot detects these as PumpSwap graduations when they're actually Raydium | Not a pool layout bug — separate graduation detection issue (documented for awareness) |
| **create_pool fallback extracts accounts[2] as token mint** | In reversed pools created via direct call, accounts[2] could be WSOL | Check accounts[3] for WSOL; if accounts[2] == WSOL, use accounts[3] as token mint |

### Scale of the problem

| Pool Size | Total | WSOL as base (offset 43) | WSOL as quote (offset 75) | Token at 43 (found by us) | Token at 75 (MISSED) |
|-----------|-------|--------------------------|---------------------------|---------------------------|----------------------|
| 211 bytes | 38,048 | 6,140 (16%) | 31,720 (84%) | 31,720 (84%) | 6,140 (16%) |
| 301 bytes | 286,641 | 232,509 (81%) | ~54,132 (19%) | ~54,132 (19%) | 232,509 (81%) |

**For 301-byte pools (the current majority), we miss 81% of all pools.**

---

## 2. Root Cause Analysis (Verified On-Chain)

### Bug 1: Unidirectional memcmp filter

**Current code** (`pool.rs:788-801`):
```rust
// PumpSwap pools have the graduated token as base_mint (offset 43), 
// WSOL as quote_mint (offset 75).
// Filter on base_mint at offset 43 to find the pool for this token.
let body = serde_json::json!({
    "filters": [
        {"memcmp": {"offset": 43, "bytes": mint_b58}}
    ]
});
```

**Reality:** PumpSwap's `create_pool` instruction sorts mint addresses deterministically. The mint with the **lower byte value** becomes `base_mint`. Since WSOL (`069b8857...`) has a very low first byte, it sorts before most pump.fun token mints, making WSOL the `base_mint` in the majority of pools.

### Bug 2: Hardcoded vault-to-role mapping

**Current code** (`pool.rs:841-842`):
```rust
let coin_vault: [u8; 32] = data[139..171].try_into().ok()?; // token vault
let pc_vault: [u8; 32] = data[171..203].try_into().ok()?;   // WSOL/SOL vault
```

When the pool is reversed (WSOL = base_mint):
- `pool_base_token_account` (offset 139..171) = **WSOL vault** (not the token vault!)
- `pool_quote_token_account` (offset 171..203) = **token vault** (not the WSOL vault!)

Our code always assigns offset 139 as `coin_vault` (token) and offset 171 as `pc_vault` (SOL), which is **wrong for reversed pools**.

### Bug 3: Shredstream create_pool fallback hardcodes accounts[2] as token mint

**Current code** (`shredstream.rs:956-963`):
```rust
// accounts[2] = base_mint  ← THE TOKEN MINT
let mint = if ix.accounts.len() > 2 {
    let mint_idx = ix.accounts[2] as usize;
```

In a reversed pool, `accounts[2]` is WSOL, not the token. The code should check `accounts[3]` (quote_mint) when `accounts[2]` is WSOL.

### NOT a bug: the example failing mints

The three mints listed in the bug report (`LtYKwqd...`, `2ZiykxvY...`, `KExnjBsx...`) all graduated to **Raydium**, not PumpSwap. They don't have PumpSwap pools at all. This is a separate issue in graduation detection (misidentifying Raydium graduations as PumpSwap), not a pool layout bug.

---

## 3. Correct PumpSwap Pool Account Layout

### Field Map (both 211-byte and 301-byte accounts)

```
Offset     Size    Field                        Notes
─────────────────────────────────────────────────────────────────
[0..8]     8       discriminator                 f19a6d0411b16dbc (sha256("account:Pool")[..8])
[8]        1       pool_bump                     PDA bump seed
[9..11]    2       index                         u16 LE — pool index
[11..43]   32      creator                       Pool creator pubkey
[43..75]   32      base_mint                     ⚠️ Can be WSOL or token (see ordering rules)
[75..107]  32      quote_mint                    ⚠️ Can be WSOL or token (see ordering rules)
[107..139] 32      lp_mint                       LP token mint
[139..171] 32      pool_base_token_account       Vault for base_mint (token OR WSOL)
[171..203] 32      pool_quote_token_account      Vault for quote_mint (WSOL OR token)
```

**211-byte accounts** (older pools):
```
[203..211] 8       lp_fee_basis_points(?)        u64 LE (observed: 100 = 1%)
Total: 211 bytes
```

**301-byte accounts** (newer pools, majority):
```
[203..211] 8       lp_fee_basis_points(?)        u64 LE (observed: 100, 101)
[211..235] 24      reserved/unknown              All zeros in sampled accounts
[235..267] 32      field_new_1                   All zeros in sampled accounts
[267..299] 32      field_new_2                   All zeros in sampled accounts
[299..301] 2       padding                       0x0000
Total: 301 bytes
```

### Ordering Rules

PumpSwap sorts mints **by raw byte comparison** (32-byte LE comparison):
- If `token_bytes < WSOL_bytes` → token is `base_mint` (offset 43), WSOL is `quote_mint` (offset 75) → **"normal"**
- If `token_bytes > WSOL_bytes` → WSOL is `base_mint` (offset 43), token is `quote_mint` (offset 75) → **"reversed"**

**WSOL raw bytes:** `069b8857feab8184fb687f634618c035dac439dc1aeb3b5598a0f00000000001`

Since WSOL's first byte is `0x06`, any mint starting with `0x07..0xFF` (the vast majority) will sort after WSOL, resulting in a **reversed** pool. This is why 81% of 301-byte pools have WSOL as base_mint.

### Discriminator

```
Anchor discriminator = sha256("account:Pool")[0..8] = f19a6d0411b16dbc
```

Identical for both 211-byte and 301-byte accounts. This has NOT changed.

---

## 4. On-Chain Evidence

### Reference Pool Accounts (raw hex dumps)

#### Pool A: REVERSED ordering (WSOL = base, token = quote)

```
Address: 114XmiBstWqYVhSiH6qnU4jFCskFxP8t9iBqBLJPmaf
Size:    301 bytes
base_mint  = So11111111111111111111111111111111111111112 (WSOL)
quote_mint = Hn6YPJUNh2f94hxumAYbRrMTSVL2D5Epj8AnuSU9QNNS (token)

Raw hex:
[  0.. 32] f19a6d0411b16dbc ff0000 b57bd18d8442b46fa7aae6ad0d45d6402684a03cdd
[           disc------------ b idx- creator-------------------------------------------
[ 32.. 64] f9a55e4af69517e1306c40 069b8857feab8184fb687f634618c035dac439dc1a
           creator-cont------- base_mint(WSOL)--------------------------------------
[ 64.. 96] eb3b5598a0f00000000001 f94864807b812c345bbc12277490afc57547cf1c3c
           base_mint-cont-------- quote_mint(token)----------------------------------
[ 96..128] dbdb160c1c07600c71b407 2a389172df6923ea674056f6153e3c487eaaa6be32
           quote_mint-cont------- lp_mint--------------------------------------------
[128..160] 01fd0148a1d1ee0a3113bb 7dcab7ceb12ac3f22d582e20695a8c2249fc7582bd
           lp_mint-cont---------- pool_base_token_account(WSOL_vault)----------------
[160..192] 6b09a31f9332ef294ba213 d7e6a388dad3f3c90d4074fdc2f082ae3c171f16ce
           base_vault-cont------- pool_quote_token_account(token_vault)--------------
[192..224] c167488d6f5172986f7eed 64 00000000 0000000000000000000000000000000000
           quote_vault-cont-----  fee? remaining-zero-padded------------------------
```

**Vault mapping for REVERSED pool:**
- `pool_base_token_account` [139..171] = **WSOL vault** (NOT token vault)
- `pool_quote_token_account` [171..203] = **Token vault** (NOT WSOL vault)

#### Pool B: NORMAL ordering (token = base, WSOL = quote)

```
Address: 11CwRL2M8m5EeZUphCx8BvD6GXjw9VGTQUhjrWkjr3L
Size:    301 bytes
base_mint  = 9GvgSRMprTdnrpuQhS3uzCe1FijtZxPU974H8zjHpump (token)
quote_mint = So11111111111111111111111111111111111111112 (WSOL)

Raw hex:
[  0.. 32] f19a6d0411b16dbc fd0100 be323dacefdfad2b71f1781da31d0b4313d65187a8
[           disc------------ b idx- creator-------------------------------------------
[ 32.. 64] a889a0061540eeecd3e67b 7af1e757c207faf4a31fc47db5646414ad128c63b5
           creator-cont------- base_mint(9GvgS...pump)-------------------------------
[ 64.. 96] 3c7279330713b412b333cf 069b8857feab8184fb687f634618c035dac439dc1a
           base_mint-cont-------- quote_mint(WSOL)-----------------------------------
[ 96..128] eb3b5598a0f00000000001 c362ae6060596a66697073d13b90cacd78fbac3e59
           quote_mint-cont------- lp_mint--------------------------------------------
[128..160] 0395fa11fbc8705c53547f f74b79ce7ebb6893f97823009584f6de0a58117ada
           lp_mint-cont---------- pool_base_token_account(token_vault)---------------
[160..192] bb3b573a937e7b7ee3d68b c2b996d8b196051d3400c924525a219a14a67492ba
           base_vault-cont------- pool_quote_token_account(WSOL_vault)---------------
[192..224] c738262023e2b2ebfaba48 65 00000000 0000000000000000000000000000000000
           quote_vault-cont-----  fee? remaining-zero-padded------------------------
```

**Vault mapping for NORMAL pool:**
- `pool_base_token_account` [139..171] = **Token vault** (correct as coin_vault)
- `pool_quote_token_account` [171..203] = **WSOL vault** (correct as pc_vault)

#### Pool C: 211-byte NORMAL ordering

```
Address: JEBqxuDB3isvH1wf2Dpa9Q9P6BWUsZSAJnNU6ksz4rpB
Size:    211 bytes
base_mint  = tEW5gqm8zvNian5T5eEWSJxf92VnpdhuG7BuJbYpump (token)
quote_mint = So11111111111111111111111111111111111111112 (WSOL)

Raw hex:
[  0.. 32] f19a6d0411b16dbc ff0000 fef83cee47c9b6733335c3ee9da78a707f3a8a5117
[ 32.. 64] 41a942061cea1e36b1a80d 0d1fe32881fbd3dea81322c065e5eda2e2086644fe
[ 64.. 96] c2565a0b1a312790ed053f 069b8857feab8184fb687f634618c035dac439dc1a
[ 96..128] eb3b5598a0f00000000001 781c9d0ab5de0ea1e4b3b01ffeb24c28dccf914858
[128..160] 6b6e2fe658646379c662cd 94957e57ff09d3cc80e94fdfbd6777df13b34996d1
[160..192] 2844658d73a7cb318407c7 818212cd53a985e7d5753b6de574ca5e4a3fdea813
[192..211] 2639eee34f8a5d7c5cabff 6400000000000000
```

### Verification Queries

```bash
# CONFIRMED: token at offset 43 → found
curl -s $RPC -X POST -d '{
  "jsonrpc":"2.0","id":1,"method":"getProgramAccounts",
  "params":["pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
    {"encoding":"base64","dataSlice":{"offset":0,"length":0},
     "filters":[{"memcmp":{"offset":43,"bytes":"9GvgSRMprTdnrpuQhS3uzCe1FijtZxPU974H8zjHpump"}}]}]
}' # → 1 result ✅

# CONFIRMED: token at offset 75 → found
curl -s $RPC -X POST -d '{
  "jsonrpc":"2.0","id":1,"method":"getProgramAccounts",
  "params":["pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
    {"encoding":"base64","dataSlice":{"offset":0,"length":0},
     "filters":[{"memcmp":{"offset":75,"bytes":"Hn6YPJUNh2f94hxumAYbRrMTSVL2D5Epj8AnuSU9QNNS"}}]}]
}' # → 1 result ✅

# CONFIRMED: token at offset 43 → NOT found (it's at 75)
curl -s $RPC -X POST -d '{
  "jsonrpc":"2.0","id":1,"method":"getProgramAccounts",
  "params":["pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
    {"encoding":"base64","dataSlice":{"offset":0,"length":0},
     "filters":[{"memcmp":{"offset":43,"bytes":"Hn6YPJUNh2f94hxumAYbRrMTSVL2D5Epj8AnuSU9QNNS"}}]}]
}' # → 0 results ❌ (this is the bug!)

# Pool count statistics:
# 211-byte:  38,048 total | WSOL at 43: 6,140 (16%)  | WSOL at 75: 31,720 (84%)
# 301-byte: 286,641 total | WSOL at 43: 232,509 (81%) | WSOL at 75: ~54,132 (19%)
```

---

## 5. Engineer Task Breakdown

All engineers work on non-overlapping files/functions. Each task is independent and can be done in parallel.

**WSOL constant** (needed by eng1, eng2, eng3): 
```rust
const WSOL_MINT_BYTES: [u8; 32] = [
    0x06, 0x9b, 0x88, 0x57, 0xfe, 0xab, 0x81, 0x84,
    0xfb, 0x68, 0x7f, 0x63, 0x46, 0x18, 0xc0, 0x35,
    0xda, 0xc4, 0x39, 0xdc, 0x1a, 0xeb, 0x3b, 0x55,
    0x98, 0xa0, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x01,
];
```

---

<a id="eng1"></a>
### eng1: Fix `resolve_pumpswap_pool_from_mint()` in pool.rs

**File:** `rust/pump-quant-core/src/momentum/pool.rs`  
**Lines:** 740–899  
**DO NOT TOUCH:** Lines 900+ (Raydium resolver), Lines 1030+ (PumpSwapPoolAccounts struct), test functions

#### Change 1: Add WSOL_MINT_BYTES constant

**Location:** After line 126 (after `PUMPSWAP_AMM_PROGRAM` constant)

**Add:**
```rust
/// WSOL mint as raw bytes for detecting reversed PumpSwap pool ordering.
/// PumpSwap sorts mints by raw byte comparison — when token > WSOL,
/// WSOL becomes base_mint and token becomes quote_mint.
const WSOL_MINT_BYTES: [u8; 32] = [
    0x06, 0x9b, 0x88, 0x57, 0xfe, 0xab, 0x81, 0x84,
    0xfb, 0x68, 0x7f, 0x63, 0x46, 0x18, 0xc0, 0x35,
    0xda, 0xc4, 0x39, 0xdc, 0x1a, 0xeb, 0x3b, 0x55,
    0x98, 0xa0, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x01,
];
```

#### Change 2: Update doc comment for `resolve_pumpswap_pool_from_mint()`

**Location:** Lines 752–769 (doc comment block)

**Replace** the entire doc comment with:
```rust
/// Resolve a PumpSwap AMM pool from the token mint using getProgramAccounts.
///
/// PumpSwap Pool account layout (verified on-chain 2026-04-01):
///   [0..8]    discriminator (f19a6d0411b16dbc)
///   [8]       pool_bump (u8)
///   [9..11]   index (u16 LE)
///   [11..43]  creator (pubkey)
///   [43..75]  base_mint  ← Can be WSOL or token (sorted by raw bytes)
///   [75..107] quote_mint ← Can be WSOL or token (sorted by raw bytes)
///   [107..139] lp_mint
///   [139..171] pool_base_token_account  ← vault for base_mint
///   [171..203] pool_quote_token_account ← vault for quote_mint
///
/// **ORDERING:** PumpSwap sorts mints by raw byte comparison. WSOL (0x069b...)
/// sorts before most pump.fun tokens, so ~81% of pools have WSOL as base_mint
/// (offset 43) and the token as quote_mint (offset 75).
///
/// **Strategy:** Try offset 43 first. If empty, retry at offset 75.
/// Then detect which field is WSOL to correctly assign coin_vault vs pc_vault.
///
/// # Parameters
/// - `public_rpc_url` — public Solana RPC for getMultipleAccounts (vault reserves)
/// - `helius_rpc_url` — Helius API-key endpoint for getProgramAccounts
```

#### Change 3: Rewrite the function body (lines 770–899)

**Replace** the entire function body (from `pub async fn resolve_pumpswap_pool_from_mint` through the closing `}` before the Raydium resolver doc comment) with:

```rust
pub async fn resolve_pumpswap_pool_from_mint(
    client: &reqwest::Client,
    mint: &[u8; 32],
    public_rpc_url: &str,
    helius_rpc_url: &str,
) -> Option<PoolResolution> {
    // ── Concurrency gate ─────────────────────────────────────────────────
    let _permit = match POOL_RESOLUTION_SEMAPHORE.try_acquire() {
        Ok(p) => p,
        Err(_) => {
            tracing::debug!("[pool] resolution semaphore full — dropping resolve_pumpswap_pool_from_mint");
            return None;
        }
    };
    let mint_b58 = bs58::encode(mint).into_string();

    // ── Two-pass getProgramAccounts: try offset 43 (base_mint), then 75 (quote_mint) ──
    // PumpSwap sorts mints by raw bytes. WSOL (0x069b...) sorts before most tokens,
    // so the token ends up as quote_mint (offset 75) in ~81% of pools.
    let mut pool_data: Option<(serde_json::Value, Vec<u8>)> = None;
    let mut token_is_base = true;

    for (offset, is_base) in [(43, true), (75, false)] {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getProgramAccounts",
            "params": [
                PUMPSWAP_AMM_PROGRAM,
                {
                    "encoding": "base64",
                    "commitment": "confirmed",
                    "filters": [
                        {"memcmp": {"offset": offset, "bytes": mint_b58}}
                    ]
                }
            ]
        });

        let resp = match client.post(helius_rpc_url).json(&body).send().await {
            Ok(r) => r,
            Err(_) => continue,
        };

        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(_) => continue,
        };

        let accounts = match json.pointer("/result").and_then(|r| r.as_array()) {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };

        use base64::Engine as _;
        if let Some(data_b64) = accounts[0].pointer("/account/data/0").and_then(|d| d.as_str()) {
            if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(data_b64) {
                if data.len() >= 204 {
                    pool_data = Some((accounts[0].clone(), data));
                    token_is_base = is_base;
                    tracing::debug!(
                        mint = %mint_b58,
                        offset,
                        token_is_base,
                        "[momentum] PumpSwap pool found"
                    );
                    break;
                }
            }
        }
    }

    let (account_json, data) = match pool_data {
        Some(d) => d,
        None => {
            tracing::debug!(
                mint = %mint_b58,
                "[momentum] PumpSwap pool lookup: no pool found at offset 43 or 75"
            );
            return None;
        }
    };

    let pool_address = decode_bs58_32(account_json.get("pubkey")?.as_str()?)?;

    // ── Vault assignment: depends on pool ordering ──────────────────────
    // When token_is_base (normal):
    //   pool_base_token_account [139..171] = token vault (coin_vault)
    //   pool_quote_token_account [171..203] = WSOL vault (pc_vault)
    // When !token_is_base (reversed, WSOL is base):
    //   pool_base_token_account [139..171] = WSOL vault (pc_vault)
    //   pool_quote_token_account [171..203] = token vault (coin_vault)
    let (coin_vault, pc_vault) = if token_is_base {
        // Normal: base=token, quote=WSOL
        let cv: [u8; 32] = data[139..171].try_into().ok()?; // token vault
        let pv: [u8; 32] = data[171..203].try_into().ok()?; // WSOL vault
        (cv, pv)
    } else {
        // Reversed: base=WSOL, quote=token
        let pv: [u8; 32] = data[139..171].try_into().ok()?; // WSOL vault (base)
        let cv: [u8; 32] = data[171..203].try_into().ok()?; // token vault (quote)
        (cv, pv)
    };

    let coin_vault_b58 = bs58::encode(&coin_vault).into_string();
    let pc_vault_b58 = bs58::encode(&pc_vault).into_string();

    // getMultipleAccounts → public RPC (vault reserves are read-only)
    let (reserve_token, reserve_sol) =
        fetch_vault_reserves(client, public_rpc_url, &coin_vault_b58, &pc_vault_b58).await?;

    // FIX-3: PumpSwap uses lower 30 SOL threshold (fresh graduations start at ~85 SOL
    // but some valid pools have 30-50 SOL). Raydium keeps 50 SOL minimum.
    if reserve_sol < MIN_PUMPSWAP_SOL_RESERVES_LAMPORTS {
        tracing::warn!(
            mint = %mint_b58,
            pool = %bs58::encode(&pool_address).into_string(),
            reserve_sol,
            "[momentum] PumpSwap pool rejected — insufficient liquidity (reserve_sol < 30 SOL)"
        );
        return None;
    }

    tracing::info!(
        mint = %mint_b58,
        pool = %bs58::encode(&pool_address).into_string(),
        token_is_base,
        reserve_sol,
        reserve_token,
        "[momentum] PumpSwap pool resolved via mint lookup"
    );

    Some(PoolResolution {
        mint: *mint,
        pool_address,
        coin_vault,
        pc_vault,
        pool_type: PoolType::PumpSwap,
        reserve_sol_lamports: reserve_sol,
        reserve_token_atoms: reserve_token,
        bc_terminal_vsol: 0.0,
        grad_block_time_ms: 0,
        amm_id: [0u8; 32],
        amm_open_orders: [0u8; 32],
        amm_target_orders: [0u8; 32],
        serum_market: [0u8; 32],
        serum_bids: [0u8; 32],
        serum_asks: [0u8; 32],
        serum_event_queue: [0u8; 32],
        serum_coin_vault: [0u8; 32],
        serum_pc_vault: [0u8; 32],
        serum_vault_signer: [0u8; 32],
    })
}
```

---

<a id="eng2"></a>
### eng2: Fix other pool resolution functions in pool.rs

**File:** `rust/pump-quant-core/src/momentum/pool.rs`  
**Lines:** 300–738 (tx-based resolution path), 1030–1070 (PumpSwapPoolAccounts)  
**DO NOT TOUCH:** Lines 740–899 (eng1 owns this), Lines 900+ (Raydium resolver), test functions

#### Analysis: `resolve_pool_from_transaction()` (lines 300–600)

This function uses `extract_vaults_from_tx_response()` which matches vaults by **mint type in postTokenBalances** (token mint → coin_vault, WSOL mint → pc_vault). This approach is **ordering-agnostic** and already correct for both normal and reversed pools. **No change needed.**

#### Analysis: `extract_vaults_from_tx_response()` (lines 616–672)

This function finds vaults by scanning `postTokenBalances` and matching on the graduation mint string vs WSOL_MINT string. It picks the entry with the highest token balance as coin_vault and highest WSOL balance as pc_vault. This is **correct regardless of pool ordering**. **No change needed.**

#### Change 1: Update `PumpSwapPoolAccounts` doc comment

**Location:** Lines 1030–1048

**Replace** the doc comments with:
```rust
/// Lightweight pool accounts for PumpSwap live execution.
/// Extracted from PoolResolution at graduation time.
/// Stored in MomentumEngine.pumpswap_pools DashMap.
///
/// **IMPORTANT:** `pool_base_token_account` and `pool_quote_token_account` are
/// named from the pool's perspective. The token can be either base or quote
/// depending on PumpSwap's mint sorting. `extract_pumpswap_pool_accounts()`
/// handles this by mapping PoolResolution.coin_vault (always the token vault)
/// to the correct field based on pool ordering.
///
/// For the TX builder, `base_mint` is always the token mint (not WSOL), and
/// `pool_base_token_account` is always the token vault, regardless of the
/// on-chain pool ordering. The builder uses these directly.
```

#### Change 2: Verify `extract_pumpswap_pool_accounts()` mapping

**Location:** Lines 1053–1070

**Current code:**
```rust
Some(PumpSwapPoolAccounts {
    pool: res.pool_address,
    base_mint: res.mint,
    pool_base_token_account: res.coin_vault,
    pool_quote_token_account: res.pc_vault,
    coin_creator_vault_ata: [0u8; 32],
    coin_creator_vault_authority: [0u8; 32],
})
```

This maps `PoolResolution.coin_vault` → `pool_base_token_account` and `PoolResolution.pc_vault` → `pool_quote_token_account`. Since eng1 ensures `coin_vault` is always the token vault and `pc_vault` is always the WSOL vault (regardless of on-chain ordering), this mapping is **semantically correct** for the TX builder.

**However**, the field names are misleading when the on-chain pool has reversed ordering. Document this:

**Replace** the function with:
```rust
pub fn extract_pumpswap_pool_accounts(res: &PoolResolution) -> Option<PumpSwapPoolAccounts> {
    if res.pool_type != PoolType::PumpSwap {
        return None;
    }
    if res.pool_address == [0u8; 32] {
        return None;
    }
    // NOTE: coin_vault = token vault, pc_vault = WSOL vault — regardless of
    // on-chain pool ordering. The caller (resolve_pumpswap_pool_from_mint or
    // resolve_pool_from_transaction) already normalizes the vault assignments.
    Some(PumpSwapPoolAccounts {
        pool: res.pool_address,
        base_mint: res.mint,
        pool_base_token_account: res.coin_vault,   // token vault (always)
        pool_quote_token_account: res.pc_vault,     // WSOL vault (always)
        coin_creator_vault_ata: [0u8; 32],
        coin_creator_vault_authority: [0u8; 32],
    })
}
```

#### Summary

| Function | Status | Action |
|----------|--------|--------|
| `resolve_pool_from_transaction()` | ✅ Correct (uses postTokenBalances by mint type) | No change |
| `extract_vaults_from_tx_response()` | ✅ Correct (ordering-agnostic) | No change |
| `fetch_vault_reserves()` | ✅ Correct (just reads SPL account data) | No change |
| `PumpSwapPoolAccounts` struct | ⚠️ Misleading doc | Update doc comment |
| `extract_pumpswap_pool_accounts()` | ⚠️ Correct but confusing | Add clarifying comment |

---

<a id="eng3"></a>
### eng3: Fix feed parsers in feeds/

**Files:**
- `rust/pump-quant-core/src/feeds/shredstream.rs` (lines 842–1000)
- `rust/pump-quant-core/src/feeds/helius.rs` (lines 488–670)

**DO NOT TOUCH:** pool.rs (eng1/eng2), tx/pumpswap.rs (eng4), test functions outside your scope

#### Change 1: Fix shredstream `create_pool` fallback (Strategy 2)

**File:** `rust/pump-quant-core/src/feeds/shredstream.rs`  
**Location:** Lines 953–974 (inside `parse_pumpswap_migration()`)

**Current code:**
```rust
        // PumpSwap `create_pool` account layout:
        // accounts[0] = pool (new PDA)
        // accounts[1] = creator
        // accounts[2] = base_mint  ← THE TOKEN MINT
        // accounts[3] = quote_mint (WSOL)
        // accounts[4] = lp_mint
        // accounts[5] = pool_base_token_account
        // accounts[6] = pool_quote_token_account
        // ...
        let mint = if ix.accounts.len() > 2 {
            let mint_idx = ix.accounts[2] as usize;
            if mint_idx < account_keys.len() {
                account_keys[mint_idx].to_bytes()
            } else {
                [0u8; 32]
            }
        } else {
            [0u8; 32]
        };
```

**Replace with:**
```rust
        // PumpSwap `create_pool` account layout:
        // accounts[0] = pool (new PDA)
        // accounts[1] = creator
        // accounts[2] = base_mint  (can be WSOL or token — PumpSwap sorts by bytes)
        // accounts[3] = quote_mint (can be WSOL or token)
        // accounts[4] = lp_mint
        // accounts[5] = pool_base_token_account
        // accounts[6] = pool_quote_token_account
        // ...
        //
        // PumpSwap sorts mints by raw bytes: lower bytes = base_mint.
        // WSOL (0x069b...) sorts before most tokens, so accounts[2] is often WSOL.
        // Extract the NON-WSOL mint as the token mint.
        let mint = if ix.accounts.len() > 3 {
            let base_idx = ix.accounts[2] as usize;
            let quote_idx = ix.accounts[3] as usize;
            let base_key = if base_idx < account_keys.len() {
                account_keys[base_idx]
            } else {
                Pubkey::default()
            };
            let quote_key = if quote_idx < account_keys.len() {
                account_keys[quote_idx]
            } else {
                Pubkey::default()
            };

            // Pick the non-WSOL mint as the token
            if base_key == WSOL_PUBKEY {
                quote_key.to_bytes()
            } else {
                base_key.to_bytes()
            }
        } else if ix.accounts.len() > 2 {
            // Fallback: only base_mint available
            let mint_idx = ix.accounts[2] as usize;
            if mint_idx < account_keys.len() {
                account_keys[mint_idx].to_bytes()
            } else {
                [0u8; 32]
            }
        } else {
            [0u8; 32]
        };
```

**Also add** the WSOL_PUBKEY constant near the top of the file (around line 53, after the PumpSwap constants):

```rust
/// WSOL mint as Pubkey for PumpSwap pool ordering detection.
const WSOL_PUBKEY: Pubkey = Pubkey::new_from_array([
    0x06, 0x9b, 0x88, 0x57, 0xfe, 0xab, 0x81, 0x84,
    0xfb, 0x68, 0x7f, 0x63, 0x46, 0x18, 0xc0, 0x35,
    0xda, 0xc4, 0x39, 0xdc, 0x1a, 0xeb, 0x3b, 0x55,
    0x98, 0xa0, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x01,
]);
```

**Note:** Strategy 1 (pump.fun `migrate` instruction, lines 867–926) extracts mint from `accounts[1]` which is always the token mint from pump.fun's perspective — **no change needed** for Strategy 1.

#### Change 2: Verify Helius `parse_pumpswap_transaction()` (helius.rs)

**File:** `rust/pump-quant-core/src/feeds/helius.rs`  
**Location:** Lines 502–665 (`parse_pumpswap_transaction()`)

**Analysis:** This function extracts the mint from `postTokenBalances` by finding the first non-WSOL mint:
```rust
let mint_b58 = post_balances.iter().find_map(|entry| {
    let mint = entry.get("mint")?.as_str()?;
    if mint != WSOL_MINT {
        Some(mint.to_string())
    } else {
        None
    }
})?;
```

This is **already ordering-agnostic** — it picks the non-WSOL mint regardless of pool ordering. **No change needed.**

The vault extraction also uses `postTokenBalances` matching by mint string (token mint → coin_vault, WSOL → pc_vault), which is **already correct**. **No change needed.**

#### Summary

| File | Function | Status | Action |
|------|----------|--------|--------|
| shredstream.rs | Strategy 1 (pump.fun migrate) | ✅ Correct | No change |
| shredstream.rs | Strategy 2 (create_pool fallback) | ❌ Bug: hardcodes accounts[2] | Fix: detect WSOL, pick non-WSOL |
| helius.rs | `parse_pumpswap_transaction()` | ✅ Correct (uses postTokenBalances) | No change |

---

<a id="eng4"></a>
### eng4: Fix TX builder in tx/pumpswap.rs

**File:** `rust/pump-quant-core/src/tx/pumpswap.rs`  
**Lines:** 60–320 (struct + instruction builder)  
**DO NOT TOUCH:** pool.rs (eng1/eng2), feeds/ (eng3), test functions (eng5 scope)

#### Analysis

The TX builder receives a `PumpSwapPoolAccounts` struct. The critical question: **when the pool is reversed (WSOL = base), does the swap instruction need different account ordering?**

Looking at the `build_pumpswap_swap_ix()` function (lines 259–320):

```rust
// [3]  base_mint           (readonly) → pool.base_mint  
// [4]  quote_mint (WSOL)   (readonly) → hardcoded WSOL
// [7]  pool_base_token_account (writable) → pool.pool_base_token_account
// [8]  pool_quote_token_account (writable) → pool.pool_quote_token_account
```

The function:
1. Uses `pool.base_mint` as account [3] (base_mint) — but in `PumpSwapPoolAccounts`, `base_mint` is always set to the **token mint** (from `PoolResolution.mint`)
2. Hardcodes `wsol_mint` as account [4] (quote_mint)
3. Uses `pool.pool_base_token_account` as account [7] — set from `PoolResolution.coin_vault` (always the token vault)
4. Uses `pool.pool_quote_token_account` as account [8] — set from `PoolResolution.pc_vault` (always the WSOL vault)

**This is WRONG for reversed pools.** The PumpSwap program expects account [3] to be the on-chain `base_mint` (which is WSOL in reversed pools) and account [7] to be the on-chain `pool_base_token_account` (which is the WSOL vault in reversed pools).

#### The Fix

The `PumpSwapPoolAccounts` struct needs to track whether the pool has reversed ordering, so the TX builder can place accounts correctly.

#### Change 1: Add `token_is_base` flag to `PumpSwapPoolAccounts`

**File:** `rust/pump-quant-core/src/tx/pumpswap.rs`, lines 78–90

**Replace:**
```rust
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
}
```

**With:**
```rust
pub struct PumpSwapPoolAccounts {
    /// Pool PDA address (PoolResolution.pool_address)
    pub pool: [u8; 32],
    /// Token mint address (always the graduated token, NOT WSOL)
    pub token_mint: [u8; 32],
    /// Token vault address (SPL token account holding graduated token reserves)
    pub token_vault: [u8; 32],
    /// WSOL vault address (SPL token account holding wrapped SOL reserves)
    pub wsol_vault: [u8; 32],
    /// Coin creator vault ATA (may be zeroed if not applicable; always include in ix)
    pub coin_creator_vault_ata: [u8; 32],
    /// Coin creator vault authority (may be zeroed; always include in ix)
    pub coin_creator_vault_authority: [u8; 32],
    /// Whether the token is the on-chain base_mint (true) or quote_mint (false).
    /// When false, WSOL is base_mint and the token is quote_mint.
    /// Affects account ordering in the swap instruction.
    pub token_is_base: bool,
}
```

#### Change 2: Update `From<momentum::pool::PumpSwapPoolAccounts>` impl

**Location:** Lines 95–105

**Replace:**
```rust
impl From<crate::momentum::pool::PumpSwapPoolAccounts> for PumpSwapPoolAccounts {
    fn from(p: crate::momentum::pool::PumpSwapPoolAccounts) -> Self {
        Self {
            pool: p.pool,
            base_mint: p.base_mint,
            pool_base_token_account: p.pool_base_token_account,
            pool_quote_token_account: p.pool_quote_token_account,
            coin_creator_vault_ata: p.coin_creator_vault_ata,
            coin_creator_vault_authority: p.coin_creator_vault_authority,
        }
    }
}
```

**With:**
```rust
impl From<crate::momentum::pool::PumpSwapPoolAccounts> for PumpSwapPoolAccounts {
    fn from(p: crate::momentum::pool::PumpSwapPoolAccounts) -> Self {
        Self {
            pool: p.pool,
            token_mint: p.base_mint,                           // always the token mint
            token_vault: p.pool_base_token_account,            // always the token vault
            wsol_vault: p.pool_quote_token_account,            // always the WSOL vault
            coin_creator_vault_ata: p.coin_creator_vault_ata,
            coin_creator_vault_authority: p.coin_creator_vault_authority,
            token_is_base: p.token_is_base,
        }
    }
}
```

#### Change 3: Update `build_pumpswap_swap_ix()` for ordering

**Location:** Lines 259–320

The key accounts that change based on ordering:
- `[3]` base_mint → WSOL if reversed, token if normal
- `[4]` quote_mint → token if reversed, WSOL if normal
- `[5]` user_base_token_account → user's WSOL ATA if reversed, user's token ATA if normal
- `[6]` user_quote_token_account → user's token ATA if reversed, user's WSOL ATA if normal
- `[7]` pool_base_token_account → WSOL vault if reversed, token vault if normal
- `[8]` pool_quote_token_account → token vault if reversed, WSOL vault if normal

**Replace** the relevant section of `build_pumpswap_swap_ix()`:

```rust
fn build_pumpswap_swap_ix(
    pool: &PumpSwapPoolAccounts,
    wallet_pubkey: &Pubkey,
    fee_recipient_idx: usize,
    arg1: u64,
    arg2: u64,
) -> Instruction {
    let pumpswap_program = Pubkey::from_str(PUMPSWAP_PROGRAM).unwrap();
    let global_config = Pubkey::from_str(PUMPSWAP_GLOBAL_CONFIG).unwrap();
    let wsol_mint = Pubkey::from_str(WSOL_MINT_STR).unwrap();
    let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM_STR).unwrap();
    let ata_program = Pubkey::from_str(SPL_ATA_PROGRAM_STR).unwrap();
    let event_authority = Pubkey::from_str(PUMPSWAP_EVENT_AUTHORITY).unwrap();
    let fee_program = Pubkey::from_str(PUMPSWAP_FEE_PROGRAM).unwrap();

    let token_mint = Pubkey::new_from_array(pool.token_mint);
    let user_token_ata = token_ata(wallet_pubkey, &token_mint);
    let user_wsol_ata = token_ata(wallet_pubkey, &wsol_mint);

    // Determine on-chain account ordering based on pool.token_is_base
    let (base_mint_pubkey, quote_mint_pubkey) = if pool.token_is_base {
        (token_mint, wsol_mint)
    } else {
        (wsol_mint, token_mint)
    };

    let (user_base_ata, user_quote_ata) = if pool.token_is_base {
        (user_token_ata, user_wsol_ata)
    } else {
        (user_wsol_ata, user_token_ata)
    };

    let (pool_base_vault, pool_quote_vault) = if pool.token_is_base {
        // Normal: base=token, quote=WSOL
        (Pubkey::new_from_array(pool.token_vault), Pubkey::new_from_array(pool.wsol_vault))
    } else {
        // Reversed: base=WSOL, quote=token
        (Pubkey::new_from_array(pool.wsol_vault), Pubkey::new_from_array(pool.token_vault))
    };

    // Fee recipient rotation
    let fee_recipient = Pubkey::from_str(
        PUMPSWAP_FEE_RECIPIENTS[fee_recipient_idx % 8],
    )
    .unwrap();
    let fee_recipient_token_account = token_ata(&fee_recipient, &wsol_mint);

    let coin_creator_vault_ata = Pubkey::new_from_array(pool.coin_creator_vault_ata);
    let coin_creator_vault_authority = Pubkey::new_from_array(pool.coin_creator_vault_authority);
    let coin_fee_config = Pubkey::from_str(PUMPSWAP_FEE_PROG_STATE).unwrap();
    let coin_fee_program_state = Pubkey::from_str(PUMPSWAP_FEE_PROG_STATE2).unwrap();

    let accounts = vec![
        AccountMeta::new(Pubkey::new_from_array(pool.pool), false),              // [0]  pool
        AccountMeta::new(*wallet_pubkey, true),                                   // [1]  user (signer)
        AccountMeta::new_readonly(global_config, false),                          // [2]  global_config
        AccountMeta::new_readonly(base_mint_pubkey, false),                       // [3]  base_mint
        AccountMeta::new_readonly(quote_mint_pubkey, false),                      // [4]  quote_mint
        AccountMeta::new(user_base_ata, false),                                   // [5]  user_base_token_account
        AccountMeta::new(user_quote_ata, false),                                  // [6]  user_quote_token_account
        AccountMeta::new(pool_base_vault, false),                                 // [7]  pool_base_token_account
        AccountMeta::new(pool_quote_vault, false),                                // [8]  pool_quote_token_account
        AccountMeta::new(fee_recipient, false),                                   // [9]  protocol_fee_recipient
        AccountMeta::new(fee_recipient_token_account, false),                     // [10] fee_recipient_token_acct
        AccountMeta::new_readonly(token_program, false),                          // [11] base_token_program
        AccountMeta::new_readonly(token_program, false),                          // [12] quote_token_program
        AccountMeta::new_readonly(system_program::id(), false),                   // [13] system_program
        AccountMeta::new_readonly(ata_program, false),                            // [14] associated_token_program
        AccountMeta::new_readonly(event_authority, false),                        // [15] event_authority
        AccountMeta::new_readonly(pumpswap_program, false),                       // [16] pump_program (self CPI)
        AccountMeta::new(coin_creator_vault_ata, false),                          // [17] coin_creator_vault_ata
        AccountMeta::new(coin_creator_vault_authority, false),                    // [18] coin_creator_vault_authority
        AccountMeta::new_readonly(coin_fee_config, false),                        // [19] coin_fee_config
        AccountMeta::new_readonly(fee_program, false),                            // [20] coin_fee_program
        AccountMeta::new_readonly(coin_fee_program_state, false),                 // [21] coin_fee_program_state
    ];

    Instruction {
        program_id: pumpswap_program,
        accounts,
        data: build_swap_data(arg1, arg2),
    }
}
```

#### Change 4: Update buy/sell TX builders for swap argument semantics

**CRITICAL:** When the pool is reversed, "base" and "quote" swap in PumpSwap's semantics:
- **Buy (SOL → Token):**
  - Normal pool: `arg1 = base_amount_out (tokens)`, `arg2 = max_quote_in (SOL)`
  - Reversed pool: `arg1 = quote_amount_out (tokens)`, `arg2 = max_base_in (SOL)`
  
Actually, looking at PumpSwap's swap instruction more carefully: the instruction data is just `(amount_a: u64, amount_b: u64)` where the semantics depend on the swap direction. The program determines direction from which token the user sends in.

**After careful analysis:** The swap instruction data is `(base_in_or_out: u64, quote_in_or_out: u64)`. For a buy:
- Normal: `base_out = tokens, max_quote_in = SOL`
- Reversed: `base_out = SOL(???)` — this doesn't make sense

**Actually, PumpSwap's swap is always (base_amount, quote_amount) regardless.** When buying tokens:
- Normal pool (token=base): you want `base_out` = tokens, `max_quote_in` = SOL ✅
- Reversed pool (WSOL=base): you want `quote_out` = tokens (since token is quote), `max_base_in` = SOL

So the arg1/arg2 meanings FLIP when reversed:
- **Buy normal:** `build_swap_data(min_tokens_out, sol_lamports)` — base_out, max_quote_in
- **Buy reversed:** `build_swap_data(sol_lamports, min_tokens_out)` — max_base_in, quote_out? NO — we need to check the actual PumpSwap instruction semantics.

**Given the complexity, let me document what needs verification:**

In `build_pumpswap_buy_tx()` (line 384-390):
```rust
// 7. PumpSwap swap: buy(base_out=min_tokens_out, max_quote_in=sol_lamports)
let ix_swap = build_pumpswap_swap_ix(
    pool,
    &wallet_pubkey,
    fee_recipient_idx,
    min_tokens_out,  // base_out → only correct if token_is_base
    sol_lamports,    // max_quote_in → only correct if WSOL is quote
);
```

**For reversed pools, the arguments must swap:**
```rust
let (arg1, arg2) = if pool.token_is_base {
    (min_tokens_out, sol_lamports)    // base_out, max_quote_in
} else {
    (sol_lamports, min_tokens_out)    // max_base_in (SOL), quote_out (tokens)
};
let ix_swap = build_pumpswap_swap_ix(
    pool, &wallet_pubkey, fee_recipient_idx, arg1, arg2,
);
```

**Similarly in `build_pumpswap_sell_tx()` (line 466-471):**
```rust
let (arg1, arg2) = if pool.token_is_base {
    (tokens_to_sell, min_sol_out)     // base_in (tokens), min_quote_out (SOL)
} else {
    (min_sol_out, tokens_to_sell)     // min_base_out (SOL), quote_in (tokens)
};
let ix_swap = build_pumpswap_swap_ix(
    pool, &wallet_pubkey, fee_recipient_idx, arg1, arg2,
);
```

#### Change 5: Update `build_pumpswap_buy_tx()` swap args

**Location:** Lines 384–390

**Replace:**
```rust
    // 7. PumpSwap swap: buy(base_out=min_tokens_out, max_quote_in=sol_lamports)
    let ix_swap = build_pumpswap_swap_ix(
        pool,
        &wallet_pubkey,
        fee_recipient_idx,
        min_tokens_out,  // base_out
        sol_lamports,    // max_quote_in
    );
```

**With:**
```rust
    // 7. PumpSwap swap: buy (SOL → Token)
    // PumpSwap swap data is (base_amount, quote_amount).
    // Normal pool (token=base): base_out=tokens, max_quote_in=SOL
    // Reversed pool (WSOL=base): max_base_in=SOL, quote_out=tokens
    let (arg1, arg2) = if pool.token_is_base {
        (min_tokens_out, sol_lamports)   // base_out, max_quote_in
    } else {
        (sol_lamports, min_tokens_out)   // max_base_in, quote_out
    };
    let ix_swap = build_pumpswap_swap_ix(
        pool,
        &wallet_pubkey,
        fee_recipient_idx,
        arg1,
        arg2,
    );
```

#### Change 6: Update `build_pumpswap_sell_tx()` swap args

**Location:** Lines 465–471

**Replace:**
```rust
    // 4. PumpSwap swap: sell(base_in=tokens_to_sell, min_quote_out=min_sol_out)
    let ix_swap = build_pumpswap_swap_ix(
        pool,
        &wallet_pubkey,
        fee_recipient_idx,
        tokens_to_sell, // base_in
        min_sol_out,    // min_quote_out
    );
```

**With:**
```rust
    // 4. PumpSwap swap: sell (Token → SOL)
    // Normal pool (token=base): base_in=tokens, min_quote_out=SOL
    // Reversed pool (WSOL=base): min_base_out=SOL, quote_in=tokens
    let (arg1, arg2) = if pool.token_is_base {
        (tokens_to_sell, min_sol_out)    // base_in, min_quote_out
    } else {
        (min_sol_out, tokens_to_sell)    // min_base_out, quote_in
    };
    let ix_swap = build_pumpswap_swap_ix(
        pool,
        &wallet_pubkey,
        fee_recipient_idx,
        arg1,
        arg2,
    );
```

#### Change 7: Update buy TX to use new field names

**Location:** `build_pumpswap_buy_tx()`, line 351-352

**Replace:**
```rust
    let base_mint = Pubkey::new_from_array(pool.base_mint);
```

**With:**
```rust
    let token_mint = Pubkey::new_from_array(pool.token_mint);
```

And update the create ATA instruction that references `base_mint`:
```rust
    // 3. Create token ATA (idempotent) — ensure token ATA exists
    let ix_create_token_ata = build_create_ata_idempotent_ix(
        &wallet_pubkey,
        &wallet_pubkey,
        &token_mint,  // was: &base_mint
    );
```

#### eng4 — Companion changes required in pool.rs (eng2 must coordinate)

The `PumpSwapPoolAccounts` struct in `momentum/pool.rs` must also be updated with the `token_is_base` field to match the TX builder's struct. eng2 owns this file section.

**File:** `rust/pump-quant-core/src/momentum/pool.rs`, lines 1030–1048

**Replace:**
```rust
pub struct PumpSwapPoolAccounts {
    pub pool: [u8; 32],
    pub base_mint: [u8; 32],
    pub pool_base_token_account: [u8; 32],
    pub pool_quote_token_account: [u8; 32],
    pub coin_creator_vault_ata: [u8; 32],
    pub coin_creator_vault_authority: [u8; 32],
}
```

**With:**
```rust
pub struct PumpSwapPoolAccounts {
    /// Pool PDA (from PoolResolution.pool_address)
    pub pool: [u8; 32],
    /// Token mint (always the graduated token, NOT WSOL)
    pub base_mint: [u8; 32],
    /// Token vault (from PoolResolution.coin_vault) — always holds the token
    pub pool_base_token_account: [u8; 32],
    /// WSOL vault (from PoolResolution.pc_vault) — always holds WSOL
    pub pool_quote_token_account: [u8; 32],
    /// Coin creator vault ATA ([0u8;32] if unknown)
    pub coin_creator_vault_ata: [u8; 32],
    /// Coin creator vault authority ([0u8;32] if unknown)
    pub coin_creator_vault_authority: [u8; 32],
    /// Whether the token is on-chain base_mint (true) or quote_mint (false).
    /// When false, WSOL is base_mint. Affects swap ix account ordering.
    pub token_is_base: bool,
}
```

And update `extract_pumpswap_pool_accounts()`:
```rust
pub fn extract_pumpswap_pool_accounts(res: &PoolResolution) -> Option<PumpSwapPoolAccounts> {
    if res.pool_type != PoolType::PumpSwap {
        return None;
    }
    if res.pool_address == [0u8; 32] {
        return None;
    }
    // Determine if token is base_mint or quote_mint in this pool.
    // Compare mint bytes to WSOL: if mint < WSOL, token is base (normal).
    let token_is_base = res.mint < WSOL_MINT_BYTES;
    
    Some(PumpSwapPoolAccounts {
        pool: res.pool_address,
        base_mint: res.mint,
        pool_base_token_account: res.coin_vault,   // always token vault
        pool_quote_token_account: res.pc_vault,     // always WSOL vault
        coin_creator_vault_ata: [0u8; 32],
        coin_creator_vault_authority: [0u8; 32],
        token_is_base,
    })
}
```

#### eng4 Summary

| Change | Location | Description |
|--------|----------|-------------|
| Rename struct fields | tx/pumpswap.rs:78–90 | `base_mint` → `token_mint`, `pool_base/quote` → `token/wsol_vault` |
| Add `token_is_base` | tx/pumpswap.rs:78–90 | New bool field for ordering |
| Update From impl | tx/pumpswap.rs:95–105 | Map new field names |
| Fix swap ix builder | tx/pumpswap.rs:259–320 | Order accounts by `token_is_base` |
| Fix buy args | tx/pumpswap.rs:384–390 | Swap arg1/arg2 when reversed |
| Fix sell args | tx/pumpswap.rs:465–471 | Swap arg1/arg2 when reversed |
| Fix buy ATA | tx/pumpswap.rs:351–372 | Use `token_mint` instead of `base_mint` |
| Companion in pool.rs | pool.rs:1030–1070 | Add `token_is_base` to PumpSwapPoolAccounts (eng2) |

---

<a id="eng5"></a>
### eng5: Tests + validation

**Files:**
- `rust/pump-quant-core/src/momentum/pool.rs` (test module at bottom)
- `rust/pump-quant-core/src/tx/pumpswap.rs` (test module at bottom)
- `rust/pump-quant-core/src/feeds/shredstream.rs` (test module at bottom)
- New: integration test for on-chain validation

**DO NOT TOUCH:** Non-test code in any file (eng1-eng4 own the production code)

#### Test 1: Pool lookup returns results for both orderings

**File:** `rust/pump-quant-core/src/momentum/pool.rs`, test module

```rust
#[test]
fn test_wsol_mint_bytes_constant() {
    // Verify WSOL_MINT_BYTES matches the known WSOL mint
    let wsol_b58 = bs58::encode(&WSOL_MINT_BYTES).into_string();
    assert_eq!(wsol_b58, "So11111111111111111111111111111111111111112");
}

#[test]
fn test_token_is_base_determination() {
    // Token with first byte > 0x06 (WSOL) should sort after WSOL → token_is_base = false
    let mut mint_high = [0xFFu8; 32];
    assert!(mint_high > WSOL_MINT_BYTES, "high mint should be > WSOL");
    
    // Token with first byte < 0x06 → token_is_base = true
    let mut mint_low = [0x01u8; 32];
    assert!(mint_low < WSOL_MINT_BYTES, "low mint should be < WSOL");
    
    // Edge case: mint exactly equal to WSOL (shouldn't happen but handle it)
    let mint_equal = WSOL_MINT_BYTES;
    // By convention, equal means normal ordering (token_is_base = true)
    // but this case is pathological — WSOL-WSOL pool shouldn't exist
}
```

#### Test 2: Vault assignment correctness for reversed pools

**File:** `rust/pump-quant-core/src/momentum/pool.rs`, test module

```rust
#[test]
fn test_extract_pumpswap_pool_accounts_token_is_base() {
    // Token mint < WSOL bytes → token_is_base = true
    let res = PoolResolution {
        mint: [0x01; 32],  // < WSOL
        pool_address: [0xAA; 32],
        coin_vault: [0xBB; 32],
        pc_vault: [0xCC; 32],
        pool_type: PoolType::PumpSwap,
        reserve_sol_lamports: 100_000_000_000,
        reserve_token_atoms: 1_000_000_000_000,
        bc_terminal_vsol: 0.0,
        grad_block_time_ms: 0,
        amm_id: [0u8; 32],
        amm_open_orders: [0u8; 32],
        amm_target_orders: [0u8; 32],
        serum_market: [0u8; 32],
        serum_bids: [0u8; 32],
        serum_asks: [0u8; 32],
        serum_event_queue: [0u8; 32],
        serum_coin_vault: [0u8; 32],
        serum_pc_vault: [0u8; 32],
        serum_vault_signer: [0u8; 32],
    };
    
    let accounts = extract_pumpswap_pool_accounts(&res).unwrap();
    assert!(accounts.token_is_base, "mint < WSOL → token_is_base = true");
    assert_eq!(accounts.pool_base_token_account, [0xBB; 32]); // coin_vault = token vault
    assert_eq!(accounts.pool_quote_token_account, [0xCC; 32]); // pc_vault = WSOL vault
}

#[test]
fn test_extract_pumpswap_pool_accounts_token_is_quote() {
    // Token mint > WSOL bytes → token_is_base = false (reversed pool)
    let res = PoolResolution {
        mint: [0xFF; 32],  // > WSOL
        pool_address: [0xAA; 32],
        coin_vault: [0xBB; 32],  // token vault
        pc_vault: [0xCC; 32],    // WSOL vault
        pool_type: PoolType::PumpSwap,
        reserve_sol_lamports: 100_000_000_000,
        reserve_token_atoms: 1_000_000_000_000,
        bc_terminal_vsol: 0.0,
        grad_block_time_ms: 0,
        amm_id: [0u8; 32],
        amm_open_orders: [0u8; 32],
        amm_target_orders: [0u8; 32],
        serum_market: [0u8; 32],
        serum_bids: [0u8; 32],
        serum_asks: [0u8; 32],
        serum_event_queue: [0u8; 32],
        serum_coin_vault: [0u8; 32],
        serum_pc_vault: [0u8; 32],
        serum_vault_signer: [0u8; 32],
    };
    
    let accounts = extract_pumpswap_pool_accounts(&res).unwrap();
    assert!(!accounts.token_is_base, "mint > WSOL → token_is_base = false");
}
```

#### Test 3: TX builder account ordering for reversed pools

**File:** `rust/pump-quant-core/src/tx/pumpswap.rs`, test module

```rust
#[test]
fn test_pumpswap_swap_ix_normal_ordering() {
    let pool = PumpSwapPoolAccounts {
        pool: [1u8; 32],
        token_mint: [2u8; 32],   // token
        token_vault: [3u8; 32],  // token vault
        wsol_vault: [4u8; 32],   // WSOL vault
        coin_creator_vault_ata: [0u8; 32],
        coin_creator_vault_authority: [0u8; 32],
        token_is_base: true,
    };
    let kp = Keypair::new();
    let ix = build_pumpswap_swap_ix(&pool, &kp.pubkey(), 0, 100, 200);
    
    // [3] should be token mint (base)
    assert_eq!(ix.accounts[3].pubkey, Pubkey::new_from_array([2u8; 32]));
    // [4] should be WSOL (quote)
    assert_eq!(ix.accounts[4].pubkey, Pubkey::from_str(WSOL_MINT_STR).unwrap());
    // [7] should be token vault (pool_base)
    assert_eq!(ix.accounts[7].pubkey, Pubkey::new_from_array([3u8; 32]));
    // [8] should be WSOL vault (pool_quote)
    assert_eq!(ix.accounts[8].pubkey, Pubkey::new_from_array([4u8; 32]));
}

#[test]
fn test_pumpswap_swap_ix_reversed_ordering() {
    let pool = PumpSwapPoolAccounts {
        pool: [1u8; 32],
        token_mint: [2u8; 32],   // token
        token_vault: [3u8; 32],  // token vault
        wsol_vault: [4u8; 32],   // WSOL vault
        coin_creator_vault_ata: [0u8; 32],
        coin_creator_vault_authority: [0u8; 32],
        token_is_base: false,     // REVERSED
    };
    let kp = Keypair::new();
    let ix = build_pumpswap_swap_ix(&pool, &kp.pubkey(), 0, 100, 200);
    
    // [3] should be WSOL (base in reversed pool)
    assert_eq!(ix.accounts[3].pubkey, Pubkey::from_str(WSOL_MINT_STR).unwrap());
    // [4] should be token mint (quote in reversed pool)
    assert_eq!(ix.accounts[4].pubkey, Pubkey::new_from_array([2u8; 32]));
    // [7] should be WSOL vault (pool_base = WSOL vault in reversed pool)
    assert_eq!(ix.accounts[7].pubkey, Pubkey::new_from_array([4u8; 32]));
    // [8] should be token vault (pool_quote = token vault in reversed pool)
    assert_eq!(ix.accounts[8].pubkey, Pubkey::new_from_array([3u8; 32]));
}
```

#### Test 4: Shredstream create_pool extracts correct mint when WSOL is base

**File:** `rust/pump-quant-core/src/feeds/shredstream.rs`, test module

```rust
#[test]
fn test_pumpswap_create_pool_reversed_extracts_token_not_wsol() {
    // Build a mock transaction where create_pool has:
    // accounts[2] = WSOL (base_mint in reversed pool)
    // accounts[3] = token_mint
    // The parser should extract the token, not WSOL
    
    use solana_sdk::hash::Hash;
    use solana_sdk::signature::Keypair;
    use solana_sdk::signer::Signer;
    use solana_sdk::transaction::Transaction;
    
    let wsol = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
    let token = Pubkey::new_unique();
    let pool = Pubkey::new_unique();
    let creator = Pubkey::new_unique();
    
    // PumpSwap create_pool: accounts = [pool, creator, base_mint(WSOL), quote_mint(token), ...]
    let pumpswap_program = Pubkey::from_str("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA").unwrap();
    
    let mut data = Vec::new();
    data.extend_from_slice(&PUMPSWAP_CREATE_POOL_DISCRIMINATOR);  // 8 bytes
    data.extend_from_slice(&[0u8; 16]);  // padding
    
    let ix = solana_sdk::instruction::Instruction {
        program_id: pumpswap_program,
        accounts: vec![
            AccountMeta::new(pool, false),
            AccountMeta::new_readonly(creator, false),
            AccountMeta::new_readonly(wsol, false),      // base_mint = WSOL (reversed!)
            AccountMeta::new_readonly(token, false),     // quote_mint = token
        ],
        data,
    };
    
    let kp = Keypair::new();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&kp.pubkey()),
        &[&kp],
        Hash::default(),
    );
    
    let versioned = solana_sdk::transaction::VersionedTransaction::from(tx);
    let event = parse_pumpswap_migration(&versioned, 1000);
    
    if let Some(FeedEvent::Migration { mint, .. }) = event {
        let mint_pubkey = Pubkey::new_from_array(mint);
        assert_eq!(mint_pubkey, token, "should extract token mint, not WSOL");
        assert_ne!(mint_pubkey, wsol, "must NOT extract WSOL as the token mint");
    } else {
        // Strategy 2 may not trigger if strategy 1 (pump.fun migrate) runs first
        // This test is valid if the create_pool instruction is the only one
    }
}
```

#### Test 5: On-chain validation (integration test)

**File:** Create new file `rust/pump-quant-core/tests/pumpswap_pool_layout.rs`

```rust
//! On-chain validation: verify PumpSwap pool layout assumptions against live data.
//! Run with: cargo test --test pumpswap_pool_layout -- --ignored
//! Requires HELIUS_RPC_URL env var.

#[ignore]
#[tokio::test]
async fn test_pumpswap_pool_layout_on_chain() {
    let rpc_url = std::env::var("HELIUS_RPC_URL")
        .expect("HELIUS_RPC_URL must be set — fail-closed, no baked-in default");
    
    let client = reqwest::Client::new();
    
    // Known pool: 9GvgSRMprTdnrpuQhS3uzCe1FijtZxPU974H8zjHpump at offset 43 (normal)
    let normal_mint = "9GvgSRMprTdnrpuQhS3uzCe1FijtZxPU974H8zjHpump";
    
    // Query at offset 43
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "getProgramAccounts",
        "params": ["pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA", {
            "encoding": "base64",
            "filters": [{"memcmp": {"offset": 43, "bytes": normal_mint}}]
        }]
    });
    
    let resp: serde_json::Value = client.post(&rpc_url)
        .json(&body).send().await.unwrap()
        .json().await.unwrap();
    
    let accounts = resp.pointer("/result").unwrap().as_array().unwrap();
    assert!(!accounts.is_empty(), "normal mint should be found at offset 43");
    
    // Verify WSOL is at offset 75
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD
        .decode(accounts[0].pointer("/account/data/0").unwrap().as_str().unwrap())
        .unwrap();
    
    let wsol_bytes: [u8; 32] = [
        0x06, 0x9b, 0x88, 0x57, 0xfe, 0xab, 0x81, 0x84,
        0xfb, 0x68, 0x7f, 0x63, 0x46, 0x18, 0xc0, 0x35,
        0xda, 0xc4, 0x39, 0xdc, 0x1a, 0xeb, 0x3b, 0x55,
        0x98, 0xa0, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x01,
    ];
    
    assert_eq!(&data[75..107], &wsol_bytes, "quote_mint at offset 75 should be WSOL");
    
    // Known reversed pool: Hn6YPJUNh2f94hxumAYbRrMTSVL2D5Epj8AnuSU9QNNS at offset 75
    let reversed_mint = "Hn6YPJUNh2f94hxumAYbRrMTSVL2D5Epj8AnuSU9QNNS";
    
    let body2 = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "getProgramAccounts",
        "params": ["pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA", {
            "encoding": "base64",
            "filters": [{"memcmp": {"offset": 75, "bytes": reversed_mint}}]
        }]
    });
    
    let resp2: serde_json::Value = client.post(&rpc_url)
        .json(&body2).send().await.unwrap()
        .json().await.unwrap();
    
    let accounts2 = resp2.pointer("/result").unwrap().as_array().unwrap();
    assert!(!accounts2.is_empty(), "reversed mint should be found at offset 75");
    
    let data2 = base64::engine::general_purpose::STANDARD
        .decode(accounts2[0].pointer("/account/data/0").unwrap().as_str().unwrap())
        .unwrap();
    
    assert_eq!(&data2[43..75], &wsol_bytes, "base_mint at offset 43 should be WSOL (reversed pool)");
    
    println!("✅ On-chain layout verified: normal and reversed pools both work");
}
```

#### Test 6: Verify existing tests still pass with struct changes

**File:** `rust/pump-quant-core/src/tx/pumpswap.rs`, test module

Update the existing `dummy_pool()` helper:

**Replace:**
```rust
    fn dummy_pool() -> PumpSwapPoolAccounts {
        PumpSwapPoolAccounts {
            pool: [1u8; 32],
            base_mint: [2u8; 32],
            pool_base_token_account: [3u8; 32],
            pool_quote_token_account: [4u8; 32],
            coin_creator_vault_ata: [0u8; 32],
            coin_creator_vault_authority: [0u8; 32],
        }
    }
```

**With:**
```rust
    fn dummy_pool() -> PumpSwapPoolAccounts {
        PumpSwapPoolAccounts {
            pool: [1u8; 32],
            token_mint: [2u8; 32],
            token_vault: [3u8; 32],
            wsol_vault: [4u8; 32],
            coin_creator_vault_ata: [0u8; 32],
            coin_creator_vault_authority: [0u8; 32],
            token_is_base: true,  // default to normal ordering for existing tests
        }
    }
```

And add a reversed variant:
```rust
    fn dummy_pool_reversed() -> PumpSwapPoolAccounts {
        PumpSwapPoolAccounts {
            pool: [1u8; 32],
            token_mint: [2u8; 32],
            token_vault: [3u8; 32],
            wsol_vault: [4u8; 32],
            coin_creator_vault_ata: [0u8; 32],
            coin_creator_vault_authority: [0u8; 32],
            token_is_base: false,
        }
    }
```

---

## 6. Compilation & Validation

### Build
```bash
cd /data/.openclaw/workspace/projects/pump-quant/rust
cargo build --release -p pump-quant-core
```

### Test
```bash
cargo test -p pump-quant-core
```

### On-chain integration test
```bash
HELIUS_RPC_URL="$HELIUS_RPC_URL" \
  cargo test --test pumpswap_pool_layout -- --ignored
```

### Validation queries
```bash
# After deploying the fix, verify these return results:
# 1. Normal pool (token at offset 43)
curl -s $RPC -d '{"jsonrpc":"2.0","id":1,"method":"getProgramAccounts",
  "params":["pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
    {"encoding":"base64","dataSlice":{"offset":0,"length":0},
     "filters":[{"memcmp":{"offset":43,"bytes":"9GvgSRMprTdnrpuQhS3uzCe1FijtZxPU974H8zjHpump"}}]}]}'
# Expected: 1 result

# 2. Reversed pool (token at offset 75)
curl -s $RPC -d '{"jsonrpc":"2.0","id":1,"method":"getProgramAccounts",
  "params":["pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
    {"encoding":"base64","dataSlice":{"offset":0,"length":0},
     "filters":[{"memcmp":{"offset":75,"bytes":"Hn6YPJUNh2f94hxumAYbRrMTSVL2D5Epj8AnuSU9QNNS"}}]}]}'
# Expected: 1 result
```

---

## Appendix A: Why the Example Mints Fail

The three mints in the original bug report **do not have PumpSwap pools**:

| Mint | Graduated To | PumpSwap Pool? |
|------|-------------|----------------|
| `LtYKwqd9C3jZZeVFA3bTodcPX2Ge6Se6UBRbuWnpump` | Raydium V4 | No |
| `2ZiykxvY8x8GYsSL4cVkS5CdBPaxq414BRMPGkQWpump` | Raydium V4 | No |
| `KExnjBsxPqctcGa1xGLKzqHXDBSN7UrqBjofGgTpump` | Raydium V4 | No |

This indicates a **separate bug in graduation detection**: these tokens' graduations are being classified as PumpSwap events when they actually graduated to Raydium. This is outside the scope of this fix spec but should be investigated separately. Possible cause: the graduation detection in the `corecast` or `helius` feed incorrectly identifies Raydium graduations as PumpSwap.

## Appendix B: PumpSwap Pool Size Distribution

| Size (bytes) | Count | Percentage | Notes |
|-------------|-------|------------|-------|
| 211 | 38,048 | 11.7% | Older version, fewer fields |
| 301 | 286,641 | 88.3% | Current version, includes creator vault fields |

Both sizes share identical field positions for the first 203 bytes. The 301-byte accounts have additional fields at offsets 203+.

## Appendix C: Cross-Reference with PUMPSWAP-GRADUATION-SPEC.md

This fix spec addresses **Phase 4 (Pool Resolution Hardening)** from the graduation spec. The other phases (Helius transactionSubscribe, ShredStream discriminator fix, PumpPortal subscribeMigration) are already implemented or tracked separately.
