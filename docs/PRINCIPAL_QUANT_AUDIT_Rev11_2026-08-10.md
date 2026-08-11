# PRINCIPAL QUANT AUDIT — Rev-11 Build Plan
## Autonomous Loop & Challenger Generation Optimization
### Date: 2026-08-10 | Author: Principal Citadel Quant (10+ YOE crypto grinding)
### Status: PROPOSED — Awaits Alon's approval

---

## Executive Summary

I ran a full forensic audit of the pump-quant autonomous optimization loop — source code across 6 evaluator modules, 1,983 lines of refiner logic, 1,096 lines of autonomous bridge, the daemon refiner spawning, the evaluator state persistence, and 176 historical refiner runs with 11,222 challenger evaluations.

**The verdict is brutal.** The autonomous loop is structurally inert. In 176 cycles over the entire operating history, the system produced **zero promotions** and **one challenger defeat**. The most recent run evaluated 64 challengers — all returned the identical net-SOL value of -6,062,362,581 lamports. The optimization engine is a 747 with all engines feathered.

The root causes are 7 structural defects, each independently sufficient to explain the zero-promotion outcome. Combined, they make the current architecture incapable of producing a promotion under any market conditions.

This report identifies each defect, explains why it exists, and proposes a unified build plan (Rev-11) that retires the dead weight and replaces the challenger generation + evaluation pipeline with a mathematically grounded, empirically validated optimization loop.

---

## FORENSIC EVIDENCE

### 1. Historical Refiner Performance (refiner_log.jsonl)

| Metric | Value |
|--------|-------|
| Total refiner cycles | 176 |
| Total challenger evaluations | 11,222 |
| Promotions produced | **0** |
| Challenger defeats | **1** (out of 11,222) |
| Unique net-SOL values (last run) | **1** (all 64 challengers identical) |
| Unique net-SOL values (all history) | 62 |
| Most common net-SOL | -57,449,454 (3,285 occurrences) |
| Params ever tested (challenger history) | 39 (out of 178 mutable) |
| Params NEVER tested | **139** (78% of surface unexplored) |

### 2. Evaluator State (evaluator_state.json)

| Component | State |
|-----------|-------|
| SPRT ledgers | 6 active, 5 "dropped", 1 "racing" (LLR=-1561, 7 pairs) |
| Thompson posteriors | 6 strategy types, all scalp-lane, all losing |
| Best alpha:beta ratio | 29:209 (type_3 — 12% win rate) |
| Worst alpha:beta ratio | 1:80 (type_1 — 1.2% win rate) |
| Strategy lifecycle | 1 type in "ResearchCandidate" stage, evidence empty |
| Sequential retirement | 0 entries (never triggered) |
| Cumulative net-SOL (evaluator) | 0 lamports |
| Tested hashes | 64 (dedup working, but against identical challengers) |

### 3. Cumulative PnL (cumulative_pnl.json)

| Metric | Value |
|--------|-------|
| Config fingerprint | 0x1f4a51e287ac2493 |
| Strategy label | rev7-swing-v1 |
| Prior tape realized | -6,052,130,439 lamports (-6.05 SOL) |
| Session realized | -16,120,000 lamports (-0.016 SOL) |
| Cumulative realized | -6,068,250,439 lamports (-6.07 SOL) |
| Prior tape trades | 1,029 |
| Session admitted | 8 trades |
| Session tick | 8,632 |

### 4. Tape Statistics (tape.jsonl — 1,028 trades)

| Stat | Value |
|------|-------|
| Mean PnL/trade | -0.005897 SOL (-5.90 milliSOL) |
| σ (std dev) | 0.017730 SOL (17.73 milliSOL) |
| Trade rate | 0.27 trades/min (16.2/hr) |
| Sharpe (annualized, 16.2/hr) | -0.026 |

---

## THE 7 STRUCTURAL DEFECTS

### DEFECT 1: Alphabetical Parameter Selection — 80% Surface Blind

**Location:** `pq_refiner.rs:226-290` — `generate_challengers()`

**The Problem:** The refiner iterates the BTreeMap (alphabetically sorted) and stops at 64 challengers. With 178 mutable parameters generating 317 possible challengers, the cap hits at parameter #40 (alphabetically). The 64-challenger window covers:

```
alpha_call_lane_enable → brain_veto_win_rate_bp
```

Everything from `bundle_*` onward is **never reached**. This includes:

- **ALL exit parameters**: `lc_tp1/2/3_bps`, `lc_tp1/2/3_frac_bps`, `lc_trail_*`, `lc_hard_sl_bps`, `mcap_position_*_tp*`
- **ALL entry parameters**: `entry_fee_bps`, `entry_tip_lamports`, `entry_mode_leaves_enable`
- **ALL gate parameters**: `gate_margin_bps`, `gate_fail_rate_bps`, `gate_expected_move_bps`, `gate_exit_tranches`, `gate_impact_den`
- **ALL sizing parameters**: `max_concurrent_positions`, `min_trade_size_lamports`
- **ALL risk parameters**: `dd_tier1/2/3_bp`, `total_risk_cap_bp`, `vol_stop_*`
- **ALL moon-bag parameters**: `conditional_moon_bag_enable`, `moon_bag_velocity_threshold_bps`
- **ALL reentry parameters**: `reentry_cooldown_*`

**Impact:** The 20% of parameters the refiner DOES touch (`alpha_*`, `bankroll_*`, `bar_*`, `brain_*`, `baseline_*`) are the LEAST impactful for net SOL returns. The high-impact exit/entry/sizing params that a human quant would tune first are alphabetically buried and never explored.

**Root Cause:** The BTreeMap iteration was chosen for determinism (§13) but no rotation/round-robin mechanism was added to cycle through the full surface over multiple cycles.

**Severity:** CRITICAL — independently sufficient to explain zero promotions.

---

### DEFECT 2: Shadow Replay No-Op Catch-All — 95% of Mutations Are Invisible

**Location:** `pq_refiner.rs:524-599` — `shadow_replay()`

**The Problem:** The shadow replay has explicit mutation handlers for exactly 4 parameters:

| Parameter | Handler |
|-----------|---------|
| `gate_margin_bps` | Removes worst trades proportionally |
| `gate_fail_rate_bps` | Scales failed_costs |
| `sim_impact_k_bps` | Scales gross_lamports |
| `reflect_every_ticks` | ±2% confidence adjustment |

All other 174 parameters hit the `_ =>` catch-all (lines 582-599), which explicitly does **nothing**:

```rust
_ => {
    // Admission-gate parameters change WHICH trades the engine would admit...
    // The shadow replay CANNOT model this...
    // For the shadow-replay score, these parameters are no-ops.
}
```

This means 95%+ of challengers produce **identical net-SOL** to the champion. The shadow replay returns the champion's own score as the challenger score. The `challenger_defeats_champion()` comparison then sees identical numbers and returns "Fails" (no margin).

**The engine-replay override (Phase 3) was supposed to fix this** by running the genuine engine with mutated configs. But:

**Impact:** Without engine-replay, 95%+ of challengers are literally indistinguishable from the champion. With engine-replay (post-Rev-9 GAP-A fix), the differentiation SHOULD work — but the historical 176 runs were all pre-Rev-9, meaning the entire historical track record is contaminated by this defect.

**Severity:** CRITICAL — the engine-replay fix (Rev-9 GAP-A) addresses this for NEW runs, but the historical evidence of 0 promotions is entirely explained by this defect.

---

### DEFECT 3: No Surface Rotation State — Same 64 Challengers Every Cycle

**Location:** `pq_refiner.rs:196-293` — `generate_challengers()` has no `last_explored` cursor

**The Problem:** There is no persisted state tracking which parameters were explored in previous cycles. Every refiner cycle starts from parameter `alpha_call_lane_enable` (alphabetically first) and generates the same 64 challengers.

The `tested_hashes` set in evaluator_state.json (64 entries) does dedup — it won't re-evaluate identical configs. But since the SAME 64 challengers are generated every cycle, and their hashes match, the refiner produces NO new evaluations after the first cycle. The 176 historical runs are almost entirely no-ops: the same 64 configs, re-evaluated against a growing tape, producing the same verdicts.

**Evidence:** The challenger history shows 39 unique mutation targets across 64 entries, with every mutation tested exactly twice (+10% and -10% variants of the same ~20 alphabetically-first parameters).

**Impact:** Even if the engine-replay fix works perfectly, the system would explore the same 40 alphabetically-first parameters forever and never reach the 138 parameters that actually matter.

**Severity:** CRITICAL — independently sufficient to explain zero promotions, even post-Rev-9.

---

### DEFECT 4: Fixed ±10% Mutation Magnitude — No Adaptive Step

**Location:** `pq_refiner.rs:254-256`

**The Problem:** Every numeric parameter gets ±10% perturbation regardless of:
- The parameter's scale (1 bp vs 10,000 bp — 10% of 1 bp rounds to 0)
- The parameter's sensitivity (gate_margin_bps at 50 → ±5, gate_protocol_bps at 100 → ±10)
- The parameter's prior evaluation history (was +10% tried? did it improve?)

A 10% step on `gate_margin_bps = 50` produces ±5 bps. On `lc_tp3_bps = 50000` it produces ±5000 bps. These are wildly different in economic significance. The fixed step also means:
- Parameters at value 1 (`bar_trades_per_bar = 8` → 10% of 8 = 0.8 → rounds to 0 → no challenger)
- Parameters at value 2-9 get a step of 0 or 1 — too coarse to be meaningful

**Evidence:** `bar_trades_per_bar = 8` → delta = 8/10 = 0 (unsigned_abs()/10 as usize = 0). The `.max(1)` saves it (delta becomes 1), but the challenger is 8→9, a 12.5% change, not 10%. For `baseline_min_trades = 32` → delta = 3, challenger is 32→35 (+9.3%) or 32→29 (-9.1%). The actual perturbation magnitude varies from 0% to 100%+ depending on rounding.

**Impact:** The mutation magnitude is neither controlled nor meaningful. Some parameters get zero perturbation (value 1-2), others get huge swings relative to their economic scale.

**Severity:** HIGH — degrades challenger quality but doesn't independently cause zero promotions (engine-replay would still differentiate).

---

### DEFECT 5: Single-Axis Only — No Combinatorial Mutations

**Location:** `pq_refiner.rs:190-293` — each Challenger has exactly one ParameterMutation

**The Problem:** Every challenger changes exactly ONE parameter. The Challenger struct accepts `vec![ParameterMutation]` (plural), but `generate_challengers()` only ever pushes single-element vectors.

In a 182-parameter space, the pairwise interaction surface is 182×181/2 = 16,521 pairs. The single-axis search explores 178 of these (one at a time). The interaction surface — where tightening the gate AND raising the TP together produces a different outcome than either alone — is completely unexplored.

**Impact:** Memecoin trading parameters are highly coupled:
- `gate_margin_bps` + `lc_tp1_bps` (tighter gate → fewer but higher-quality trades → can afford tighter TP)
- `mcap_band_lo_lamports` + `entry_tip_lamports` (lower mcap band → more volatile → need higher tip for priority)
- `dd_tier1_bp` + `lc_hard_sl_bps` + `total_risk_cap_bp` (three risk parameters that interact)

The single-axis search cannot discover these interactions. A real quant would test parameter PAIRS.

**Severity:** MEDIUM-HIGH — limits optimization ceiling but doesn't independently cause zero promotions.

---

### DEFECT 6: Thompson Sampling Disconnected from Challenger Generation

**Location:** `thompson_sampling.rs` (real Beta-Bernoulli) vs `pq_refiner.rs:generate_challengers()` (alphabetical)

**The Problem:** The system has two optimization loops that don't feed each other:

1. **Thompson sampling** (`thompson_sampling.rs`): Maintains Beta(α,β) posteriors per strategy TYPE (EntryMode × Archetype × SizingFamily × Lane). Allocates paper capital across strategy types. This is real Bayesian bandit math, properly implemented.

2. **Challenger generation** (`pq_refiner.rs:generate_challengers()`): Alphabetical single-axis ±10% perturbation of config parameters. No input from Thompson posteriors, no prioritization of parameters that belong to high-performing strategy types.

The Thompson sampling knows which strategy types are winning (currently none — all 6 types have α:β ratios below 15%). But this knowledge is NEVER used to prioritize which config parameters to perturb. A challenger that mutates a parameter belonging to a high-Thompson-posterior strategy type should get higher priority, but it doesn't.

**Impact:** The Bayesian intelligence in the system (Thompson) is walled off from the parameter search. The parameter search remains dumb alphabetical iteration regardless of what Thompson learns.

**Severity:** MEDIUM — wastes the Thompson signal but doesn't independently cause zero promotions.

---

### DEFECT 7: Cumulative PnL is -6.07 SOL — The Champion Itself is Losing

**Location:** `cumulative_pnl.json`, `tape.jsonl`

**The Problem:** The champion config (rev7-swing-v1, fingerprint 0x1f4a51e287ac2493) has:
- 1,029 prior trades at -5.90 milliSOL/trade average
- Cumulative realized: -6,068,250,439 lamports (-6.07 SOL)
- 8 new session trades at -16,120,000 lamports (-0.016 SOL)
- Sharpe ratio: -0.026 (annualized)

The refiner is trying to find a challenger that beats THIS. But the champion is already deeply negative. The `challenger_defeats_champion()` function requires the challenger to beat the champion's net by a margin. When the champion's net is -6.07 SOL, a challenger that returns -6.06 SOL would "defeat" it — but both are losing.

The deeper problem: the refiner evaluates challengers against the SAME tape that the champion was run on. If the champion is losing on this tape, challengers that make the SAME trades with slightly different parameters will also lose. The engine-replay (when it works) produces DIFFERENT trades — but the event stream is a fixed historical recording, so the market conditions are fixed. The refiner is optimizing for "less bad on past data" rather than "better on future data."

**Impact:** Even with all other defects fixed, the refiner would be optimizing a losing strategy on fixed historical data. The optimization target should be forward-looking (paper trading performance post-mutation), not backward-looking (replay of past events).

**Severity:** HIGH — fundamental design question about what the refiner is optimizing for.

---

## WHAT'S WORKING (Don't Retire These)

The audit found genuinely excellent, research-grounded components that must be preserved:

1. **SPRT (Wald 1945)** — Real sequential probability ratio test with correct milli-nat LLR increments (182/-223), correct boundaries (-2944/5023), truncation at 400 pairs. Source: `evaluator_state.rs:42-58`, `strategy_type_sprt.rs`. This is textbook Wald sequential analysis.

2. **8-Gate Evaluation** — Real FDR (Benjamini-Hochberg, α=0.05), PBO (Bailey/LdP 2014, <50%), DSR (deflated Sharpe, 30+ samples), holdout reserve (20%, peek-once budget), walk-forward purge gap (5 min), majority pass (≥4/5 folds), catastrophic veto (50% drawdown). Source: `eight_gate.rs`, `evaluator_state.rs:71-93`. This is institutional-grade OOS validation.

3. **Thompson Sampling (Thompson 1933, Auer 2002)** — Real Beta-Bernoulli posterior allocation with uniform priors, win/loss updates, n_observations tracking. Source: `thompson_sampling.rs:1-80`. Properly implemented Bayesian bandit.

4. **Engine Replay** — When `--event-stream-path` is passed, the refiner spawns `pq-engine-replay` and feeds the full event stream through the genuine engine with the mutated config. This produces REAL admission/sizing/exit decisions, not shadow approximations. The 672MB event_stream.jsonl exists and the binary is compiled. Source: `pq_refiner.rs:790-875`.

5. **Auto-Revert (Rev-10)** — Variance-based `3×σ×√n` threshold, σ=17.8M lamports, floor=50M, min 50 trades. This is a statistically grounded safety net. Source: `autonomous_bridge.rs`.

6. **Denylist** — Correctly blocks 4 envelope-affecting parameters from mutation. Defense-in-depth at both generation and promotion. Source: `pq_refiner.rs:148-172`.

7. **Integer-Only Math (§22)** — All money quantities in lamports, no floats in the decision path. FNV-1a hashing for dedup. Constitution-compliant.

8. **Determinism (§13)** — BTreeMap sorted iteration, fixed RNG seed. Reproducible across runs.

---

## RETIREMENTS (What Must Be Removed)

### RETIREMENT 1: Alphabetical BTreeMap Iteration in `generate_challengers()`
**Retire:** The `for (param_name, value) in &params` loop at line 226 that iterates alphabetically and breaks at 64.
**Replace with:** Parameter-priority-queue driven selection (see Build Plan §1).

### RETIREMENT 2: Fixed ±10% Mutation Magnitude
**Retire:** The `let delta = (val_i64.unsigned_abs() / 10).max(1) as i64` at line 255.
**Replace with:** Parameter-specific mutation magnitudes based on economic scale (see Build Plan §2).

### RETIREMENT 3: Shadow Replay as Primary Scoring Path
**Retire:** The shadow_replay() function's role as the default scoring path. It should become a fast pre-filter ONLY, never the final score.
**Replace with:** Engine-replay as the mandatory scoring path, with shadow replay as an optional fast-reject pre-filter (see Build Plan §3).

### RETIREMENT 4: The 64-Challenger Hard Cap
**Retire:** The `max_challengers = 64` cap and the `break` at line 227.
**Replace with:** Budget-controlled adaptive cap based on engine-replay throughput (see Build Plan §4).

### RETIREMENT 5: Single-Parameter-Per-Challenger Constraint (Soft)
**Retire:** The implicit constraint that `mutations: vec![ParameterMutation]` only ever has one element.
**Replace with:** Mixed single + combinatorial challengers (see Build Plan §5).

### RETIREMENT 6: Historical Refiner Log (176 runs, 0 promotions)
**Retire:** Archive the existing `refiner_log.jsonl`, `evaluator_state.json`, and `refiner_status.json`. They are contaminated by Defects 1-3 and provide no valid baseline.
**Replace with:** Fresh state initialized post-Rev-11, with first cycle producing genuine differentiated challengers.

---

## REV-11 BUILD PLAN

### Architecture: Priority-Weighted Adaptive Challenger Generation

The core insight: **the refiner should prioritize parameters by their expected impact on net SOL, use adaptive mutation magnitudes, and rotate through the full surface across cycles.** The Thompson posterior signal should feed into parameter prioritization.

---

### BUILD §1: Parameter Priority Queue (Retires Defects 1, 3)

**What:** Replace the alphabetical BTreeMap iteration with a priority queue that:
1. **Classifies every parameter** into an impact tier based on its semantic role
2. **Rotates through tiers** so the full surface is covered over N cycles
3. **Boosts parameters** belonging to strategy types with high Thompson posteriors
4. **Tracks exploration state** in evaluator_state.json (last explored index, tier rotation)

**Parameter Impact Tiers:**

| Tier | Parameters | Rationale |
|------|-----------|-----------|
| T0 (Critical Exit) | `lc_tp1/2/3_bps`, `lc_tp1/2/3_frac_bps`, `lc_trail_base/max_bps`, `lc_trail_k_div`, `lc_hard_sl_bps`, `mcap_position_*_tp*`, `target_ceiling/floor_bp` | Directly determine when and how much to sell — the #1 driver of net SOL |
| T1 (Critical Entry) | `gate_margin_bps`, `gate_fail_rate_bps`, `gate_expected_move_bps`, `gate_exit_tranches`, `mcap_band_lo/hi`, `entry_fee_bps`, `entry_tip_lamports` | Determine which trades are admitted and at what cost |
| T2 (Sizing/Risk) | `max_concurrent_positions`, `min_trade_size_lamports`, `dd_tier1/2/3_bp`, `total_risk_cap_bp`, `vol_stop_scale_bp` | Determine position size and risk envelope |
| T3 (Moon Bag) | `conditional_moon_bag_enable`, `moon_bag_velocity_threshold_bps`, `moon_bag_acceleration_window` | Asymmetric upside capture — fat-tail events |
| T4 (Reentry/Cooldown) | `reentry_cooldown_enable`, `reentry_cooldown_ticks`, `watchlist_ttl_ticks` | Re-trade timing on the same token |
| T5 (Brain/Reflect) | `brain_*`, `reflect_every_ticks`, `reflect_weight_*` | Learning system tuning — indirect effect |
| T6 (Meta/Taxonomy) | `meta_*`, `narrative_*`, `taxonomy_*` | Classification — indirect, low priority |
| T7 (Infrastructure) | `bankroll_initial`, `bar_trades_per_bar`, `baseline_*`, `confirm_ttl`, `paper_tick_period` | Infrastructure — rarely needs tuning |

**Rotation Schedule:**
- Cycle N: Explore T0 (8 params × 2 directions = 16 challengers) + T1 (7 params × 2 = 14) + T2 (5 × 2 = 10) = 40 challengers
- Cycle N+1: Explore T3 + T4 + T5 = ~24 challengers + re-test T0/T1 winners
- Cycle N+2: Explore T6 + T7 + re-test all previous winners
- Every 3rd cycle: Full surface sweep (one challenger per param, +10% only, to detect regime shifts)

**Thompson Boost:** Parameters belonging to strategy types with Thompson α/(α+β) > 0.5 get +50% priority weight. Currently all types are below 15%, so no boost applies — but as the system learns, the priority queue will automatically concentrate on winning strategy types.

**Exploration State:** Persist in evaluator_state.json:
```json
{
  "exploration_state": {
    "last_tier_cycle": {"T0": 176, "T1": 176, "T2": 176, ...},
    "tier_rotation_index": 0,
    "winners_from_last_cycle": [{"param": "lc_tp1_bps", "direction": "+10%", "netsol_delta": 1234567}],
    "full_surface_sweep_due": false
  }
}
```

**Implementation:**
- New struct `ParameterTier` with classification map (hardcoded tier assignments — these are semantic, not algorithmic)
- New struct `ExplorationState` persisted in evaluator_state.json
- `generate_challengers()` rewritten to use the priority queue + rotation
- Determinism preserved: within a tier, parameters are iterated alphabetically (BTreeMap), and the RNG seed is deterministic

**Constitution compliance:**
- §13 (determinism): Tier assignment is static, rotation index is deterministic, within-tier order is BTreeMap sorted
- §22 (integer-only): No changes to money math
- §45-56 (evaluation): The 8-gate, SPRT, FDR, PBO, DSR are all preserved

---

### BUILD §2: Adaptive Mutation Magnitudes (Retires Defect 4)

**What:** Replace the flat `value / 10` with parameter-specific mutation schedules based on economic scale.

**Magnitude Schedule:**

| Parameter Type | Mutation | Rationale |
|---------------|----------|-----------|
| BPS parameters (value in basis points) | ±value × 0.15 (±15%) | BPS params are finely grained; 15% is a meaningful but safe step |
| Lamport parameters (value in lamports) | ±value × 0.10 (±10%) | Lamport params are larger scale; 10% is standard |
| Tick parameters (value in ticks) | ±value × 0.20 (±20%) | Tick params are coarsely grained; 20% provides meaningful variation |
| Count parameters (value is a count) | ±1 or ±2 (absolute) | Counts like `max_concurrent_positions=3` → try 2 and 4 |
| Bool/enable parameters | Toggle 0→1/1→0 | No magnitude — binary |
| Zero-valued parameters | Skip | Can't perturb zero |

**Additionally — Multi-step mutations for proven winners:**
If a parameter's +10% challenger defeated the champion in a previous cycle, the next cycle should test +20% (double step in the same direction) and +5% (half step — bracket the optimum). This is a simple form of directional momentum in the search.

**Implementation:**
- New function `mutation_magnitude(param_name: &str, current_value: i64) -> Vec<i64>` returning 1-3 proposed values
- Classification by parameter name suffix: `_bps` → BPS, `_lamports` → Lamport, `_ticks` → Tick, `_enable` → Bool
- The directional momentum requires reading `exploration_state.winners_from_last_cycle`

---

### BUILD §3: Engine-Replay as Mandatory Scoring Path (Retires Defect 2)

**What:** Make engine-replay the ONLY scoring path that matters for promotion decisions. Shadow replay becomes a fast-reject pre-filter.

**Two-Phase Evaluation:**

**Phase A (Fast Reject — Shadow Replay):**
- Run shadow_replay() on all generated challengers
- If shadow_replay net == champion net (the no-op catch-all), AND the parameter is in the no-op set, mark as "needs engine-replay"
- If shadow_replay net differs from champion by < 1% in either direction, mark as "marginal — engine-replay optional"
- If shadow_replay shows > 10% degradation, fast-reject (the mutation is clearly harmful even in the approximate model)

**Phase B (Mandatory — Engine Replay):**
- For all challengers not fast-rejected, run engine-replay
- The engine-replay score is the SOLE input to `challenger_defeats_champion()` and the 8-gate
- If engine-replay fails for a challenger (subprocess error), the challenger is REJECTED (not scored on shadow approximation)

**Throughput Control:**
- Engine-replay processes 672MB of event data per challenger. At ~2 sec/challenger (estimated), 64 challengers = ~128 sec. This is acceptable within the 2h refiner cycle.
- If engine-replay throughput is too slow, reduce challenger count (not quality) — the adaptive cap (§4) handles this.

**Fallback Safety:**
- If `event_stream.jsonl` is missing or corrupted, the refiner logs a CRITICAL warning and skips the cycle rather than scoring on shadow approximations alone. No promotion decisions are made without engine-replay evidence.

---

### BUILD §4: Adaptive Challenger Cap (Retires Defect 4's cap constraint)

**What:** Replace the fixed 64 cap with an adaptive budget based on engine-replay throughput and tier priority.

**Logic:**
- Base budget: 64 challengers per cycle (matches historical throughput)
- If all T0+T1 challengers fit within 64, fill remaining slots with T2+T3
- If T0+T1 exceeds 64, prioritize T0 (exit params) over T1 (entry params)
- If engine-replay took > 60 sec for 64 challengers last cycle, reduce to 48 next cycle
- If engine-replay took < 30 sec, increase to 80 (explore more surface)
- Min: 24 (must explore at least T0 every cycle), Max: 128 (engine-replay time budget)

**Implementation:**
- Track `last_engine_replay_seconds` in exploration_state
- `generate_challengers()` takes a `budget: usize` parameter derived from this

---

### BUILD §5: Combinatorial Challengers (Retires Defect 5 — Phased)

**Phase 1 (Rev-11): Paired mutations on known-coupled parameters**
- Hardcode 8 known-coupled parameter pairs (based on trading domain knowledge):
  1. `(gate_margin_bps, lc_tp1_bps)` — tighter gate + tighter TP
  2. `(mcap_band_lo_lamports, entry_tip_lamports)` — lower band + higher tip
  3. `(dd_tier1_bp, lc_hard_sl_bps)` — drawdown tier + hard stop
  4. (lc_tp1_bps, lc_tp1_frac_bps) — TP level + TP fraction
  5. (lc_tp2_bps, lc_tp2_frac_bps) — TP2 level + TP2 fraction
  6. (lc_trail_base_bps, lc_trail_max_bps) — trail range
  7. (max_concurrent_positions, min_trade_size_lamports) — sizing coherence
  8. (vol_stop_scale_bp, lc_hard_sl_bps) — vol stop + hard stop

- Each pair generates 4 challengers: (A+10%, B+10%), (A+10%, B-10%), (A-10%, B+10%), (A-10%, B-10%)
- These 32 combinatorial challengers (8 pairs × 4) are added to the single-axis challengers from §1

**Phase 2 (Rev-12):** Thompson-guided combinatorial — use Thompson posteriors to identify which parameter combinations belong to the same strategy type and test them together.

---

### BUILD §6: Forward-Looking Validation Window (Addresses Defect 7)

**What:** The refiner currently evaluates challengers against the FULL accumulated tape (all 1,028 trades). This is backward-looking. Add a forward-looking window:

**Rolling Window Evaluation:**
- The engine-replay should process only the LAST N ticks of the event stream (e.g., last 20,000 ticks = ~1.4 hours at 250ms/tick)
- This tests the challenger against RECENT market conditions, not the full historical period
- The champion's score is computed on the SAME window for fair comparison
- The full-tape score is retained as a secondary metric (regression check)

**Rationale:** Memecoin regimes shift rapidly. A config that was optimal 3 days ago may be suboptimal now. The forward-looking window ensures the refiner optimizes for CURRENT market conditions.

**Implementation:**
- New CLI arg `--replay-window-ticks 20000` on pq-engine-replay
- The engine-replay binary reads the event stream but only processes events within the window
- The champion config is re-replayed on the same window for comparison

---

### BUILD §7: State Reset and Fresh Initialization (Retires Defect-contaminated history)

**What:** Archive the contaminated historical state and initialize fresh evaluator state post-Rev-11.

**Actions:**
1. Archive `evaluator_state.json` → `evaluator_state_pre_rev11.json.bak`
2. Archive `refiner_log.jsonl` → `refiner_log_pre_rev11.jsonl.bak`
3. Initialize fresh evaluator_state.json with:
   - `exploration_state` (new) — tier rotation index 0, all tiers at cycle 0
   - Thompson posteriors preserved (6 types — these are valid, they reflect real trade outcomes)
   - SPRT ledgers RESET (the old ledgers were built on no-op challengers)
   - `tested_hashes` cleared (new challenger generation will produce new hashes)
   - `challenger_history` cleared
4. The first Rev-11 refiner cycle will be the first genuine differentiated evaluation in the system's history

---

## IMPLEMENTATION SEQUENCE

| Step | Component | Files Modified | Estimated LOC |
|------|-----------|---------------|---------------|
| 1 | Parameter tier classification + ExplorationState | `pq_refiner.rs`, `evaluator_state.rs` | +200 |
| 2 | Rewrite `generate_challengers()` with priority queue | `pq_refiner.rs` | -60, +150 (net +90) |
| 3 | Adaptive mutation magnitudes | `pq_refiner.rs` | +80 |
| 4 | Engine-replay mandatory scoring path | `pq_refiner.rs` | +60 |
| 5 | Adaptive challenger cap | `pq_refiner.rs` | +30 |
| 6 | Combinatorial challengers (8 pairs) | `pq_refiner.rs` | +80 |
| 7 | Rolling window for engine-replay | `pq_engine_replay.rs`, `pq_refiner.rs` | +100 |
| 8 | State reset + fresh initialization | `autonomous_bridge.rs`, data files | +30 |
| 9 | Tests (golden tape, prop tests, regression) | `pq-regression/` | +200 |
| 10 | Compilation + verification | — | — |

**Total estimated: ~970 new LOC, ~60 retired LOC**

---

## VERIFICATION CRITERIA

The Rev-11 build must satisfy ALL of:

1. **First refiner cycle produces ≥3 unique net-SOL values** across challengers (proves engine-replay differentiation is working)
2. **First refiner cycle explores ≥1 T0 parameter** (proves tier rotation is working)
3. **Within 3 cycles, all 7 tiers are explored** (proves full surface coverage)
4. **Within 10 cycles, at least 1 combinatorial challenger is evaluated** (proves paired mutations work)
5. **All golden tape tests pass** (digest stability invariant)
6. **All prop tests pass** (property-based invariants)
7. **All pq-regression tests pass** (50 existing tests)
8. **No new `unsafe` blocks** (§24(b) safety policy)
9. **Determinism preserved** — same input config → same challenger set (§13)
10. **Integer-only math** — no floats in decision path (§22)

---

## RISK ASSESSMENT

| Risk | Probability | Impact | Mitigation |
|------|------------|--------|------------|
| Engine-replay too slow for 64 challengers | Medium | Refiner cycle exceeds time budget | Adaptive cap (§4) reduces count; can fall back to 24 |
| Rolling window misses regime shifts | Low | Optimizes for wrong conditions | Full-tape score retained as regression check |
| Tier classification is wrong | Low | Wrong params prioritized | Tiers are semantically assigned by domain knowledge, not algorithmic |
| Combinatorial challengers explode in count | Low | Too many challengers | Only 8 hardcoded pairs in Rev-11 (32 challengers) |
| State reset loses valid Thompson posteriors | Very Low | Loses strategy-type learning | Thompson posteriors are PRESERVED in reset |

---

## SUMMARY OF RETIREMENTS vs BUILDS

| Retired | Replaced By |
|---------|-------------|
| Alphabetical BTreeMap iteration | Priority queue with tier rotation (§1) |
| Fixed ±10% mutation | Adaptive magnitudes by parameter type (§2) |
| Shadow replay as primary score | Engine-replay mandatory + shadow as pre-filter (§3) |
| Fixed 64-challenger cap | Adaptive budget based on throughput (§4) |
| Single-param-only challengers | Mixed single + 8 coupled pairs (§5) |
| Full-tape-only evaluation | Rolling window + full-tape regression check (§6) |
| Contaminated historical state | Fresh init with preserved Thompson posteriors (§7) |

---

## APPROVAL REQUEST

This build plan proposes **7 structural changes** across **3 source files** and **2 data files**, adding ~970 LOC and retiring ~60 LOC. The changes are:

1. Parameter priority queue with tier rotation
2. Adaptive mutation magnitudes
3. Engine-replay as mandatory scoring path
4. Adaptive challenger cap
5. Combinatorial challengers (8 coupled pairs)
6. Rolling window for forward-looking validation
7. State reset and fresh initialization

**I await Alon's approval before writing any code.**

---

*"The best quant isn't the one with the most models — it's the one who knows which models are dead weight and has the discipline to retire them." — Anonymous Citadel Desk Head*

---

**END OF REPORT**
