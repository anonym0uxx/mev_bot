# Entry Choice Audit — Principal Quant Analysis

**Date**: 2026-08-19
**Author**: Hermes (Principal Rust Engineer + Principal Citadel Pump Fun Memecoin Quant)
**Scope**: Deep analysis of the entry decision pipeline from raw event → admission → scoring → sizing → buy. Identify whether entry CHOICE (not just entry EXECUTION) is the root cause of slow bleed.

---

## 1. The Entry Decision Pipeline (Top to Bottom)

```
LaserStream / account-subscribe / pump-portal
        ↓
    on_event(AppEvent)                    [engine.rs:1695]
        ↓
    watchlist.ingest_union()              [adds candidate to bounded watchlist]
        ↓
    evaluate()                            [engine.rs:2552, runs per tick]
        ↓
    promote_top(k, min_rank)              [promote.rs:23 — strongest-first by rank]
        ↓
    for each promoted candidate:
        ├── reentry_cooldown check        [reject code 25]
        ├── priced_move computation       [expected move: model → lane evidence → cold-start]
        ├── gate::decide()                [gate.rs:148 — THE GATE]
        │   ├── NeedsOnchainConfirmation  [code 1: no OnchainConfirm event]
        │   ├── NoNumericConfirmation     [code 2: no numeric snapshot]
        │   ├── OutsideMcapBand           [code 9: mcap outside configured band]
        │   ├── EntryQualityFilter        [code 26: EQF — 7 sub-checks]
        │   ├── Tp1Unreachable            [code 18: model says TP1 unreachable]
        │   ├── EconomicallyUnviable      [code 3: cost band refuses]
        │   └── WangrTokenStandard...     [codes 30-34: wangr filters]
        ↓ (if Admitted)
    post-gate veto chain (engine.rs:3210+)
        ├── FabricatedFlow                [code 9: auth_bps too low]
        ├── CreatorDump                   [code 13: confirmed creator distribution]
        ├── FeeFloor                      [code 14: saturated first-slot footprint]
        ├── HolderConcentration           [code 17: concentrated holdings]
        ├── BundleDetected                [code 20: same-slot buy count ≥ threshold]
        ├── BundleConcentration           [code 21: same-slot buyer concentration]
        ├── DevHistory                    [code 22: low graduation rate deployer]
        ├── CoordinatedFunding            [code 23: shared funding source]
        ├── InsufficientExitLiquidity     [code 24: too few holders]
        ├── VPIN_Toxic                    [code 4: sell-dominant dump]
        └── MaxConcurrent                 [code 6: positions ≥ cap]
        ↓ (if all vetoes clear)
    SIZING (engine.rs:3430+)
        ├── bankroll chain (deployable, risk_budget, available_risk)
        ├── f_eff = drawdown-ratcheted fraction
        ├── regime_mult (trend/revert)
        ├── auth_mult (flow authenticity)
        ├── conc_mult (concentration fragility)
        ├── cred_mult (creator credibility)
        ├── deployer_mult (deployer screen)
        ├── fee_fade (fee floor graded)
        └── final size = balance × f_eff × all_mults / 10_000^n
        ↓ (if size ≥ min_trade_size)
    ADMIT — position opened, entry recorded
```

## 2. Rejection Histogram Analysis (Live Daemon Data)

From `live_status.json` (73,421 total rejections, 11 admitted):

| Code | Reason | Count | % of Rejects |
|------|--------|-------|--------------|
| 1 | NeedsOnchainConfirmation | 34,802 | 47.4% |
| 9 | OutsideMcapBand | 33,097 | 45.1% |
| 26 | EntryQualityFilter (EQF) | 1,775 | 2.4% |
| 6 | MaxConcurrent | 1,503 | 2.0% |
| 19 | OpenFailure | 1,490 | 2.0% |
| 3 | EconomicallyUnviable | 372 | 0.5% |
| 7 | BelowCostFloor | 307 | 0.4% |
| 25 | ReentryCooldown | 75 | 0.1% |

### Key Finding #1: 92.5% of rejections are PRE-ECONOMIC

**47.4% (code 1)** = candidates that reached the gate but had NO on-chain confirmation. This means the watchlist promoted them, but no `OnchainConfirm` event arrived before the gate evaluated them. This is a **TIMING** issue — the on-chain confirmation feed is slower than the watchlist promotion cycle.

**45.1% (code 9)** = candidates with on-chain confirmation but whose market cap falls outside the configured `[mcap_band_lo, mcap_band_hi]` range. This is a **SELECTION** issue — the band is filtering out the majority of candidates.

### Key Finding #2: Only 0.5% reach the economic gate

Only 372 candidates (0.5%) were rejected as EconomicallyUnviable — meaning the cost band refused them. This means the gate's economic model is NOT the bottleneck. The bottleneck is UPSTREAM: on-chain confirmation timing and mcap band filtering.

### Key Finding #3: EQF rejects 2.4% of candidates

The Entry Quality Filter (code 26) rejects 1,775 candidates across 7 sub-checks:
1. `trades_observed < entry_min_trades_observed` — insufficient evidence
2. `buy_ratio_bp < entry_min_buy_ratio_bp` — no organic demand
3. `max_trade_lamports > entry_max_sol_per_trade_lamports` — whale dominance
4. `age_slots < entry_min_age_slots` — too young (crash-reversion risk)
5. `volume_lamports < entry_min_volume_lamports` — insufficient exit liquidity
6. `buy_pressure_bp < entry_min_buy_pressure_bp` — dead-on-arrival
7. `unique_buyers < entry_min_unique_buyers` — buyer concentration

## 3. Ranking / Promotion Logic Analysis

The `score_rank` function (rank.rs:118) computes:

```
rank = discovery_score × recency_factor × lane_weight
```

Where:
- `discovery_score` = the candidate's raw score from the discovery lane
- `recency_factor` = linear decay from 1.0 → 0 over `watchlist_ttl_ticks`
- `lane_weight` = per-lane weight in basis points (adjustable from realized performance)

**Observation**: The ranking is PURELY recency-weighted discovery score. There is NO quality signal in the ranking itself — no buy pressure, no holder concentration, no authenticity. Quality enters ONLY at the gate (post-promotion). This means the watchlist may be promoting high-velocity garbage that the gate then rejects, wasting evaluation cycles.

### Promote logic (promote.rs:23):
```rust
state.ranked(now)
    .into_iter()
    .filter(|(rank, _)| *rank >= min_rank)
    .take(k)
    .map(|(_, cand)| cand)
    .collect()
```

The `take(k)` means only the top-k candidates by rank are evaluated per tick. If k is small and the watchlist is flooded with new candidates (high recency), older but higher-quality candidates may be starved of evaluation slots.

## 4. The PricedMove / Expected Move Pipeline

The engine computes ONE expected-move estimate per candidate:

```
model_estimate (if expected_move_model_enable):
    → expected_move.estimate(vsol, SignalObs, MoveParams)
    → MoveVerdict::Known(e) | MoveVerdict::Unknown(_)

priced_move = self.priced_move(cand.lane, model_estimate.as_ref())
```

Precedence:
1. **Calibrated model** (if armed AND above sample floor)
2. **Lane's realized evidence** (if available)
3. **Cold-start constant** (`gate_expected_move_bps = 3_400`)

**Critical observation**: The cold-start prior of 3,400 bps (34%) is a POPULATION estimate. Every cold-start candidate is assumed to have a 34% expected move. This means the gate evaluates all cold-start candidates identically from an expected-move perspective — there is NO per-candidate differentiation at cold-start. The differentiation comes ENTIRELY from the cost model (impact, fees, fixed costs) and the post-gate veto chain.

## 5. The Gate's Economic Model

The `size_band` function computes:
- `x_min` = minimum economic size (below this, round-trip costs exceed expected move)
- `x_max` = maximum size the pool can absorb (payout reserve)
- `x_cost` = cost-minimizing size

The gate refuses if:
- `x_max == 0` (no capacity) → EconomicallyUnviable
- `x_min > x_max` (cost floor exceeds capacity) → EconomicallyUnviable
- TP1 reachability: if the model's estimated upside can't reach TP1 (+10%) after round-trip costs → Tp1Unreachable

**This is sound.** The economic gate is well-designed. The issue is NOT the gate's economics.

## 6. ROOT CAUSE ANALYSIS — Is Entry CHOICE the Problem?

### The user's suspicion: entry choices may be the root cause of slow bleed.

**VERDICT: Partially confirmed, but the bleed mechanism is more nuanced.**

The entry CHOICE has three structural weaknesses:

### Weakness A: No quality-weighted promotion (RANKING blind spot)

The `score_rank` function uses ONLY `discovery_score × recency × lane_weight`. It does NOT consider:
- Buy pressure (buy_pressure_bp)
- Buyer diversity (unique_buyers)
- Authenticity (fabrication probability)
- Holder concentration
- Creator credibility

These quality signals are evaluated ONLY at the gate, AFTER promotion. This means:
- A high-velocity wash-traded coin with high discovery_score gets promoted
- The gate then rejects it (code 9 = FABRICATED_FLOW or code 26 = EQF)
- But the evaluation slot was wasted — a slower but higher-quality coin was NOT promoted

**Impact**: The bot is spending evaluation cycles on garbage and missing quality. This doesn't directly cause bleed (rejected candidates don't trade), but it INDIRECTLY causes bleed by:
1. Keeping `max_concurrent_positions` filled with lower-quality positions
2. Reducing the number of quality candidates that reach the gate per tick

### Weakness B: Cold-start prior is UNIFORM across all candidates

The 3,400 bps cold-start prior means every uncalibrated candidate is assumed to have a 34% expected move. The model only differentiates AFTER it has enough paper trades to calibrate. This means:
- At cold-start, the gate admits ANY candidate whose costs are below 34% round-trip
- There is no pre-filter for "this coin is more likely to moonshot than that one" at cold-start
- The ONLY differentiation is the cost model (depth, fees, impact)

**Impact**: The bot enters positions based on COST economics, not RETURN economics, during cold-start. This is fine for survival (cost-aware sizing prevents ruin) but suboptimal for profit maximization (it doesn't preferentially enter higher-EV candidates).

### Weakness C: The mcap band filters 45% of candidates

33,097 candidates (45.1%) rejected as OutsideMcapBand. This is a HARD filter — any candidate outside `[mcap_band_lo, mcap_band_hi]` is refused before the economic gate. If the band is too narrow, the bot is missing potentially profitable candidates. If too wide, it's admitting garbage.

**Impact**: Need to audit the mcap band configuration against the actual distribution of graduated tokens. The wangr study analyzed 567,876 tokens (2,770 graduations) — the band should reflect the mcap distribution at GRADUATION time, not at LAUNCH time.

## 7. RECOMMENDED FIXES (Priority-Ordered)

### Fix 1: Add quality-weighted ranking (HIGH PRIORITY)

Inject buy_pressure_bp and unique_buyers into `score_rank` as quality multipliers. This ensures the promotion step preferentially evaluates higher-quality candidates, not just faster ones.

```
rank = discovery_score × recency × lane_weight × quality_factor
where quality_factor = f(buy_pressure_bp, unique_buyers, authenticity)
```

### Fix 2: Calibrate the mcap band against graduation data (HIGH PRIORITY)

Audit `mcap_band_lo_lamports` and `mcap_band_hi_lamports` against the wangr graduation study. 45% rejection rate suggests the band may be too restrictive.

### Fix 3: Add per-candidate cold-start differentiation (MEDIUM PRIORITY)

Instead of a uniform 3,400 bps prior, use the Features snapshot (buy_pressure, unique_buyers, age, volume) to compute a PER-CANDIDATE cold-start prior. This lets the gate preferentially enter higher-EV candidates even before the model calibrates.

### Fix 4: Increase promotion k or add quality-quota (MEDIUM PRIORITY)

If k (promote count per tick) is small relative to watchlist size, increase it. Or add a quality quota: reserve some promotion slots for candidates with high quality signals but lower recency.

---

## 8. CONCLUSION

The entry CHOICE is NOT the direct cause of slow bleed (rejected candidates don't trade). The direct causes were:
1. **No on-chain feedback loop** (FIXED Rev-19) — paper PnL recorded as live
2. **Real buys without real exits** (FIXED Rev-19) — sell path wired
3. **Failed tx fees** (FIXED Rev-20 item 3) — skipPreflight=false for sells

HOWEVER, the entry CHOICE has structural weaknesses that INDIRECTLY reduce profitability:
- Quality-blind ranking wastes evaluation slots on garbage
- Uniform cold-start prior prevents preferential entry of high-EV candidates
- The mcap band may be too restrictive (45% rejection)

These should be addressed to maximize PnL, but they are OPTIMIZATION issues, not bleed issues. The bleed is fixed. The profit maximization is the next frontier.
