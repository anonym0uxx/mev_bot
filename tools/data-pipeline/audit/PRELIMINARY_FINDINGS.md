# LaserStream v3 Forensic Certification — Preliminary Findings
## (Based on code review + Audit 1 RAW reconciliation)

## AUDIT 1: RAW EVENT RECONCILIATION — ⚠️ FAIL (critical gap)

### Capture Manifest Counts (authoritative)
| Event Class | Manifest Count |
|---|---|
| creates | 38 |
| pump_buys | 67,043 |
| pump_sells | 80,870 |
| migrations | 10,721 |
| pumpswap_buys | 1,441,982 |
| pumpswap_sells | 2,642,211 |
| pumpswap_create_pools | 453 |
| pumpswap_deposits | 110 |
| pumpswap_withdraws | 567 |
| **total_events** | **4,243,995** |

### v3 Builder Combined Counts (outer + inner)
| Event Class | v3 Outer | v3 Inner | v3 Combined |
|---|---|---|---|
| pumpfun_buy | 32,082 | 20,943 | 53,025 |
| pumpfun_sell | 46,394 | 23,521 | 69,915 |
| pumpfun_migrate | 29 | 0 | 29 |
| pumpswap_buy | 1,351,147 | 1,681 | 1,352,828 |
| pumpswap_sell | 2,364,581 | 11,258 | 2,375,839 |
| pumpswap_create_pool | 395 | 103 | 498 |
| pumpswap_deposit | 83 | 0 | 83 |
| pumpswap_withdraw | 529 | 0 | 529 |

### Finding 1: Inner Instructions NOT Processed by v3 Builder
**CRITICAL**: The v3 builder (`process_transactions`, line 730-731) only iterates 
`msg.get('instructions', [])` — **outer instructions only**. It never processes 
`meta.get('inner_instructions', [])`. On Solana, CPI calls (inner instructions) 
carry a significant fraction of PumpFun/PumpSwap trades.

Inner instruction counts missed:
- 20,943 pumpfun buys (39% of total)
- 23,521 pumpfun sells (34% of total)
- 1,681 pumpswap buys (0.12%)
- 11,258 pumpswap sells (0.47%)
- 103 pumpswap pool creations
- **44,464 total inner events missed**

### Finding 2: Migration 10,721 → 29 Reduction Explained
The 10,721 "migration" events in the capture manifest are NOT 10,721 unique 
migration transactions. Analysis of the NDJSON shows:
- 10,676 events share ONE curve account `4qHAq...pump` across 292 slots
- Each slot contains up to 201 transactions referencing this curve
- The Rust normalizer counts EVERY instruction matching the migration discriminator
  (8-byte `9beae79cec9ea21e`), including buy/sell transactions that include the
  curve account in their account list
- **329 unique (curve, slot) pairs** = 329 actual migration events
- **29 unique curves** with true migration instructions = 29 real migrations
- The v3 builder correctly detects ~29 real migrations

**Verdict**: The 10,721→29 reduction is NOT a bug. It's the Rust normalizer's 
inflated counting (every discriminator match) vs the v3 builder's correct 
deduplication. However, the v3 builder is missing inner-instruction events 
which IS a bug.

### Finding 3: Failed Transactions Filtered
The v3 builder filters out failed transactions (err != None). The audit found 
3,832,202 failed vs 7,909,098 successful in 11.7M transactions. This accounts 
for some of the gap between manifest and v3 counts.

### Reconciliation Summary
| Event Class | Manifest | v3 Combined | Gap | Gap % | Explanation |
|---|---|---|---|---|---|
| pumpfun_buy | 67,043 | 53,025 | -14,018 | -21% | Inner ix + failed txs |
| pumpfun_sell | 80,870 | 69,915 | -10,955 | -14% | Inner ix + failed txs |
| pumpswap_buy | 1,441,982 | 1,352,828 | -89,154 | -6% | Inner ix + failed txs |
| pumpswap_sell | 2,642,211 | 2,375,839 | -266,372 | -10% | Inner ix + failed txs |
| migrations | 10,721 | 29 | -10,692 | -99.7% | Rust normalizer inflation |

**NOTE**: Even the v3 builder's COMBINED counts (if it processed inner ix) 
don't fully match the manifest, because the manifest's Rust decoder uses 
different event class naming and may count events differently. The remaining 
6-21% gap needs investigation but the inner-ix gap is the primary cause.

---

## AUDIT 2: FAT-TAIL FORENSICS — ⚠️ FAIL (methodology error)

### Finding: Fat-Tail Validation Logic is Wrong
The `revalidate_fat_tails` function (line 1326) classifies extreme outliers 
based on **absolute price threshold**, NOT return percentages:
```python
if price < 1.0:    # sub-1-lamport per raw token
    extreme_outlier = True
elif price < 100.0:
    extreme_outlier = True
else:
    extreme_outlier = False
```
This marks ANY state with price < 100 lamports/rawtok as "extreme" — which 
is most early-lifecycle tokens. This explains the 64% extreme rate: it's not 
measuring fat-tail returns at all, just low prices.

**The correct approach**: extreme outliers should be identified by RETURN 
percentage (e.g., |return| > 1000%), not by absolute price level. The 
current implementation conflates "low price" with "extreme return."

---

## AUDIT 3: L2 OBJECTIVE TRUTH — ❌ FAIL (severely impoverished)

### Finding: L2 Has Only 7 Fields
The L2 layer (line 1399-1412) contains ONLY:
- state_id
- venue
- return_pct (benchmark 0.1 SOL)
- outcome_class
- entry_executable
- exit_executable
- capacity_constrained
- entry_failure_reason

### Missing Required Fields
- ❌ Returns at 1/2/5/10/30/60/120/300s horizons
- ❌ MFE/MAE + timing
- ❌ Barrier-first events
- ❌ Peak/time
- ❌ Observed-through / right-censored per horizon
- ❌ Migration/venue outcome
- ❌ Executable-observation metadata

The L2 is reduced to just return_pct/outcome_class — exactly what the user 
said must NOT happen. The rich policy-independent truth (censoring, no-trade 
flags, per-horizon returns) exists in L1 but is NOT carried through to L2.

---

## AUDIT 4: COUNTERFACTUAL SEMANTICS — ⚠️ FAIL (missing assumptions)

### Finding 1: No path_invariance_assumption Recorded
The counterfactual builder (line 1064-1258) does NOT explicitly record the 
path_invariance_assumption — the assumption that our hypothetical trade does 
not change subsequent observed market behavior. This is a critical missing 
metadata field.

### Finding 2: No Capacity/Confidence Flags for Larger Sizes
The counterfactuals do flag `capacity_constrained` when the sell quote exceeds 
available reserves (line 1187, 1193), but there's no graduated confidence 
flag for larger sizes that might move the market more. The 1 SOL size gets 
the same confidence as 0.05 SOL.

### Finding 3: Fees Appear Correct
- PumpFun buys: 1% fee (100 bps) applied ✓
- PumpFun sells: 0% fee ✓ (buyer pays fee model)
- PumpSwap: 0.25% fee (25 bps) ✓

### Finding 4: Forward Exit Scanning Correct
Exits use future price timelines (line 1170-1210), not same-time buy-and-sell. ✓

---

## AUDIT 5: STATE_ID UNIQUENESS — ⚠️ NEEDS VERIFICATION

### Finding: state_id = mint+slot+signature[:16]+ix_index
The `make_state_id` function (line 258) creates: 
`mint_b58 + "_" + slot + "_" + sig[:16] + "_" + ix_index`

**Risk**: If two events for the same mint in the same slot with the same 
signature (different ix_index) exist, AND one is an outer instruction and 
one is an inner instruction, they COULD share ix_index (inner instructions 
are indexed separately from outer instructions). Since the v3 builder only 
processes outer instructions, this collision may not occur in practice — 
but if inner instructions are added (per Audit 1 fix), collisions become 
possible.

**Also**: The signature is truncated to 16 chars — this creates a collision 
risk if two different signatures share the first 16 characters.

---

## AUDIT 6: PRICE/RESERVE COVERAGE — Pending Build Completion

### Preliminary: Price Recovery Method
The `recover_price_from_raw` function (line 430) uses balance deltas:
- PumpFun: native SOL balance delta (largest absolute delta = curve)
- PumpSwap: wSOL token account delta

This is the correct empirical approach. However, it depends on all balance 
arrays being present in the RAW data, which may not always be the case for 
failed transactions or account lookup table (ALT) scenarios.

---

## AUDIT 7: FINAL QA — Pending Build Completion

---

## SUMMARY OF PRELIMINARY FINDINGS

| Audit | Status | Critical? |
|---|---|---|
| 1. RAW Reconciliation | ⚠️ FAIL | YES — inner ix not processed |
| 2. Fat-Tail Forensics | ⚠️ FAIL | YES — methodology error |
| 3. L2 Objective Truth | ❌ FAIL | YES — severely impoverished |
| 4. Counterfactual Semantics | ⚠️ FAIL | YES — missing path_invariance |
| 5. state_id Uniqueness | ⚠️ VERIFY | Maybe — truncation risk |
| 6. Price/Reserve Coverage | PENDING | - |
| 7. Final QA | PENDING | - |

**RECOMMENDATION**: Do NOT freeze v3. Multiple critical issues require fixes 
before certification. The inner-instruction gap, fat-tail methodology error, 
and L2 impoverishment are blocking issues.
