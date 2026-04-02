# PumpSwap Graduation Detection — Integration Spec

**Date:** 2026-04-01  
**Status:** Draft  
**Author:** Apollo (architect agent)  

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Root Cause Analysis](#2-root-cause-analysis)
3. [On-Chain Graduation Flow (Researched)](#3-on-chain-graduation-flow-researched)
4. [Option Evaluation](#4-option-evaluation)
5. [Recommended Architecture](#5-recommended-architecture)
6. [Implementation Spec — Phase 1: Helius transactionSubscribe](#6-implementation-spec--phase-1-helius-transactionsubscribe)
7. [Implementation Spec — Phase 2: ShredStream Discriminator Fix](#7-implementation-spec--phase-2-shredstream-discriminator-fix)
8. [Implementation Spec — Phase 3: PumpPortal subscribeMigration](#8-implementation-spec--phase-3-pumpportal-subscribemigration)
9. [Implementation Spec — Phase 4: Pool Resolution Hardening](#9-implementation-spec--phase-4-pool-resolution-hardening)
10. [PumpSwap Account Layout Reference](#10-pumpswap-account-layout-reference)
11. [Latency Analysis](#11-latency-analysis)
12. [Risk Analysis](#12-risk-analysis)
13. [Testing Strategy](#13-testing-strategy)
14. [Migration Plan](#14-migration-plan)

---

## 1. Executive Summary

**The problem:** Since April 2026, 100% of pump.fun graduations go to PumpSwap (program `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`), but our momentum engine trades **zero** of them due to four cascading failures:

1. **Wrong discriminator in ShredStream** — looking for `migrate_funds` (doesn't exist on PumpSwap) instead of `create_pool`
2. **Helius logsSubscribe → getTransaction race** — logsSubscribe fires at `processed` commitment, but getTransaction with `confirmed` commitment fails because the tx isn't confirmed yet
3. **Zero mint from logsSubscribe** — no account keys available, so fallback mint-based lookup can't run
4. **CoreCast sends only stale Raydium sigs** — no PumpSwap graduation events at all

**The fix (4 phases):**
- **Phase 1 (P0, immediate):** Add Helius Enhanced WebSocket `transactionSubscribe` on PumpSwap program — provides full tx with account keys, eliminates getTransaction round-trip entirely
- **Phase 2 (P0, immediate):** Fix ShredStream discriminator from `migrate_funds` → `create_pool` on PumpSwap program
- **Phase 3 (P1, 1-2 days):** Add PumpPortal `subscribeMigration` as a third independent feed
- **Phase 4 (P1, 1-2 days):** Harden pool resolution to handle PumpSwap-first flow without getTransaction fallback

**Expected outcome:** 100% graduation detection rate, ~200-400ms detection-to-entry latency (vs current: 0% detection rate).

---

## 2. Root Cause Analysis

### Problem 1: Helius logsSubscribe → getTransaction Fails

**Current flow:**
```
Helius logsSubscribe (processed) → detects "Instruction: MigrateFunds" log
  → mint=[0u8;32] (logsSubscribe has no account keys)
  → calls resolve_pool_from_transaction(sig) with getTransaction(sig, confirmed)
  → FAILS: tx is processed but not yet confirmed → result: null
  → 5 retries with exponential backoff (1s, 2s, 4s, 8s) = 15 seconds total
  → By retry 5, tx IS confirmed, but... wait, it still fails?
```

**Root cause:** The getTransaction call uses `"commitment": "confirmed"` (pool.rs line ~277). When logsSubscribe fires at `processed` commitment, the tx may not be `confirmed` for another 400-800ms. The retry backoff starts at 1000ms, which should catch it — but there's a **second issue**: the Helius standard RPC endpoint (`mainnet.helius-rpc.com`) may have indexing lag for recent transactions on the free/developer tier. The `confirmed` commitment for getTransaction doesn't mean "return at confirmed level" — it means "only return if the tx has reached confirmed level AND is indexed."

**Evidence:** All 12 Helius-detected PumpSwap graduations had `mint=[0u8;32]` and all 5 getTransaction retries returned null. This suggests a persistent indexing issue, not just a timing race.

**Contributing factor:** Even if getTransaction eventually succeeds, the 15-second retry cycle is far too slow for momentum trading. By then the price has moved 5-20%.

### Problem 2: ShredStream Uses Wrong Discriminator

**Current code** (`feeds/shredstream.rs:63-65`):
```rust
/// 8-byte Anchor discriminator for PumpSwap `migrate_funds` instruction.
/// SHA256("global:migrate_funds")[..8].
const PUMPSWAP_MIGRATE_DISCRIMINATOR: [u8; 8] = [42, 229, 10, 231, 189, 62, 193, 174];
```

**The issue:** There is **NO `migrate_funds` instruction** in the PumpSwap program. The PumpSwap IDL has exactly these instructions: `create_pool`, `buy`, `sell`, `deposit`, `withdraw`, `create_config`, `disable`, `extend_account`, `update_admin`, `update_fee_config`.

The graduation flow is:
1. Pump.fun program receives `migrate` instruction call (discriminator: `[155, 234, 231, 146, 236, 158, 162, 30]` = `sha256("global:migrate")[:8]`)
2. Pump.fun's `migrate` instruction CPI-calls PumpSwap's `create_pool` instruction
3. The `create_pool` discriminator is `[233, 146, 209, 142, 207, 104, 64, 188]` = `sha256("global:create_pool")[:8]`

**The current discriminator `migrate_funds` literally matches nothing on-chain.** This is why only 1 detection in 10 minutes (likely a false positive or a different program's instruction that happened to have a colliding discriminator prefix).

### Problem 3: CoreCast Sends Raydium Backlog

CoreCast emits ~1000 graduation events/minute, but they are ALL stale Raydium-era migration signatures. CoreCast hasn't been updated to send PumpSwap graduation events. These resolve to dead Raydium pools with zeroed Serum accounts, waste RPC credits, and produce no valid entries.

**Impact:** RPC credit drain + noise in logs + masks the real problem.

### Problem 4: Pool Type Misidentification

When `resolve_pool_from_transaction` successfully fetches a graduation tx, it identifies pool type by scanning `accountKeys` for program IDs:

```rust
let pool_type = if account_keys_strs.iter().any(|k| *k == RAYDIUM_AMM_V4_PROGRAM) {
    PoolType::RaydiumAmmV4
} else if account_keys_strs.iter().any(|k| *k == PUMPSWAP_AMM_PROGRAM) {
    PoolType::PumpSwap
} else {
    PoolType::Unknown
};
```

This is correct in theory, but PumpSwap graduation transactions may contain BOTH program IDs in the accountKeys (the pump.fun `migrate` instruction references both programs). The check prioritizes Raydium — if both are present, it picks Raydium. Since April 2026, Raydium should never be the actual pool.

**Fix:** Invert priority: check for PumpSwap first, then Raydium.

---

## 3. On-Chain Graduation Flow (Researched)

### Authoritative Sources
- **Pump.fun official docs:** [`pump-fun/pump-public-docs`](https://github.com/pump-fun/pump-public-docs)
- **PumpSwap README:** [`docs/PUMP_SWAP_README.md`](https://github.com/pump-fun/pump-public-docs/blob/main/docs/PUMP_SWAP_README.md)
- **Pump Program README:** [`docs/PUMP_PROGRAM_README.md`](https://github.com/pump-fun/pump-public-docs/blob/main/docs/PUMP_PROGRAM_README.md)

### Graduation Transaction Anatomy

A pump.fun → PumpSwap graduation is a **single Solana transaction** containing:

1. **Outer instruction:** Pump.fun program `migrate(user, mint)` 
   - Discriminator: `sha256("global:migrate")[:8]` = `[155, 234, 231, 146, 236, 158, 162, 30]` (hex: `9beae792ec9ea21e`)
   - Permissionless — anyone can call it on a completed bonding curve (`complete == true`, `real_token_reserves == 0`)
   - Idempotent — calling on already-migrated curve is a no-op

2. **CPI inner instruction:** PumpSwap program `create_pool(index, creator, baseMint, quoteMint, baseIn, quoteIn)`
   - Discriminator: `sha256("global:create_pool")[:8]` = `[233, 146, 209, 142, 207, 104, 64, 188]` (hex: `e992d18ecf6840bc`)
   - Creates a new PumpSwap AMM pool PDA from seeds `["pool", index, creator, baseMint, quoteMint]`
   - `index = 0` (canonical pool index for pump.fun migrations)
   - `baseMint` = graduated token mint
   - `quoteMint` = WSOL (`So11111111111111111111111111111111111111112`)
   - Deposits ~793.1B token atoms + ~85 SOL into the pool

3. **LP token burning:** The LP tokens minted during `create_pool` are burned, locking liquidity permanently.

### Program Log Pattern

The Solana runtime logs for a graduation tx look like:
```
Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]
Program log: Instruction: Migrate
...
Program pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA invoke [2]
Program log: Instruction: CreatePool
...
Program pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA success
...
Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success
```

**Key insight:** The correct log markers are:
- `"Instruction: Migrate"` (pump.fun program)
- `"Instruction: CreatePool"` (PumpSwap program, as CPI)
- NOT `"Instruction: MigrateFunds"` — this doesn't exist

### PumpSwap Pool State Layout

From the official PumpSwap docs and empirical analysis:

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0..8 | 8 | discriminator | `f19a6d0411b16dbc` (Anchor account discriminator) |
| 8 | 1 | pool_bump | PDA bump seed |
| 9..11 | 2 | index | Pool index (0 for pump.fun migrations) |
| 11..43 | 32 | creator | Pool creator pubkey |
| 43..75 | 32 | base_mint | Base token mint (the graduated token) |
| 75..107 | 32 | quote_mint | Quote token mint (WSOL) |
| 107..139 | 32 | lp_mint | LP token mint |
| 139..171 | 32 | pool_base_token_account | Token vault (base token ATA of pool) |
| 171..203 | 32 | pool_quote_token_account | WSOL vault (quote token ATA of pool) |
| 203..211 | 8 | lp_supply | Total LP supply (u64 LE) |
| 211+ | var | padding/extensions | May include `is_mayhem_mode` (bool, 1 byte) |

**IMPORTANT naming clarification:**
- PumpSwap: `base_mint` = graduated token, `quote_mint` = WSOL
- Our code: `coin_vault` = token vault, `pc_vault` = WSOL vault
- Mapping: `pool_base_token_account` → `coin_vault`, `pool_quote_token_account` → `pc_vault`

Our existing `resolve_pumpswap_pool_from_mint()` correctly reads these offsets (filters on `quote_mint` at offset 75 for WSOL, reads vaults from 139-203). Note: the code comment says it filters `quote_mint` at offset 75 for the token mint — **this is actually wrong in the comment but correct in practice** because it's using `memcmp` with the token mint at offset 75. Let me verify...

Actually, re-reading the code: our pool resolver filters with `{"memcmp": {"offset": 75, "bytes": mint_b58}}` where `mint_b58` is the **token mint**. According to the official layout, offset 75 is `quote_mint` = WSOL. But the code comment says "PumpSwap pools have the token as quote_mint (offset 75)."

**This is WRONG.** According to the official PumpSwap docs:
- `base_mint` (offset 43) = graduated token
- `quote_mint` (offset 75) = WSOL

Our filter should be at **offset 43** (base_mint), not offset 75 (quote_mint).

Wait — let me re-check the official example:
```json
"base_mint": "7LSsEoJGhLeZzGvDofTdNg7M3JttxQqGWNLo6vWMpump",
"quote_mint": "So11111111111111111111111111111111111111112"
```

So yes: `base_mint` = token, `quote_mint` = WSOL. Our filter at offset 75 is looking for the token mint at the `quote_mint` position, which is WSOL. **This means our mint-based PumpSwap pool lookup has been silently failing!**

**This is a critical bug** — fix the memcmp offset from 75 to 43.

---

## 4. Option Evaluation

### Option A: Second Helius logsSubscribe for PumpSwap Program

**Approach:** Subscribe to `logsSubscribe` with `mentions: ["pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"]` and look for `"Instruction: CreatePool"` in logs.

| Criterion | Rating | Notes |
|-----------|--------|-------|
| Detection reliability | ⭐⭐⭐ | Catches all PumpSwap create_pool events, not just pump.fun migrations |
| Account key availability | ⭐ | logsSubscribe still has NO account keys — same mint=[0;32] problem |
| Latency | ⭐⭐⭐ | ~200-400ms from block to notification |
| Implementation effort | ⭐⭐⭐⭐ | Minimal — add subscription, reuse existing handler |
| Cost | ⭐⭐⭐⭐ | Standard logsSubscribe, low credit usage |

**Verdict:** Insufficient on its own — still has the zero-mint problem. Would still need getTransaction fallback.

### Option B: Helius Enhanced WebSocket `transactionSubscribe` ⭐ RECOMMENDED

**Approach:** Use `transactionSubscribe` with `accountInclude: ["pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"]`, `transactionDetails: "full"`, `encoding: "jsonParsed"`. Provides the **complete transaction** including account keys, token balances, and log messages.

| Criterion | Rating | Notes |
|-----------|--------|-------|
| Detection reliability | ⭐⭐⭐⭐⭐ | Full tx with all accounts — can extract mint, vaults, pool directly |
| Account key availability | ⭐⭐⭐⭐⭐ | Full accountKeys + postTokenBalances in the notification |
| Latency | ⭐⭐⭐⭐ | ~200-400ms, same infrastructure as logsSubscribe |
| Implementation effort | ⭐⭐⭐ | New subscription type, but reuse existing vault extraction |
| Cost | ⭐⭐⭐ | 2 credits per 0.1 MB — PumpSwap has high tx volume, so filter tightly |

**Verdict:** **Best option.** Eliminates the getTransaction round-trip entirely. The full tx is in the notification — we can extract mint, vaults, and reserves immediately. This alone fixes Problems 1, 3, and 4.

### Option C: Extract Mint from PumpSwap Log Data

**Approach:** Parse the Anchor event/log data from PumpSwap's `CreatePool` instruction within the existing logsSubscribe.

| Criterion | Rating | Notes |
|-----------|--------|-------|
| Detection reliability | ⭐⭐ | Depends on PumpSwap emitting structured log data (not guaranteed) |
| Account key availability | ⭐⭐ | Would need to parse base64-encoded event data from logs |
| Latency | ⭐⭐⭐ | Same as logsSubscribe |
| Implementation effort | ⭐⭐ | Complex log parsing, fragile if PumpSwap changes event format |
| Cost | ⭐⭐⭐⭐ | Reuses existing subscription |

**Verdict:** Fragile and complex. Not recommended when Option B exists.

### Option D: PumpPortal `subscribeMigration` Webhook

**Approach:** Connect to `wss://pumpportal.fun/api/data` and send `{"method": "subscribeMigration"}`.

| Criterion | Rating | Notes |
|-----------|--------|-------|
| Detection reliability | ⭐⭐⭐⭐ | PumpPortal tracks pump.fun specifically — high reliability for pump.fun graduations |
| Account key availability | ⭐⭐⭐ | Likely provides mint address in the migration event |
| Latency | ⭐⭐⭐ | ~300-800ms (external service, additional hop) |
| Implementation effort | ⭐⭐⭐⭐ | Simple WebSocket, JSON messages |
| Cost | ⭐⭐⭐⭐⭐ | Free API |

**Verdict:** **Excellent secondary feed.** Provides mint directly, free, and specifically tracks pump.fun migrations. Use as a redundant secondary alongside Helius transactionSubscribe.

### Option E: Fix getTransaction Retry Logic

**Approach:** Change commitment to `processed`, increase timeout, add longer backoff.

| Criterion | Rating | Notes |
|-----------|--------|-------|
| Detection reliability | ⭐⭐ | Still depends on RPC indexing lag — bandaid fix |
| Latency | ⭐ | Even with faster retry, adds 500ms-2s latency |
| Implementation effort | ⭐⭐⭐⭐ | Simple config change |

**Verdict:** Bandaid. Does not fix the root cause (zero mint, no account keys from logsSubscribe).

### Option F: Direct PumpSwap Pool accountSubscribe

**Approach:** Use `accountSubscribe` on PumpSwap program accounts for pool creation.

| Criterion | Rating | Notes |
|-----------|--------|-------|
| Detection reliability | ⭐⭐ | Would catch pool creation but hard to filter — PumpSwap pools are created for many reasons |
| Latency | ⭐⭐⭐⭐ | Near-instant account change notification |
| Implementation effort | ⭐ | Hard to know which accounts to watch — pool PDA is derived from mint |

**Verdict:** Impractical. Pool address is unknown until creation.

### Final Ranking

| Priority | Option | Fix Phase |
|----------|--------|-----------|
| **P0** | **B: Helius transactionSubscribe** | Phase 1 — primary feed |
| **P0** | **ShredStream discriminator fix** | Phase 2 — fix existing feed |
| **P1** | **D: PumpPortal subscribeMigration** | Phase 3 — redundant secondary |
| **P1** | **Pool resolution hardening** | Phase 4 — defense in depth |
| Skip | A: Second logsSubscribe | Superseded by B |
| Skip | C: Log data parsing | Too fragile |
| Skip | E: getTransaction retry fix | Bandaid |
| Skip | F: accountSubscribe | Impractical |

---

## 5. Recommended Architecture

### New Graduation Detection Pipeline

```
┌─────────────────────────────────────────────────────────────────────┐
│                    GRADUATION DETECTION FEEDS                       │
│                                                                     │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐  │
│  │ Helius Enhanced   │  │ ShredStream gRPC │  │ PumpPortal WS    │  │
│  │ transactionSub    │  │ (block entries)  │  │ subscribeMigrate │  │
│  │                   │  │                  │  │                  │  │
│  │ PumpSwap program  │  │ create_pool disc │  │ migration events │  │
│  │ accountInclude    │  │ on PumpSwap pgm  │  │ with mint        │  │
│  │                   │  │                  │  │                  │  │
│  │ Full tx + keys ✅ │  │ Mint from ix ✅   │  │ Mint provided ✅  │  │
│  └────────┬─────────┘  └────────┬─────────┘  └────────┬─────────┘  │
│           │                     │                      │            │
│           └──────────┬──────────┴──────────────────────┘            │
│                      │                                              │
│                      ▼                                              │
│             ┌────────────────┐                                      │
│             │   Dedup Layer  │  resolving_sigs DashMap               │
│             │  (sig-based)   │  First-seen wins                     │
│             └───────┬────────┘                                      │
│                     │                                               │
│                     ▼                                               │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │              POOL RESOLUTION                                  │   │
│  │                                                               │   │
│  │  Path A: Full tx in notification (Helius Enhanced)            │   │
│  │    → Extract mint, vaults from postTokenBalances directly     │   │
│  │    → Detect pool type from accountKeys                        │   │
│  │    → Fetch vault reserves (getMultipleAccounts)               │   │
│  │    → Skip getTransaction entirely                             │   │
│  │                                                               │   │
│  │  Path B: Mint-only (ShredStream, PumpPortal)                  │   │
│  │    → Call resolve_pumpswap_pool_from_mint(mint)               │   │
│  │    → getProgramAccounts with memcmp at offset 43 (base_mint)  │   │
│  │    → Extract vaults from pool state                           │   │
│  │    → Fetch vault reserves (getMultipleAccounts)               │   │
│  │                                                               │   │
│  │  Path C: Fallback (sig-only, no mint)                         │   │
│  │    → resolve_pool_from_transaction(sig) with getTransaction   │   │
│  │    → Used only if other paths fail                            │   │
│  └──────────────────────────────────────────────────────────────┘   │
│                     │                                               │
│                     ▼                                               │
│             ┌────────────────┐                                      │
│             │  on_graduation │  Scorer → PendingEntry → Position     │
│             └────────────────┘                                      │
└─────────────────────────────────────────────────────────────────────┘
```

### Key Design Principles

1. **No getTransaction in the hot path.** The Helius Enhanced subscription provides the full tx inline. The ShredStream/PumpPortal paths use mint-based pool lookup (getProgramAccounts), which is faster and more reliable than getTransaction for fresh txs.

2. **Dedup at sig level.** All three feeds emit into the same `resolving_sigs` DashMap. First feed to deliver a sig wins; duplicates are dropped.

3. **PumpSwap-first pool type detection.** Invert the priority: check for PumpSwap before Raydium in accountKeys scanning.

4. **Fixed memcmp offset.** Change PumpSwap pool lookup from offset 75 → offset 43 (base_mint, not quote_mint).

---

## 6. Implementation Spec — Phase 1: Helius transactionSubscribe

### Overview

Replace the existing Helius `logsSubscribe` (which provides only logs + signature) with `transactionSubscribe` (which provides the full transaction including account keys, token balances, and metadata).

### File: `feeds/helius.rs`

#### New Constants

```rust
// Replace PUMPSWAP_LOG_MARKER:
// Old (wrong): const PUMPSWAP_LOG_MARKER: &[u8] = b"Instruction: MigrateFunds";
// New:
const PUMPSWAP_CREATE_POOL_LOG: &[u8] = b"Instruction: CreatePool";
const PUMP_MIGRATE_LOG: &[u8] = b"Instruction: Migrate";
const PUMPSWAP_AMM_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
```

#### New Subscription: `HeliusPumpSwapClient`

Add a **second** WebSocket client that uses `transactionSubscribe` specifically for PumpSwap graduation detection. Keep the existing `HeliusWsClient` for pump.fun trade pre-warming (buy/sell detection).

```rust
pub struct HeliusPumpSwapClient {
    config: HeliusConfig,
    engine_tx: Sender<FeedEvent>,
}

impl HeliusPumpSwapClient {
    pub fn new(config: HeliusConfig, engine_tx: Sender<FeedEvent>) -> Self {
        Self { config, engine_tx }
    }

    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run_loop().await;
        })
    }

    async fn run_loop(self) {
        if !self.config.enabled || self.config.api_key.is_empty() {
            info!("[helius_pumpswap] disabled or no API key — skipping");
            return;
        }

        let url = format!(
            "wss://mainnet.helius-rpc.com/?api-key={}",
            self.config.api_key
        );

        // transactionSubscribe: filter for PumpSwap program,
        // request full tx with jsonParsed encoding
        let sub_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "transactionSubscribe",
            "params": [
                {
                    "accountInclude": [PUMPSWAP_AMM_PROGRAM],
                    "failed": false
                },
                {
                    "commitment": "processed",
                    "encoding": "jsonParsed",
                    "transactionDetails": "full",
                    "maxSupportedTransactionVersion": 0
                }
            ]
        })
        .to_string();

        let mut backoff_ms: u64 = 1_000;

        loop {
            info!("[helius_pumpswap] connecting transactionSubscribe");

            match connect_async(&url).await {
                Err(e) => {
                    warn!("[helius_pumpswap] connect failed: {e} — retrying in {backoff_ms}ms");
                }
                Ok((ws_stream, _)) => {
                    backoff_ms = 1_000;
                    let (mut write, mut read) = ws_stream.split();

                    if let Err(e) = write.send(Message::Text(sub_msg.clone().into())).await {
                        error!("[helius_pumpswap] subscribe send failed: {e}");
                        continue;
                    }

                    info!("[helius_pumpswap] connected and subscribed (transactionSubscribe)");

                    // Ping every 30s to keep alive (Helius 10-min inactivity timer)
                    let ping_interval = tokio::time::interval(
                        tokio::time::Duration::from_secs(30)
                    );
                    tokio::pin!(ping_interval);

                    loop {
                        tokio::select! {
                            msg = read.next() => {
                                match msg {
                                    Some(Ok(Message::Text(text))) => {
                                        if let Some(event) = parse_pumpswap_transaction(&text) {
                                            if self.engine_tx.send(event).is_err() {
                                                info!("[helius_pumpswap] engine channel closed");
                                                return;
                                            }
                                        }
                                    }
                                    Some(Ok(Message::Ping(data))) => {
                                        let _ = write.send(Message::Pong(data)).await;
                                    }
                                    Some(Ok(Message::Close(_))) => {
                                        warn!("[helius_pumpswap] server sent close frame");
                                        break;
                                    }
                                    Some(Err(e)) => {
                                        warn!("[helius_pumpswap] ws error: {e}");
                                        break;
                                    }
                                    Some(Ok(_)) => {}
                                    None => {
                                        warn!("[helius_pumpswap] stream ended");
                                        break;
                                    }
                                }
                            }
                            _ = ping_interval.tick() => {
                                let _ = write.send(Message::Ping(vec![])).await;
                            }
                        }
                    }

                    warn!("[helius_pumpswap] disconnected — retrying in {backoff_ms}ms");
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
        }
    }
}
```

#### New Parser: `parse_pumpswap_transaction`

This is the critical function. It receives the full transaction from Helius Enhanced WebSockets and extracts everything we need — **no getTransaction call required**.

```rust
/// Parse a Helius transactionSubscribe notification for PumpSwap graduation.
///
/// The transactionNotification provides the FULL transaction including:
/// - transaction.transaction.message.accountKeys (with pubkey strings)
/// - meta.postTokenBalances (mint + vault addresses)
/// - meta.logMessages (instruction names)
/// - signature
///
/// This eliminates the need for a separate getTransaction RPC call.
///
/// Returns FeedEvent::Migration with mint extracted from postTokenBalances,
/// OR a new FeedEvent::PumpSwapGraduationDirect with pre-extracted pool info.
fn parse_pumpswap_transaction(text: &str) -> Option<FeedEvent> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    if v.get("method")?.as_str()? != "transactionNotification" {
        return None;
    }

    let result = v.pointer("/params/result")?;
    let tx = result.get("transaction")?;
    let meta = tx.get("meta")?;

    // Skip failed transactions
    if !meta.get("err")?.is_null() {
        return None;
    }

    // Check logs for graduation marker: "Instruction: CreatePool" from PumpSwap CPI
    let logs = meta.get("logMessages")?.as_array()?;
    let is_create_pool = logs.iter().any(|l| {
        l.as_str()
            .map(|s| memchr::memmem::find(s.as_bytes(), PUMPSWAP_CREATE_POOL_LOG).is_some())
            .unwrap_or(false)
    });

    if !is_create_pool {
        return None; // Not a pool creation — likely a swap, deposit, etc.
    }

    // Also verify it's a pump.fun migration (not a manual pool creation)
    let is_pump_migrate = logs.iter().any(|l| {
        l.as_str()
            .map(|s| memchr::memmem::find(s.as_bytes(), PUMP_MIGRATE_LOG).is_some())
            .unwrap_or(false)
    });

    if !is_pump_migrate {
        // CreatePool without Migrate = someone manually creating a PumpSwap pool
        // This is NOT a pump.fun graduation. Skip.
        return None;
    }

    // Extract signature
    let sig_str = result.get("signature")?.as_str()?;
    let mut sig = [0u8; 64];
    match bs58::decode(sig_str).onto(&mut sig[..]) {
        Ok(64) => {}
        _ => return None,
    }

    // Extract mint from postTokenBalances (first non-WSOL mint)
    let post_balances = meta.get("postTokenBalances")?.as_array()?;
    let mint_b58 = post_balances.iter().find_map(|entry| {
        let mint = entry.get("mint")?.as_str()?;
        if mint != "So11111111111111111111111111111111111111112" {
            Some(mint.to_string())
        } else {
            None
        }
    })?;

    let mut mint = [0u8; 32];
    match bs58::decode(&mint_b58).onto(&mut mint[..]) {
        Ok(32) => {}
        _ => return None,
    }

    // Extract vault addresses from postTokenBalances
    // Find the token vault (non-WSOL with highest balance) and WSOL vault
    let mut coin_vault = [0u8; 32];
    let mut pc_vault = [0u8; 32];
    let mut coin_vault_found = false;
    let mut pc_vault_found = false;

    for entry in post_balances {
        let entry_mint = entry.get("mint")?.as_str()?;
        let owner = entry.get("owner").and_then(|o| o.as_str()).unwrap_or("");
        let account_index = entry.get("accountIndex")?.as_u64()?;

        // Get the account pubkey from accountKeys using the index
        let account_keys = tx.pointer("/transaction/message/accountKeys")?
            .as_array()?;
        if (account_index as usize) >= account_keys.len() {
            continue;
        }
        let account_key = account_keys[account_index as usize]
            .as_str()
            .or_else(|| account_keys[account_index as usize]
                .get("pubkey")
                .and_then(|p| p.as_str())
            )?;

        let mut acct = [0u8; 32];
        if bs58::decode(account_key).onto(&mut acct[..]).ok()? != 32 {
            continue;
        }

        if entry_mint == mint_b58 && !coin_vault_found {
            coin_vault = acct;
            coin_vault_found = true;
        } else if entry_mint == "So11111111111111111111111111111111111111112" && !pc_vault_found {
            pc_vault = acct;
            pc_vault_found = true;
        }
    }

    if !coin_vault_found || !pc_vault_found {
        // Fallback: we have the mint, dispatch as Migration for mint-based resolution
        info!(
            sig = %sig_str,
            mint = %mint_b58,
            "[helius_pumpswap] graduation detected but vault extraction failed — using mint-based fallback"
        );

        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        return Some(FeedEvent::Migration {
            mint,
            ts_ms,
            source: MigrationSource::HeliusEnhanced,
            sig,
        });
    }

    info!(
        sig = %sig_str,
        mint = %mint_b58,
        coin_vault = %bs58::encode(&coin_vault).into_string(),
        pc_vault = %bs58::encode(&pc_vault).into_string(),
        "[helius_pumpswap] graduation detected with full vault resolution — fast path"
    );

    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // NEW: Emit a richer event with pre-extracted pool data.
    // This skips the entire pool resolution step in on_migration().
    Some(FeedEvent::PumpSwapGraduationDirect {
        mint,
        sig,
        ts_ms,
        coin_vault,
        pc_vault,
        source: MigrationSource::HeliusEnhanced,
    })
}
```

#### New FeedEvent Variant

**File: `feeds/mod.rs`** (or wherever FeedEvent is defined)

```rust
pub enum FeedEvent {
    // ... existing variants ...
    PreWarm(PreWarmEvent),
    Migration {
        mint: [u8; 32],
        ts_ms: u64,
        source: MigrationSource,
        sig: [u8; 64],
    },
    /// NEW: PumpSwap graduation with pre-extracted pool data.
    /// Emitted by Helius Enhanced transactionSubscribe.
    /// Skips getTransaction — vaults already extracted from notification.
    PumpSwapGraduationDirect {
        mint: [u8; 32],
        sig: [u8; 64],
        ts_ms: u64,
        coin_vault: [u8; 32],
        pc_vault: [u8; 32],
        source: MigrationSource,
    },
}

pub enum MigrationSource {
    // ... existing variants ...
    PumpPortal,
    CoreCast,
    HeliusLogs,
    ShredStream,
    /// NEW: Helius Enhanced transactionSubscribe
    HeliusEnhanced,
    /// NEW: PumpPortal subscribeMigration
    PumpPortalMigration,
}
```

#### New Handler in Engine

**File: `momentum/mod.rs`** — add handler for `PumpSwapGraduationDirect` in the main event loop:

```rust
FeedEvent::PumpSwapGraduationDirect {
    mint, sig, ts_ms, coin_vault, pc_vault, source
} => {
    // Fast path: skip pool resolution — vaults already extracted
    if engine.resolving_sigs.contains_key(&sig) {
        continue; // dedup
    }
    engine.resolving_sigs.insert(sig, ts_ms);

    // Fetch vault reserves (one RPC call — getMultipleAccountsInfo)
    let coin_vault_b58 = bs58::encode(&coin_vault).into_string();
    let pc_vault_b58 = bs58::encode(&pc_vault).into_string();

    match pool::fetch_vault_reserves(
        &engine.http_client,
        &engine.helius_rpc_url,
        &coin_vault_b58,
        &pc_vault_b58,
    ).await {
        Some((reserve_token, reserve_sol)) => {
            if reserve_sol < pool::MIN_PUMPSWAP_SOL_RESERVES_LAMPORTS {
                tracing::warn!(
                    mint = %bs58::encode(&mint).into_string(),
                    reserve_sol,
                    "[momentum] PumpSwap pool rejected — insufficient liquidity"
                );
                continue;
            }

            // Derive pool PDA (for swap instruction building)
            let pool_address = derive_pumpswap_pool_pda(&mint);

            let pool_info = pool::PoolInfo {
                coin_vault,
                pc_vault,
                reserve_token,
                reserve_sol,
                pool_type: pool::PoolType::PumpSwap,
                mint,
            };

            // Look up enrichment from hot_path mint_map
            let enrichment = get_enrichment_for_mint(&mint);

            engine.on_graduation(
                &pool_info,
                ts_ms,
                enrichment.grad_speed_s,
                enrichment.volume_sol_x100,
                enrichment.buys_5s,
                enrichment.sells_5s,
                enrichment.is_cold_miss,
            ).await;
        }
        None => {
            tracing::warn!(
                mint = %bs58::encode(&mint).into_string(),
                "[momentum] PumpSwapGraduationDirect vault reserves fetch failed"
            );
            // Fallback: try mint-based resolution
            if let Some(resolution) = pool::resolve_pumpswap_pool_from_mint(
                &engine.http_client, &mint, &engine.helius_rpc_url
            ).await {
                // ... same graduation flow with resolution ...
            }
        }
    }
}
```

#### Existing logsSubscribe Changes

Update the log markers in the existing `check_graduation_logs` function:

```rust
// Old:
const PUMPSWAP_LOG_MARKER: &[u8] = b"Instruction: MigrateFunds";

// New:
const PUMPSWAP_LOG_MARKER: &[u8] = b"Instruction: CreatePool";
```

This fixes the existing logsSubscribe to correctly detect PumpSwap graduations as a backup (though it still has the zero-mint issue).

#### Credit Cost Analysis

Helius Enhanced WSS charges **2 credits per 0.1 MB** of streamed data. PumpSwap processes ~50-100 transactions/minute (swaps + pool creations + deposits/withdrawals). Only pool creation txs are relevant (~5-10/minute for pump.fun graduations).

The `accountInclude` filter runs server-side, so we receive ALL PumpSwap transactions (not just `CreatePool`). Our client-side filter (`parse_pumpswap_transaction`) drops non-graduation txs.

Estimated data volume: ~100 txs/min × ~2KB/tx = ~200KB/min = ~12MB/hour.
Credit cost: ~12MB/h × 20 credits/MB = ~240 credits/hour = ~5,760 credits/day.

This is within Helius Developer plan limits (1M credits/day), but monitor closely.

**Optimization:** If credit usage is too high, add a second `accountRequired` filter to require BOTH PumpSwap and pump.fun programs:

```json
{
    "accountInclude": ["pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"],
    "accountRequired": [
        "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
        "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
    ]
}
```

This ensures only transactions involving BOTH programs are streamed — effectively filtering to pump.fun graduation txs only.

---

## 7. Implementation Spec — Phase 2: ShredStream Discriminator Fix

### Overview

Fix the ShredStream `parse_pumpswap_migration()` to detect the correct instruction discriminator.

### File: `feeds/shredstream.rs`

#### Change 1: Fix Discriminator Constant

```rust
// OLD (WRONG — "migrate_funds" doesn't exist on PumpSwap):
/// 8-byte Anchor discriminator for PumpSwap `migrate_funds` instruction.
/// SHA256("global:migrate_funds")[..8].
const PUMPSWAP_MIGRATE_DISCRIMINATOR: [u8; 8] = [42, 229, 10, 231, 189, 62, 193, 174];

// NEW — two discriminators to check:

/// 8-byte Anchor discriminator for PumpSwap `create_pool` instruction.
/// SHA256("global:create_pool")[..8]. This is what PumpSwap actually executes
/// when pump.fun's `migrate` CPI-calls pool creation.
const PUMPSWAP_CREATE_POOL_DISCRIMINATOR: [u8; 8] = [233, 146, 209, 142, 207, 104, 64, 188];

/// 8-byte Anchor discriminator for pump.fun `migrate` instruction.
/// SHA256("global:migrate")[..8]. This is the outer instruction on the pump.fun
/// program that triggers the PumpSwap CPI.
const PUMPFUN_MIGRATE_DISCRIMINATOR: [u8; 8] = [155, 234, 231, 146, 236, 158, 162, 30];
```

#### Change 2: Dual Detection Strategy

The graduation tx has TWO relevant instructions:
1. **Outer:** pump.fun `migrate` (program `6EF8...`) — top-level instruction, visible in `tx.message.instructions()`
2. **Inner CPI:** PumpSwap `create_pool` (program `pAMM...`) — inner instruction, visible only in `meta.inner_instructions`

ShredStream provides raw entries which may include the outer instruction but NOT inner instructions. So we should match on the **pump.fun `migrate` discriminator** (outer), not PumpSwap `create_pool` (inner CPI).

However, we should also check inner instructions if available in the ShredStream entry data.

```rust
fn parse_pumpswap_migration(
    tx: &solana_sdk::transaction::VersionedTransaction,
    now_ms: u64,
) -> Option<FeedEvent> {
    let account_keys = tx.message.static_account_keys();
    let instructions = tx.message.instructions();

    // Strategy 1: Look for pump.fun's `migrate` instruction (outer)
    for ix in instructions {
        let program_id_index = ix.program_id_index as usize;
        if program_id_index >= account_keys.len() {
            continue;
        }

        // Check if this is a pump.fun program instruction
        if account_keys[program_id_index] != PUMPFUN_PROGRAM_PUBKEY {
            continue;
        }

        if ix.data.len() < 8 {
            continue;
        }

        // Match pump.fun `migrate` discriminator
        if ix.data[..8] != PUMPFUN_MIGRATE_DISCRIMINATOR {
            continue;
        }

        // pump.fun `migrate` instruction:
        // accounts[0] = user (signer)
        // accounts[1] = mint  ← EXTRACT THIS
        // accounts[2] = bonding_curve
        // ... (many more accounts for CPI to PumpSwap)
        let mint = if ix.accounts.len() > 1 {
            let mint_idx = ix.accounts[1] as usize;
            if mint_idx < account_keys.len() {
                account_keys[mint_idx].to_bytes()
            } else {
                [0u8; 32]
            }
        } else {
            [0u8; 32]
        };

        let sig: [u8; 64] = if !tx.signatures.is_empty() {
            tx.signatures[0].into()
        } else {
            continue;
        };

        let mint_b58 = bs58::encode(&mint).into_string();
        tracing::info!(
            mint = %mint_b58,
            sig = %&bs58::encode(&sig).into_string()[..8],
            "[shredstream] PumpSwap graduation detected via pump.fun migrate"
        );

        return Some(FeedEvent::Migration {
            mint,
            ts_ms: now_ms,
            source: MigrationSource::ShredStream,
            sig,
        });
    }

    // Strategy 2: Look for PumpSwap `create_pool` (may appear as top-level in some entries)
    for ix in instructions {
        let program_id_index = ix.program_id_index as usize;
        if program_id_index >= account_keys.len() {
            continue;
        }

        if account_keys[program_id_index] != PUMPSWAP_PROGRAM_PUBKEY {
            continue;
        }

        if ix.data.len() < 8 || ix.data[..8] != PUMPSWAP_CREATE_POOL_DISCRIMINATOR {
            continue;
        }

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

        let sig: [u8; 64] = if !tx.signatures.is_empty() {
            tx.signatures[0].into()
        } else {
            continue;
        };

        return Some(FeedEvent::Migration {
            mint,
            ts_ms: now_ms,
            source: MigrationSource::ShredStream,
            sig,
        });
    }

    None
}
```

#### Change 3: Add PUMPFUN_PROGRAM_PUBKEY Constant

```rust
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

lazy_static::lazy_static! {
    static ref PUMPFUN_PROGRAM_PUBKEY: Pubkey =
        Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P").unwrap();
}
```

(Or use `const` with byte array if the existing code uses that pattern.)

---

## 8. Implementation Spec — Phase 3: PumpPortal subscribeMigration

### Overview

Add PumpPortal's free WebSocket API as a third independent graduation feed.

### New File: `feeds/pumpportal.rs`

```rust
//! PumpPortal WebSocket feed — migration event subscription.
//!
//! Subscribes to pump.fun graduation (migration) events via PumpPortal's
//! free public WebSocket API at `wss://pumpportal.fun/api/data`.
//!
//! Provides mint address directly in the event payload, eliminating
//! the need for getTransaction. Used as a redundant secondary feed
//! alongside Helius Enhanced transactionSubscribe and ShredStream.

use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn, error};

use crate::feeds::{FeedEvent, MigrationSource};

const PUMPPORTAL_WS_URL: &str = "wss://pumpportal.fun/api/data";
const MAX_BACKOFF_MS: u64 = 30_000;

pub struct PumpPortalConfig {
    pub enabled: bool,
}

pub struct PumpPortalMigrationClient {
    config: PumpPortalConfig,
    engine_tx: Sender<FeedEvent>,
}

impl PumpPortalMigrationClient {
    pub fn new(config: PumpPortalConfig, engine_tx: Sender<FeedEvent>) -> Self {
        Self { config, engine_tx }
    }

    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run_loop().await;
        })
    }

    async fn run_loop(self) {
        if !self.config.enabled {
            info!("[pumpportal_migration] disabled — skipping");
            return;
        }

        let sub_msg = serde_json::json!({
            "method": "subscribeMigration"
        }).to_string();

        let mut backoff_ms: u64 = 1_000;

        loop {
            info!("[pumpportal_migration] connecting to {}", PUMPPORTAL_WS_URL);

            match connect_async(PUMPPORTAL_WS_URL).await {
                Err(e) => {
                    warn!("[pumpportal_migration] connect failed: {e}");
                }
                Ok((ws_stream, _)) => {
                    backoff_ms = 1_000;
                    let (mut write, mut read) = ws_stream.split();

                    if let Err(e) = write.send(Message::Text(sub_msg.clone().into())).await {
                        error!("[pumpportal_migration] subscribe send failed: {e}");
                        continue;
                    }

                    info!("[pumpportal_migration] subscribed to migration events");

                    loop {
                        match read.next().await {
                            Some(Ok(Message::Text(text))) => {
                                if let Some(event) = parse_migration_event(&text) {
                                    if self.engine_tx.send(event).is_err() {
                                        return;
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(data))) => {
                                let _ = write.send(Message::Pong(data)).await;
                            }
                            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                            _ => {}
                        }
                    }

                    warn!("[pumpportal_migration] disconnected");
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
        }
    }
}

/// Parse a PumpPortal migration event.
///
/// Expected payload (based on PumpPortal API docs — exact format TBD, verify empirically):
/// ```json
/// {
///     "mint": "...",
///     "signature": "...",
///     "pool": "...",           // PumpSwap pool address (if available)
///     "timestamp": 1234567890,
///     ...
/// }
/// ```
fn parse_migration_event(text: &str) -> Option<FeedEvent> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    // PumpPortal migration events should contain a mint address
    let mint_b58 = v.get("mint").and_then(|m| m.as_str())?;

    let mut mint = [0u8; 32];
    match bs58::decode(mint_b58).onto(&mut mint[..]) {
        Ok(32) => {}
        _ => return None,
    }

    // Extract signature if available
    let mut sig = [0u8; 64];
    if let Some(sig_str) = v.get("signature").and_then(|s| s.as_str()) {
        match bs58::decode(sig_str).onto(&mut sig[..]) {
            Ok(64) => {}
            _ => {} // sig stays zeroed — use mint-based resolution
        }
    }

    let ts_ms = v.get("timestamp")
        .and_then(|t| t.as_u64())
        .map(|t| if t < 1_000_000_000_000 { t * 1_000 } else { t }) // handle sec vs ms
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64
        });

    info!(
        mint = %mint_b58,
        "[pumpportal_migration] graduation detected"
    );

    Some(FeedEvent::Migration {
        mint,
        ts_ms,
        source: MigrationSource::PumpPortalMigration,
        sig,
    })
}
```

**Note:** The exact PumpPortal migration event payload format needs to be verified empirically by connecting to `wss://pumpportal.fun/api/data` and observing the messages. The parser should be adjusted after the first test run.

---

## 9. Implementation Spec — Phase 4: Pool Resolution Hardening

### Fix 1: PumpSwap memcmp Offset (CRITICAL BUG)

**File: `momentum/pool.rs` — `resolve_pumpswap_pool_from_mint()`**

```rust
// OLD (WRONG — offset 75 is quote_mint = WSOL, not the token):
{"memcmp": {"offset": 75, "bytes": mint_b58}}

// NEW (CORRECT — offset 43 is base_mint = the graduated token):
{"memcmp": {"offset": 43, "bytes": mint_b58}}
```

This is a **critical bug** — the current filter searches for the token mint at the WSOL position, which will never match. This means `resolve_pumpswap_pool_from_mint()` has been silently returning `None` for all queries.

### Fix 2: PumpSwap-First Pool Type Detection

**File: `momentum/pool.rs` — `resolve_pool_inner()`**

```rust
// OLD (Raydium prioritized):
let pool_type = if account_keys_strs.iter().any(|k| *k == RAYDIUM_AMM_V4_PROGRAM) {
    PoolType::RaydiumAmmV4
} else if account_keys_strs.iter().any(|k| *k == PUMPSWAP_AMM_PROGRAM) {
    PoolType::PumpSwap
} else {
    PoolType::Unknown
};

// NEW (PumpSwap prioritized — all graduations go to PumpSwap since April 2026):
let pool_type = if account_keys_strs.iter().any(|k| *k == PUMPSWAP_AMM_PROGRAM) {
    PoolType::PumpSwap
} else if account_keys_strs.iter().any(|k| *k == RAYDIUM_AMM_V4_PROGRAM) {
    PoolType::RaydiumAmmV4
} else {
    PoolType::Unknown
};
```

### Fix 3: Mint-Based Resolution as Primary Path

When a `FeedEvent::Migration` arrives with a non-zero mint (from ShredStream or PumpPortal), skip `resolve_pool_from_transaction` entirely and go directly to `resolve_pumpswap_pool_from_mint`:

**File: `momentum/mod.rs` — `on_migration()`**

Add this block before the `resolve_pool_from_transaction` call:

```rust
// NEW: Fast path — if we have a mint, try PumpSwap pool lookup directly.
// This avoids the getTransaction round-trip which is slow and often fails.
if mint != [0u8; 32] {
    if let Some(resolution) = crate::momentum::pool::resolve_pumpswap_pool_from_mint(
        &self.http_client, &mint, &self.helius_rpc_url
    ).await {
        if resolution.reserve_sol_lamports >= crate::momentum::pool::MIN_PUMPSWAP_SOL_RESERVES_LAMPORTS {
            tracing::info!(
                mint = %bs58::encode(&mint).into_string(),
                reserve_sol = resolution.reserve_sol_lamports,
                "[momentum] PumpSwap pool resolved via mint-based lookup (fast path)"
            );
            // ... proceed to on_graduation with resolution ...
            // (same code as the existing resolution success path)
            return;
        }
    }
    // Mint-based lookup failed or insufficient liquidity — fall through to getTransaction
}

// Existing getTransaction fallback path continues here...
match resolve_pool_from_transaction(&self.http_client, &sig, &self.rpc_url).await { ... }
```

### Fix 4: Use `processed` Commitment for getTransaction on Helius-detected Sigs

When the source is `HeliusLogs` (processed commitment), use `processed` commitment for getTransaction:

```rust
let commitment = match source {
    MigrationSource::HeliusLogs | MigrationSource::HeliusEnhanced => "processed",
    _ => "confirmed",
};

let body = serde_json::json!({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "getTransaction",
    "params": [
        sig_b58,
        {
            "encoding": "jsonParsed",
            "maxSupportedTransactionVersion": 0,
            "commitment": commitment
        }
    ]
});
```

### Fix 5: Existing logsSubscribe Log Marker Fix

```rust
// Old:
const PUMPSWAP_LOG_MARKER: &[u8] = b"Instruction: MigrateFunds";

// New (detect either the CPI instruction or the outer pump.fun instruction):
const PUMPSWAP_LOG_MARKER: &[u8] = b"Instruction: CreatePool";
// Also add:
const PUMPFUN_MIGRATE_MARKER: &[u8] = b"Instruction: Migrate";
```

Update `logs_contain_graduation_marker`:

```rust
fn logs_contain_graduation_marker(logs: &[serde_json::Value]) -> bool {