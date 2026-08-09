# Helius LaserStream Server-Side Filtering — Quant Feasibility Report & Enhancement Proposal

**Date:** 2026-08-08
**Author:** Principal Quant (Hermes Agent)
**Status:** APPROVED — held for implementation until 24h paper-trade data collection completes (~2026-08-09 18:48 UTC)
**Scope:** LaserStream gRPC + Helius WS server-side filter optimization for pump.fun/Solana memecoin trading
**Hold reason:** 24h data-gathering mandate — no code/strategy/config changes until paper-trade baseline collected

---

## I. Executive Summary

**Server-side filtering is not only possible — our bot already uses a subset of it, and the unused remainder contains the highest-impact credit optimizations available.** More critically, our deep architecture review uncovered a **strategic blind spot**: we decode the mayhem-mode flag in our protocol layer but **never gate on it in the strategy/app layer**. Mayhem-mode tokens are reaching our entry pipeline, consuming compute cycles and on-chain confirmation credits, only to be traded and likely exited at a loss because mayhem mode alters the token's economics in ways our TP ladder doesn't model.

The optimizations below are ranked by **net SOL per trade impact**, not just credit savings — because getting the right data faster is worth more than paying for less data.

---

## II. Current Architecture: Three-Lane Ingest

Our bot runs three concurrent ingest lanes:

| Lane | Transport | What It Delivers | Server-Side Filters Used | Credits? |
|------|-----------|------------------|--------------------------|----------|
| **PumpPortal WS** | WebSocket | Trade events (buy/sell/migration) for all pump.fun tokens | None (firehose) | Free |
| **Helius WS** | WebSocket | `transactionSubscribe` (pump programs) + per-mint `accountSubscribe` (bonding curves) + `slotSubscribe` (heartbeat) | `accountInclude` (pump programs), `vote: false`, `failed: false` | Free tier |
| **LaserStream gRPC** | gRPC (Yellowstone) | Transaction stream + account updates + slot notifications | `accountInclude` (pump programs), `owner` (PumpSwap pools), `vote: false`, `failed: false` | **Paid credits** |

**Credit-bearing lane = LaserStream gRPC only.** The WS lanes are free-tier and don't consume LaserStream credits. All optimizations below target either (a) reducing LaserStream credit consumption or (b) improving data quality/timeliness on the free WS lanes via Helius enhanced-WS filtering.

---

## III. Deep Finding: The Mayhem-Mode Gap

### The problem

Our protocol layer (`pump-quant-protocol/src/decode.rs` and `pumpswap.rs`) fully decodes three mayhem-related fields:

| Account | Offset | Field | Type |
|---------|--------|-------|------|
| BondingCurve (pump.fun) | 81 | `is_mayhem_mode` | bool (optional tail) |
| Pool (PumpSwap) | 243 | `is_mayhem_mode` | bool (optional tail) |
| GlobalConfig (PumpSwap) | 417 | `mayhem_mode_enabled` | bool (global switch) |

**But `grep -rn 'mayhem' crates/pump-quant-app/src/` returns EMPTY.** The strategy engine — the gate that decides whether to enter a trade — never checks the mayhem flag. We decode it, store it in the struct, and then ignore it.

### Why this matters for net SOL per trade

Mayhem mode fundamentally changes pump.fun token economics:
- **Different fee structure** — mayhem tokens route fees differently through the `pfee` program, with dynamic market-cap-tiered rates that our TP ladder doesn't model
- **Different liquidity dynamics** — mayhem mode affects how the bonding curve behaves near graduation, changing the expected move profile
- **Our TP ladder assumes standard bonding-curve economics** — TP1 at +10% (in bps), TP2, TP3, and the 10% moon-bag retention are calibrated for non-mayhem tokens. A mayhem token hitting our TP1 threshold may not actually realize that gain after mayhem-specific fee extraction

**Impact estimate:** If even 10% of our admitted trades are mayhem-mode tokens that we wouldn't have entered with a mayhem gate, and those trades have a worse win rate than the cohort (likely — the fee structure eats the edge), then excluding them server-side improves both net SOL per trade AND credit efficiency.

### Three-layer mayhem filtering strategy

**Layer 1 — Global switch (cheapest, broadest):**
Subscribe to the PumpSwap `GlobalConfig` PDA (`ADyA8hkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ`) via LaserStream account subscription with `memcmp` at offset 417. If `mayhem_mode_enabled = false` globally, no per-token mayhem filtering is needed at all — zero mayhem tokens exist. If `true`, activate Layer 2.

**Layer 2 — Server-side memcmp on bonding-curve account subscriptions:**
Add a `memcmp` filter at offset 81, value `0x00` (non-mayhem) to the LaserStream account subscription for pump.fun-owned bonding curves:
```rust
SubscribeRequestFilterAccounts {
    owner: vec![PUMP_PROGRAM],
    filters: vec![
        Filter::Memcmp { offset: 81, bytes: vec![0] },  // non-mayhem only
    ],
    notify_on: NotifyOn::Write,
}
```
This tells the LaserStream server: "Only send me bonding-curve account updates where byte 81 = 0 (non-mayhem)." Mayhem-mode token account updates are filtered server-side — **never transmitted, never credited**.

**Critical nuance — this only works for accounts, not transactions.** The `SubscribeRequestFilterTransactions` message does not support `memcmp` on account data. Transaction filtering is by `accountInclude`/`accountExclude` (pubkey lists) only. So:

**Layer 3 — Client-side mayhem blocklist for transactions:**
When we decode a `create_v2` instruction from the transaction stream (which contains the mayhem flag in the instruction data), we extract the mint address and add it to a local blocklist. This blocklist feeds `account_exclude` on a periodically refreshed transaction subscription, causing the server to stop sending transactions involving known-mayhem mints.

**BUT** — LaserStream subscriptions are static at subscription time. Dynamic `account_exclude` updates require either:
- (a) Using the SDK's subscription management to drop and re-create the subscription with updated exclude lists (disruptive — causes a brief stream gap)
- (b) Accepting mayhem tokens in the transaction stream and filtering client-side (cheaper to implement, costs some credits but avoids stream gaps)

**Recommendation:** Layer 1 + Layer 2 are the high-impact, low-complexity wins. Layer 3 is a marginal credit optimization that risks stream gaps — defer until we have data on what fraction of transactions touch mayhem mints.

---

## IV. Server-Side Filter Inventory: Used vs. Available

### LaserStream gRPC (proto-verified from our compiled `geyser.rs`)

The compiled proto at `tools/stream-capture-rs/grpc-server-only/target/.../geyser.rs` confirms these filter types are available:

| Filter | Proto Type | We Use? | Credit Impact | Net-SOL Impact |
|--------|-----------|---------|---------------|----------------|
| `account_include` (tx) | `Vec<String>` | ✅ pump programs | Core filter — reduces tx to pump-only | Critical — without it we'd get all txs |
| `account_exclude` (tx) | `Vec<String>` | ❌ | Removes noise accounts from tx stream | Medium — see Layer 3 above |
| `vote` (tx) | `Option<bool>` | ✅ false | Skips vote txs | Saves ~40% of raw tx volume |
| `failed` (tx) | `Option<bool>` | ✅ false | Skips failed txs | Saves ~5-10% of tx volume |
| `owner` (accounts) | `Vec<String>` | ✅ PumpSwap pools | Filters accounts by owner program | Critical for pool tracking |
| **`filters` (accounts)** | `Vec<Filter>` | ❌ **NOT USED** | **memcmp + datasize + lamports** | **HIGH — enables mayhem filtering** |
| **`cuckoo_accounts_filter`** | `Option<CuckooFilter>` | ❌ **NOT USED** | Compact PDA tracking (3-4 bytes/key) | **HIGH — replaces 256 WS subs** |
| **`accounts_data_slice`** | `Vec<DataSlice>` | ❌ **NOT USED** | Partial account data (saves bandwidth) | **MEDIUM — see below** |
| `commitment` | `i32` | ✅ Processed | Fastest/cheapest | Correct for our timing |
| **`notify_on`** | (Helius ext) | ❌ **NOT USED** | Skips no-op write-lock updates | **MEDIUM — ~5% noise reduction** |

### Helius Enhanced WS (free tier)

| Filter | We Use? | Impact |
|--------|---------|--------|
| `accountInclude` (transactionSubscribe) | ✅ pump programs | Core filter |
| `vote: false` | ✅ | Skips vote txs |
| `failed: false` | ✅ | Skips failed txs |
| `encoding: base64` | ✅ | Full transaction data |
| `transactionDetails: full` | ✅ | Full tx with inner instructions |
| `maxSupportedTransactionVersion: 0` | ✅ | Legacy + v0 support |
| `accountSubscribe` encoding | ✅ base64 | Full account data |
| **`accountSubscribe` with `encoding: base64+json` + `filters`** | ❌ | Helius enhanced-WS supports memcmp filters on accountSubscribe (Developer plan+) |

---

## V. Optimization Proposals (Ranked by Net-SOL-per-Trade Impact)

### 🔴 OPT-1: Mayhem-mode gate in strategy layer + server-side memcmp filter

**Problem:** We decode `is_mayhem_mode` but never gate on it. Mayhem tokens enter our pipeline, consume on-chain confirmation credits, get traded, and likely underperform because our TP ladder models standard economics.

**Solution (two parts):**

*Part A — Strategy gate (code change in `pump-quant-app/src/engine.rs`):*
Add a new reject code `REJECT_MAYHEM_MODE: u8 = 19` and gate on `is_mayhem_mode == Some(true)` at the entry decision point. When the bonding-curve tail decode reveals mayhem mode, reject the candidate before any on-chain confirmation is requested.

*Part B — Server-side filter (code change in `grpc-server-only/src/main.rs`):*
Add `memcmp` filter at offset 81, value `0x00` to the pump.fun bonding-curve account subscription. This prevents mayhem-mode bonding-curve account updates from being transmitted at all.

**Net-SOL impact:** Direct — every mayhem token that would have been admitted and traded at a loss is now excluded pre-entry. Based on our 20% win rate and the fact that mayhem tokens likely underperform, this could improve the win rate on the remaining cohort.

**Credit impact:** Proportional to mayhem token prevalence. If 15% of new tokens are mayhem-mode, that's a 15% reduction in account-update credits for the bonding-curve subscription.

**Complexity:** Low. Part A is ~10 lines in engine.rs. Part B is ~5 lines in main.rs.

**HOLD NOTE:** Per the 24h data-gathering mandate, this is proposed but not implemented. When the hold lifts, this is the #1 priority change.

---

### 🔴 OPT-2: `notifyOn: Write` on account subscriptions

**Problem:** LaserStream sends account updates even when the write-lock is taken but the data hasn't changed (no-op updates). These consume credits without delivering new information.

**Solution:** Add `notify_on: Some(NotifyOn::Write as i32)` to `SubscribeRequestFilterAccounts` in `main.rs`.

**Net-SOL impact:** Indirect — reduces processing overhead for no-op updates, freeing CPU time for real signal processing. Doesn't change what data we receive, just eliminates redundant copies.

**Credit impact:** ~5% of account updates are no-op duplicates (per Helius docs). Small but free.

**Complexity:** 1 line.

---

### 🔴 OPT-3: Cashback-coin filtering via memcmp at offset 82

**Problem:** Cashback coins (`is_cashback_coin = true` at byte offset 82 on bonding curves) have a different fee distribution model. Our fee modeling assumes standard pump.fun economics. If we trade cashback coins, our round-trip cost estimates may be wrong, leading to mispriced TP thresholds.

**Solution:** Add a second `memcmp` filter at offset 82, value `0x00` (non-cashback) to the same account subscription that filters mayhem mode. Both filters apply as AND conditions server-side.

**Net-SOL impact:** Prevents trades where our fee model is wrong — wrong fee model means wrong cost floor, wrong TP1 threshold, wrong expected-net. Every cashback-coin trade we avoid is a trade where our edge calculation was invalid.

**Credit impact:** Additional reduction in account-update volume, proportional to cashback-coin prevalence.

**Complexity:** 1 additional filter in the same `filters` vec.

**Verification needed:** We should confirm via the 24h data whether cashback coins appear in our promoted candidates. If they do, this is high-value. If the pump.fun ecosystem has moved fully to cashback by now, this filter would exclude everything and must NOT be applied. **Flag for post-hold analysis.**

---

### 🟡 OPT-4: `accounts_data_slice` — partial account data delivery

**Problem:** We currently receive full account data on every update. But our bonding-curve decoder only reads offsets 0-48 (core) and 49-82 (tail) — that's 83 bytes of a ~115-byte account. The remaining bytes (quote_mint at 83-114, plus any padding) are transmitted but never consumed by our decoder.

**Solution:** Use `accounts_data_slice` to request only the byte ranges we actually decode:
- Slice 1: offset 0, length 48 (discriminator + reserves + total supply + complete flag)
- Slice 2: offset 49, length 36 (creator + mayhem + cashback + quote_mint partial)

This reduces per-update payload by ~30% (from ~115 bytes to ~84 bytes).

**Net-SOL impact:** Marginal direct — we don't lose any data we use. Indirect — faster parsing, less bandwidth, lower per-message credit cost (LaserStream bills by data volume).

**Credit impact:** ~25-30% reduction in per-account-update data volume. Since LaserStream credits scale with data transmitted, this is a direct credit saving.

**Complexity:** Medium — requires restructuring the `SubscribeRequest` to include `accounts_data_slice` entries, and the emit path must handle partial data. The bonding-curve decoder already bounds-checks every field, so partial data that stops short of offset 48 would return `None` (fail-closed) — we need to ensure our slices cover the minimum decode length.

**Risk:** If pump.fun appends new fields beyond offset 114, we won't see them until we update the slices. Mitigated by the fact that our decoder is length-tolerant and fields are only ever appended.

---

### 🟡 OPT-5: Cuckoo filter for bulk bonding-curve PDA tracking

**Problem:** We currently maintain up to 256 individual `accountSubscribe` WS subscriptions for bonding-curve PDAs (one per watched mint). Each subscription consumes a WS connection slot and generates per-mint traffic. When we hit the 256 cap, we evict dormant mints — but the eviction is reactive, not predictive.

**Solution:** Replace the 256 individual WS `accountSubscribe` calls with a single LaserStream gRPC account subscription that uses a cuckoo filter to track all active bonding-curve PDAs. The cuckoo filter is a compressed probabilistic structure (3-4 bytes per key vs 32 bytes per pubkey) that tells the server "send me updates for any of these N thousand accounts."

**Net-SOL impact:** Significant indirect — we can track ALL active bonding curves simultaneously (not just 256), meaning we never miss a bonding-curve state change due to eviction. More complete data = better entry decisions = higher net SOL per trade.

**Credit impact:** Replaces 256 WS subscriptions (free tier) with 1 gRPC subscription (paid). The credit cost depends on the number of active bonding curves and update frequency. **This may INCREASE total credit consumption** if we're tracking more accounts than before — but the data quality improvement justifies it.

**Complexity:** High — requires:
1. Maintaining a cuckoo filter of active mint PDAs (updated as mints are discovered and graduated)
2. Modifying the gRPC `SubscribeRequest` to include the cuckoo filter
3. Removing or reducing the WS `accountSubscribe` path
4. The LaserStream SDK (`helius-laserstream` 0.2.0+) supports cuckoo filters natively (confirmed in proto)

**Recommendation:** Defer to post-24h-hold. This is a structural change to the ingest architecture, not a filter tweak. Worth doing, but needs the 24h data to quantify the eviction rate and data-loss impact before committing.

---

### 🟡 OPT-6: `account_exclude` for known noise accounts in transaction stream

**Problem:** Pump.fun transactions often touch auxiliary accounts that carry no trading signal: Metaplex metadata accounts, SPL token program accounts for mint/transfer authority, system program accounts for rent. These transactions pass our `account_include` filter (because they also touch pump.fun programs) but the auxiliary account interactions are noise.

**Solution:** Build a static exclude list of known noise programs and add them to `account_exclude`:
- Metaplex metadata program (`meta...`)
- SPL token program (`Token...`)
- System program (`111...`)
- Memo program (`Memo...`)

**BUT** — this is WRONG. These programs are touched by EVERY pump.fun transaction. Excluding them would filter out ALL pump.fun transactions because `account_exclude` removes transactions where ANY listed account appears, and every pump.fun buy/sell touches the SPL token program.

**Correction:** `account_exclude` in LaserStream/Yellowstone filters transactions where the excluded account is present — it's an exclusion filter on the account KEY LIST, not on the program being called. Since SPL token accounts appear in virtually every pump.fun transaction, excluding the SPL token PROGRAM would drop everything.

**Revised approach:** Instead of excluding programs, exclude specific auxiliary account PUBKEYS that appear in noise transactions (e.g., specific Metaplex metadata accounts for tokens we don't care about). This requires maintaining a dynamic exclude list, which is operationally complex.

**Net-SOL impact:** Low — our `account_include` filter on pump programs already does the heavy lifting. The remaining noise is transactions that touch both pump programs AND auxiliary accounts, which still carry our trading signal.

**Recommendation:** Skip. The `account_include` filter is sufficient. `account_exclude` at the program level would be counterproductive.

---

### 🟡 OPT-7: Cashback-coin gate in strategy layer (complement to OPT-3)

**Problem:** Even if we don't filter cashback coins server-side (pending the verification noted in OPT-3), we should gate on `is_cashback_coin` in the strategy layer. If our fee model doesn't account for cashback fee distribution, trades on cashback coins have mispriced expected-net.

**Solution:** Add `REJECT_CASHBACK_COIN: u8 = 20` reject code and gate on `is_cashback_coin == Some(true)` at entry.

**Net-SOL impact:** Prevents trades where the fee model is invalid. Same logic as mayhem — if the coin's economics differ from what we model, the edge calculation is wrong.

**Complexity:** ~10 lines in engine.rs, mirrors OPT-1 Part A.

**HOLD NOTE:** Same as OPT-1 — proposed, not implemented per 24h mandate.

---

### 🟢 OPT-8: Helius enhanced-WS `accountSubscribe` with memcmp filters

**Problem:** Our free-tier WS `accountSubscribe` calls send full account data on every update with no server-side content filtering. We decode it all client-side.

**Solution:** Helius Developer plan+ supports `accountSubscribe` with `encoding: "base64+json"` and server-side `filters` (memcmp). We could add the same `memcmp` at offset 81 (mayhem) to our WS account subscriptions, filtering mayhem-mode bonding curves at the WS layer too.

**Net-SOL impact:** Mirrors OPT-1 but on the free WS lane. No credit impact (WS is free tier), but reduces client-side processing load.

**Complexity:** Low — modify `account_subscribe_request()` in `helius_ws.rs` to include filter params.

**Caveat:** This requires the Developer plan or above. If we're on the free tier, this filter isn't available on WS — only on LaserStream gRPC. Need to verify our Helius plan tier.

---

### 🟢 OPT-9: `account_required` for multi-account transaction filtering

**Problem:** Some transactions touch the pump.fun program but aren't buy/sell instructions — they could be `create`, `create_v2`, `migrate`, or other administrative instructions. These pass our `account_include` filter but carry no trading signal for our entry/exit pipeline.

**Solution:** Use `account_required` to require that a transaction touches BOTH the pump.fun program AND a bonding-curve PDA (or pool account). This filters out pure administrative transactions (like `create`/`create_v2`) that touch the pump program but no specific bonding curve.

**BUT** — `create_v2` transactions DO touch a bonding-curve PDA (the one being created). And we NEED to see `create_v2` transactions to detect mayhem-mode tokens at birth (Layer 3 of the mayhem strategy). Filtering them out would break mayhem detection.

**Net-SOL impact:** Negative — we'd lose `create_v2` instructions that we need for mayhem blocklist construction.

**Recommendation:** Skip. The transaction volume after `account_include` filtering is already manageable.

---

### ❌ OPT-10 (NOT FEASIBLE): Server-side mcap-band filtering

**Problem:** Our 118-263 SOL mcap band is the core entry gate. Could we filter server-side?

**Analysis:** Market cap is computed from `virtual_sol_reserves * 10^6 / virtual_token_reserves` (or equivalent formula using the bonding-curve constants). These are u64 values at offsets 8-15 (virtual_token) and 16-23 (virtual_sol) in the bonding-curve account.

A `memcmp` filter matches exact byte sequences at a fixed offset. Market cap is a COMPUTED value from TWO fields — it cannot be expressed as a single `memcmp` on one field. We'd need to filter on the ratio of two u64 values, which is not a memcmp operation.

**Alternative — lamports filter:** The proto supports `SubscribeRequestFilterAccountsFilterLamports` with comparison operators (`eq`, `lt`, `gt`, `le`, `ge`). But this filters on the account's LAMPORT BALANCE (the SOL rent balance), not on virtual reserves. Bonding-curve accounts typically have ~0.2 SOL rent — this doesn't correlate with market cap.

**Conclusion:** Not feasible. Mcap filtering must remain client-side. The server cannot compute derived values.

---

### ❌ OPT-11 (NOT FEASIBLE): Server-side TP-ladder filtering

**Problem:** Could the server filter out tokens whose price won't reach our TP1 threshold?

**Analysis:** TP1 reachability depends on: (a) current bonding-curve price (from reserves), (b) our entry price (which we haven't determined yet at filter time), (c) round-trip costs (fees + slippage), and (d) the model's estimated upside. None of these are server-filterable — they're computed client-side from multiple inputs.

**Conclusion:** Not feasible. TP-ladder filtering is inherently client-side.

---

### ⚠️ OPT-12 (DEFER): Server-side `complete` flag filtering

**Problem:** Could we filter out bonding curves that have `complete = true` (graduated/migrated) to avoid receiving updates for dead curves?

**Analysis:** The `complete` flag is at offset 48 (1 byte, bool). We COULD use `memcmp` at offset 48, value `0x00` to filter for non-complete curves only. This would stop updates for graduated curves.

**BUT** — we NEED to see the `complete` transition (the moment `complete` flips from 0 to 1) because that's the migration event. If we filter server-side for `complete = 0`, we'd never receive the update where `complete` becomes 1 — we'd miss the migration signal.

**Alternative:** We could filter for `complete = 0` AND separately subscribe to PumpSwap pool creation events (via the `owner` filter on PumpSwap pools). The migration is then detected by the pool creation, not by the `complete` flag flip.

**Net-SOL impact:** Medium — reduces bonding-curve update volume for graduated tokens (which keep sending updates even after migration if they have residual activity). But risks missing the migration transition if the pool-creation subscription has latency.

**Recommendation:** Feasible but risky. Defer to post-hold analysis. Only implement if the 24h data shows significant credit waste from post-graduation bonding-curve updates.

---

## VI. Implementation Priority Matrix

| Priority | OPT | Net-SOL Impact | Credit Savings | Complexity | Status |
|----------|-----|----------------|----------------|------------|--------|
| 🔴 P0 | OPT-1A: Mayhem strategy gate | **HIGH** — prevents bad trades | N/A (strategy) | Low | **HOLD** — post 24h |
| 🔴 P0 | OPT-1B: Mayhem memcmp filter | **HIGH** — prevents mayhem data ingestion | Proportional to mayhem prevalence | Low | **HOLD** — post 24h |
| 🔴 P0 | OPT-2: notifyOn:Write | Low | ~5% account updates | 1 line | **HOLD** — post 24h |
| 🔴 P1 | OPT-3: Cashback memcmp filter | **HIGH** — prevents mispriced-fee trades | Proportional to cashback prevalence | 1 line | **HOLD** — verify prevalence first |
| 🔴 P1 | OPT-7: Cashback strategy gate | **HIGH** — mirrors OPT-1A for cashback | N/A | Low | **HOLD** — post 24h |
| 🟡 P2 | OPT-4: accounts_data_slice | Marginal | ~25-30% per-update data | Medium | **HOLD** — post 24h |
| 🟡 P3 | OPT-5: Cuckoo filter | Indirect — better data completeness | Replaces WS overhead, may increase gRPC cost | High | **HOLD** — needs 24h eviction data |
| 🟢 P4 | OPT-8: WS memcmp filters | Marginal | N/A (free tier) | Low | **HOLD** — verify plan tier |
| ❌ | OPT-6: account_exclude | Negative | Counterproductive | — | **SKIP** |
| ❌ | OPT-9: account_required | Negative | Loses mayhem detection | — | **SKIP** |
| ❌ | OPT-10: Server-side mcap | — | Not feasible | — | **N/A** |
| ❌ | OPT-11: Server-side TP | — | Not feasible | — | **N/A** |
| ⚠️ | OPT-12: complete flag filter | Medium | Medium | Low | **DEFER** — needs data |

---

## VII. Bonding-Curve Account Layout Reference (Verified Against Code)

```
Offset  Size  Field                        Type      Filterable?
0       8     anchor discriminator         [u8;8]    ✅ memcmp (identity check)
8       8     virtual_token_reserves       u64 LE    ❌ (part of mcap formula — can't filter on ratio)
16      8     virtual_sol_reserves         u64 LE    ❌ (part of mcap formula)
24      8     real_token_reserves          u64 LE    ❌
32      8     real_sol_reserves            u64 LE    ❌
40      8     token_total_supply           u64 LE    ❌
48      1     complete                     bool      ⚠️ memcmp possible but loses migration signal
49      32    creator                      Pubkey    ❌ (no filter value)
81      1     is_mayhem_mode               bool      ✅ memcmp → 0x00 (KEY FILTER)
82      1     is_cashback_coin             bool      ✅ memcmp → 0x00 (KEY FILTER)
83      32    quote_mint                   Pubkey    ❌ (could filter for WSOL-only, but complex)
```

**Combined server-side filter for bonding-curve account subscription:**
```rust
SubscribeRequestFilterAccounts {
    owner: vec![PUMP_PROGRAM.to_string()],
    filters: vec![
        // Mayhem mode = false (offset 81)
        Filter::Memcmp { offset: 81, bytes: vec![0] },
        // Cashback coin = false (offset 82) — VERIFY PREVALENCE FIRST
        Filter::Memcmp { offset: 82, bytes: vec![0] },
    ],
    notify_on: Some(NotifyOn::Write as i32),
}
```

---

## VIII. PumpSwap Pool Account Layout (Post-Migration)

```
Offset  Size  Field                        Type      Filterable?
0       8     anchor discriminator         [u8;8]    ✅ memcmp
8       1     pool_bump                    u8        ❌
9       2     index                        u16 LE    ❌
11      32    creator                      Pubkey    ❌
43      32    base_mint                    Pubkey    ❌ (would need known mint list)
75      32    quote_mint                   Pubkey    ⚠️ could filter for WSOL-only pools
107     32    lp_mint                      Pubkey    ❌
139     32    pool_base_token_account      Pubkey    ❌
171     32    pool_quote_token_account     Pubkey    ❌
203     8     lp_supply                    u64 LE    ❌
211     32    coin_creator                 Pubkey    ❌
243     1     is_mayhem_mode               bool      ✅ memcmp → 0x00
244     1     is_cashback_coin             bool      ✅ memcmp → 0x00
```

---

## IX. Global Mayhem Switch (PumpSwap GlobalConfig)

```
Account: ADyA8hkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ
Offset  Size  Field                        Type
0       8     discriminator                [u8;8]
...     ...   (fixed fields through 313)   ...
417     1     mayhem_mode_enabled          bool      ✅ memcmp → check global state
```

**Usage:** Subscribe to this account once. If `mayhem_mode_enabled = false`, skip all per-token mayhem filtering (no mayhem tokens exist). If `true`, activate per-token `memcmp` filters.

---

## X. Proposed Implementation Order (Post-24h-Hold)

**Phase 1 — Immediate (when hold lifts):**
1. OPT-1A: Add `REJECT_MAYHEM_MODE = 19` gate in `engine.rs`
2. OPT-1B: Add `memcmp` filter at offset 81 → `0x00` in `grpc-server-only/src/main.rs`
3. OPT-2: Add `notifyOn: Write` to account subscriptions in `main.rs`
4. Rebuild and redeploy `pq-laserstream-grpc` + `pq-daemon`

**Phase 2 — After verifying cashback prevalence in 24h data:**
5. OPT-3 + OPT-7: If cashback coins appear in promoted candidates, add `memcmp` at offset 82 + strategy gate

**Phase 3 — Structural improvements:**
6. OPT-4: Add `accounts_data_slice` for partial account data
7. OPT-5: Cuckoo filter migration (if eviction data warrants)

---

## XI. What This Report Does NOT Address (Out of Scope)

- **TP ladder recalibration** — the 24h data will inform whether TP1/TP2/TP3 thresholds need adjustment; this is a strategy change, not a filtering optimization
- **Entry timing optimization** — LaserStream `Processed` vs `Confirmed` commitment tradeoff; `Processed` is correct for our latency budget
- **Shred Delivery migration** — premature; our $9k-$20k mcap entry timing doesn't require sub-slot latency
- **WS lane elimination** — the three-lane architecture (PumpPortal + Helius WS + LaserStream gRPC) provides redundancy; eliminating a lane removes a source of truth

---

## XII. Reddit Intelligence Summary

From `r/solanadev` (scraped via Firecrawl):
- A Rust MEV dev building a Pump.fun bot got "killed by pay-as-you-go overage fees" — burned $26 in hours on accidental `getAccountInfo` spam
- Community recommends split-stack architecture (gRPC stream from one provider, REST from free-tier)
- **No Reddit threads found** specifically about LaserStream credit optimization, mayhem mode filtering, or server-side memcmp filters — this is genuinely niche territory

---

## XIII. Bottom Line

**The single highest-impact optimization is mayhem-mode filtering** — both as a server-side `memcmp` filter (OPT-1B, prevents mayhem data from consuming credits) and as a strategy-layer gate (OPT-1A, prevents mayhem trades from being entered). We currently decode the flag but never act on it — a strategic blind spot that likely contributes to our 20% win rate and -404M lamport net session result.

**The second highest-impact optimization is cashback-coin filtering** (OPT-3 + OPT-7), pending verification of cashback-coin prevalence in our 24h data.

**Credit-specific optimizations** (`notifyOn`, `accounts_data_slice`, cuckoo filters) are real but secondary — they reduce cost per trade, while the mayhem/cashback gates improve the trade selection itself.

All changes held per the 24h data-gathering mandate. Ready to implement Phase 1 the moment the hold lifts.

---

## Appendix: Key Files Referenced

| File | Role |
|------|------|
| `tools/stream-capture-rs/grpc-server-only/src/main.rs` | LaserStream gRPC server — **target for memcmp + notifyOn additions** |
| `tools/stream-capture-rs/src/helius_ws.rs` | Helius WS lane — transactionSubscribe + accountSubscribe builders |
| `crates/pump-quant-app/src/engine.rs` | Strategy engine — **target for mayhem/cashback reject gates** |
| `crates/pump-quant-protocol/src/decode.rs` | Bonding-curve decoder (offset 81 = mayhem, 82 = cashback) |
| `crates/pump-quant-protocol/src/pumpswap.rs` | PumpSwap Pool + GlobalConfig decoder (offset 243/417 = mayhem) |
| `tools/stream-capture-rs/grpc-server-only/target/.../geyser.rs` | Compiled proto — confirms Memcmp, CuckooFilter, DataSlice, NotifyOn available |

---

*Document frozen 2026-08-08. Review post 24h paper-trade data collection (~2026-08-09 18:48 UTC).*
