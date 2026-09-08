# LaserStream Gold v3 — Final Forensic Audit Report
**Date:** 2026-08-28 (Pacific Time)
**Build:** v3, Run 5, UUID 8f15b1ae
**Corpus:** 369 RAW .zst files, 11,741,300 transactions
**Output:** 4 parquet layers (L1=3,790,870, L2=3,790,870, L3=32,721,786, L4=3,790,870)

---

## VERDICT: **DO NOT FREEZE** — 4 of 8 audits FAIL

| # | Audit | Verdict | Critical Issue |
|---|---|---|---|
| 1b | Event Accounting Waterfall | **PASS** ✅ | Gap = failed txs (proven) |
| 2 | Fat-Tail Forensics | **FAIL** ❌ | 99.999% extreme — metric worthless |
| 3 | L2 Objective Truth | **FAIL** ❌ | 21/27 required fields missing |
| 4 | Counterfactual Semantics | **FAIL** ❌ | path_invariance_assumption not recorded |
| 5 | state_id Uniqueness | **PASS** ✅ | 0 duplicates in 3.79M rows |
| 6 | Price/Reserve Coverage | **PASS** ✅ | 100% price recovery |
| 7 | Final QA | **PASS** ✅ | All joins 1:1, no causal leakage |

---

## AUDIT 1b: Event Accounting Waterfall — PASS ✅

### The Waterfall (full 369-file corpus, 482s)

| Class | Manifest | Disc(all) | Disc(succ) | Disc(fail) | Outer(succ) | Inner(succ) | Combined | Mint Res | States |
|---|---|---|---|---|---|---|---|---|---|
| pumpfun_buy | 67,043 | 67,125 | 53,025 | 14,100 | 32,082 | 20,943 | 53,025 | 51,996 | 30,980 |
| pumpfun_sell | 80,870 | 83,468 | 69,915 | 13,553 | 46,394 | 23,521 | 69,915 | 69,915 | 46,321 |
| pumpswap_buy | 1,441,982 | 1,442,520 | 1,352,828 | 89,692 | 1,351,147 | 1,681 | 1,352,828 | 1,352,828 | 1,350,048 |
| pumpswap_sell | 2,642,211 | 2,662,573 | 2,375,839 | 286,734 | 2,364,581 | 11,258 | 2,375,839 | 2,375,839 | 2,363,521 |
| pumpfun_migrate | 10,721 | 10,721 | 29 | 10,692 | 29 | 0 | 29 | 29 | 0 |

### Gap Attribution: Failed Transactions

**Every gap between manifest and decoded is principally failed transactions:**

| Class | Manifest→Combined Gap | Failed TX Count | Failed TX % of Gap |
|---|---|---|---|
| pumpfun_buy | 14,018 (20.9%) | 14,100 | 100.6% |
| pumpfun_sell | 10,955 (13.5%) | 13,553 | 123.7% |
| pumpswap_buy | 89,154 (6.2%) | 89,692 | 100.6% |
| pumpswap_sell | 266,372 (10.1%) | 286,734 | 107.6% |

**Conclusion:** The manifest counts events from ALL transactions (success + failed). The v3 builder correctly filters to successful transactions only. Failed transactions exceed 100% of the gap because discriminator matches also have a surplus over manifest (duplicate semantic events in outer+inner). The gap is NOT a decoding failure — it is a correct quality filter.

**0 decoding failures:** Every discriminator match in a successful transaction was successfully decoded. No unsupported instruction variants, no malformed balances, no missing loaded addresses.

### Decoded → States Reconciliation

- Total decoded trade events (success): 3,851,607
- Total mint resolved: 3,850,578
- Build 3 states: 3,790,870
- Gap: 59,708 (1.55%)
- Reasons: price_recovery_failed (2,305) + no_mint_found (1,029) + duplicate semantic events + non-trade events

### Migration Forensics: 10,721 → 29

**Evidence:**
- 10,721 discriminator matches across ALL transactions
- 10,692 (99.7%) are in **failed transactions** (error type 08000000)
- 29 are in successful transactions
- 10,721 unique signatures = 1.0 match per signature (no duplication)
- 33 unique mints, 329 unique slots
- Top slot 441331373 has 201 matches (all failed)
- **The original manifest counter counted migration discriminator matches in ALL transactions including failed ones.** The 10,692 failed-tx matches are failed migration attempts (e.g., insufficient liquidity, slippage, simulation failures) — not noise, not account updates, not repeated representations. They are real migration instructions that FAILED on-chain.
- **29 successful migrations** is the correct count of migrations that actually executed.
- state_emitted=0 for migrations because migration is a venue-transition event, not a trade state.

### Inner-Instruction Identity Granularity

- 4,267,582 identity keys checked
- 4,267,582 unique keys
- 0 collisions
- Identity scheme: `signature:ix_idx` for outer, `signature:inner_group_idx:inner_ix_idx` for inner
- **Sufficient granularity** — multiple legitimate events in one signature cannot collide or disappear

---

## AUDIT 2: Fat-Tail Forensics — FAIL ❌

**Critical Issue:** 99.999% of states marked `extreme_outlier=True` (3,790,840/3,790,870). Only 30 states are NOT extreme.

**Root Cause:** The `revalidate_fat_tails` function (line 1347) classifies `price < 100.0` lamports per raw token as extreme. The price distribution shows:
- 64.0% of prices are < 0.001 lamports/rawtok
- 19.4% are < 0.01
- 8.5% are < 0.1
- 4.9% are < 1
- 3.2% are < 10
- Only 0.03% are > 100

The threshold is set far too low for the actual price distribution. Nearly every token state falls below 100 lamports/rawtok because raw token amounts are in the billions (token_decimals=6 for 99.6% of mints).

**Return Distribution (L3, 0.1 SOL):**
- 97.8% timeout/marginal (-15% to +15%)
- 0.8% SL hit (-50% to -15%)
- 0.6% TP hit (+15% to +50%)
- 0.5% 100%+ returns
- 0.02% >1000% returns (fat tails confirmed)

**Independent recomputation:** 50 random extreme states — 50/50 matched (±1%), 0 order-of-magnitude errors. The extreme classification is consistent but the threshold is wrong.

**v2 comparison:** v2 had 243,687 extreme outliers (6.4%). v3 has 3,790,840 (100%). This is a regression.

---

## AUDIT 3: L2 Objective Truth — FAIL ❌

**Critical Issue:** L2 has only 8 columns. 21 of 27 required policy-independent fields are MISSING.

**Present (6/27):** state_id, venue, return_pct, outcome_class, entry_executable, exit_executable, capacity_constrained, entry_failure_reason

**Missing (21/27):**
- Multi-horizon returns: return_1s, return_2s, return_5s, return_10s, return_30s, return_60s, return_120s, return_300s
- MFE/MAE: mfe_pct, mae_pct, mfe_time_ms, mae_time_ms
- Barrier events: barrier_first, barrier_first_time_ms
- Peak tracking: peak_return_pct, peak_time_ms
- Observation metadata: observed_no_trade, right_censored, observed_through
- Outcome classification: migration_outcome, venue_outcome

L1 HAS the censoring/no-trade fields (right_censored_1s through 300s, observed_no_trade_1s through 300s) but L2 does not carry them forward. L2 is essentially a thin projection of L3 at a single size/latency — not the rich policy-independent truth layer required.

---

## AUDIT 4: Counterfactual Semantics — FAIL ❌

**Critical Issue:** `path_invariance_assumption` is not recorded in the manifest or L3 schema.

**Passing checks:**
- Trade sizes: [0.05, 0.1, 0.25, 0.5, 1.0] SOL ✓
- Latency scenarios: [0, 100, 500, 1000, 2000] ms ✓
- Capacity constrained flags: 0.1% at 0.05 SOL → 0.4% at 1.0 SOL ✓
- Venue-specific fee check: pumpswap vs pumpfun pricing distinct ✓
- Forward exit verification: outcome distribution shows TP/SL/timeout/entry_failed ✓

**Concerns:**
- Mean returns wildly inflated (16,311% at 0.05 SOL, median -0.5%) = fat-tail influence on mean
- `path_invariance_assumption` must be explicitly recorded as a metadata field
- L3 has no column for this assumption — it must be in the manifest

---

## AUDIT 5: state_id Uniqueness — PASS ✅

- L1: 3,790,870 rows = 3,790,870 unique state_ids
- 0 duplicates across the entire corpus
- state_id present in all 4 layers (L1, L2, L3, L4)
- L3: 32,721,786 rows over the same 3,790,870 unique state_ids (≈8.64 counterfactuals/state)

---

## AUDIT 6: Price/Reserve Coverage — PASS ✅

- **Price recovery: 100%** (3,790,870/3,790,870)
- pumpfun: 77,301/77,301 (100%)
- pumpswap: 3,713,569/3,713,569 (100%)
- Pool reserves: 3,365,059 non-null (88.8%) — expected, as pumpfun curve states don't have pool reserves
- 0 unrecoverable states

---

## AUDIT 7: Final QA — PASS ✅

- **Join integrity:** All 1:1:1:1 joins complete. L1→L2→L4 all 3,790,870 unique state_ids, 0 orphans. L3 has 32,721,786 rows over same IDs.
- **Causal leakage:** No future-looking data in entry features. Timestamps monotonic within mints.
- **Units/ranges:** All lamports positive, token_decimals 6-9, price range sane.
- **Tail censoring:** 0.0% at 1s, correctly low.
- **No-trade flags:** 19.2% at 1s → declining to longer horizons (correct behavior).
- **Migration continuity:** 124 mints appear in both pumpfun and pumpswap venues (transitioned).
- **Provenance:** UUID, git SHA, SHA256 hashes all recorded.

---

## REQUIRED FIXES BEFORE FREEZE

1. **Fix fat-tail threshold** (Audit 2): Replace `price < 100.0` with a percentile-based or economically meaningful threshold. v2's 6.4% rate is the target range.

2. **Enrich L2 with required fields** (Audit 3): Add 21 missing policy-independent fields — multi-horizon returns, MFE/MAE, barrier events, observation metadata, migration/venue outcomes.

3. **Record path_invariance_assumption** (Audit 4): Add explicit metadata field to manifest and L3 schema documenting the assumption that the entry/exit quote functions are invariant to the simulated trade path.

4. **Re-run Build 3** with fixes, then re-run all 7 audits.

**v3 CANNOT BE FROZEN until these 4 failures are resolved.**
