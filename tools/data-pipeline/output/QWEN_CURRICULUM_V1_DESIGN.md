# Qwen Curriculum v1 — REVISED Design Document
## Specialized Solana Pump.fun/PumpSwap Live Decision Brain

**Status:** APPROVED v3 — proceeding to eval freeze + export generation  
**Revision:** v3 — incorporates all final decisions  
**Date:** August 29, 2026  
**Training route:** PRIMARY = Qwen 27B FULL-PARAMETER BF16 via Unsloth + Accelerate + DeepSpeed ZeRO-3 on 3×96GB GPUs. BF16 LoRA = fallback only. No smoke-test stage; real run's early steps = feasibility validation. No Hermes/LLM required during training — self-contained handoff package.  
**Corpora:** laserstream_gold_v3 (FROZEN), slinky_gold_v3 (FROZEN), rust_gold_v1 (FROZEN_IMMUTABLE), narrative_gold_v1.1 (FROZEN_IMMUTABLE)

---

## 1. Architecture Overview

```
┌──────────────────────────────────────────────────────────────────┐
│                    PRODUCTION PIPELINE                            │
│                                                                  │
│  LaserStream gRPC ──→ Rust Causal State ──→ Qwen Ranks ──→     │
│  (real-time events)   (~25-50 candidates)   (continuous utility  │
│                                              + BUY/WATCH/SKIP)    │
│                                              │                   │
│                                    Rust Final Validation          │
│                                    (freshness/risk/execution)     │
│                                              │                   │
│                                    Execute or Skip                │
└──────────────────────────────────────────────────────────────────┘
```

**Training goal:** Qwen learns to reason cross-sectionally across ~25-50 live Pump candidates using causal state only, producing continuous robust-executable-utility rankings with secondary BUY/WATCH/SKIP decisions. Panels allow NO_BUY when nothing has robust positive evidence. Never force a winner.

**Training route (priority order):**
1. **Primary:** Full-parameter Qwen 27B BF16 via Unsloth + Accelerate + DeepSpeed ZeRO-3 on 3×96GB GPUs. No smoke-test stage.
2. **Fallback:** BF16 LoRA on single 96GB RTX PRO 6000 via Unsloth Studio.
3. All exports are **training-method-neutral** — no method-specific assumptions in schemas.
4. **Self-contained handoff:** Training run requires NO Hermes/LLM calls. Complete package with train.py, configs, launch/resume scripts, monitoring, runbook created before Hermes shutdown.

**No benchmark blockers:** No vanilla Qwen benchmark or RTX 6000 inference benchmark required before training. Immutable eval set MUST exist before training-data export. Post-training eval/shadow testing required before live promotion.

---

## 2. Frozen Corpus Inventory (Re-examined)

### 2A. Slinky Gold v3 — FOUR layers, not one

| Layer | Files | Rows (est.) | Columns | Role |
|-------|-------|-------------|---------|------|
| **pump_state_v3** | 134 | 33,855,448 | 105 (103 causal + 2 provenance) | **CAUSAL INPUT** — decision-time market state |
| pump_outcome_v3 | 134 | 33,581,765 | 92 | **TARGET EVIDENCE** — L2 outcomes (returns, barriers, graduation, survival) |
| counterfactual_trade_v3 | 134 | 33,855,448 | 106 | **TARGET EVIDENCE** — execution economics (5 sizes @ 250ms latency, economic_class) |
| policy_eval_v3 | 134 | ~850K | varies | **TARGET EVIDENCE** — champion vs oracle policy |

**pump_state_v3 causal fields (103 — all available at decision time, NO future leakage):**

| Category | Fields | Count |
|----------|--------|-------|
| Trade-level | event_time_unix_ms, mint, state_id, seq, venue, trade_side, is_buy, sol_amount_sol, token_amount_raw, price_sol | 10 |
| Bonding curve | v_sol_bonding_curve_sol, v_tokens_bonding_curve_raw, curve_pct_depleted, graduation_proximity_pct, v_sol_depletion_rate_5s, v_tokens_accumulation_rate_5s | 6 |
| Velocity/momentum | trade_velocity_1s/5s/30s, vol_velocity_5s/30s, vol_acceleration, price_momentum_5s/30s_bp, mcap_momentum_5s_bp, price_change_since_launch_bp, price_volatility_30s_bp | 11 |
| Buy/sell imbalance | buy_sell_imbalance_5s/30s, buy_pressure, net_flow_sol, buy_vol_sol, sell_vol_sol, total_vol_sol | 7 |
| Breadth/concentration | unique_buyers_5s/so_far, unique_sellers_5s/so_far, unique_wallets_so_far, buyer/seller_concentration_ratio, top1/top5/top10_buyer_pct, holder_concentration_hhi, initial_gini | 12 |
| Coordination/sybil | coordinated_buy_ratio, wash_trade_ratio, sybil_cluster_size, rapid_rebuy_count, wallets_also_selling, repeat_buyer/seller_ratio, toxic_flow_indicator | 8 |
| Trade size stats | avg/median/min/max_trade_size_sol, trade_size_std_sol, largest_buy/sell_pct_of_vol | 7 |
| Volume counts | buy_count_so_far, sell_count_so_far, trade_count_so_far | 3 |
| Liquidity/curve dynamics | liquidity_sol, liquidity_change_5s/30s_sol, curve_depletion_velocity_5s/30s | 5 |
| Market context (cross-sectional) | market_active_mints_5m, market_avg_buy_pressure_5m, market_total_vol_5m_sol, market_cap_sol | 4 |
| Launch/age | seconds_since_launch, minutes_since_launch, initial_market_cap_sol, initial_price_sol, initial_supply_raw, initial_holder_count | 6 |
| Creator | creator, creator_past_tokens, creator_past_rugs | 3 |
| Token metadata | token_name, token_symbol, token_decimals | 3 |
| Flags | is_mayhem, is_zombie, is_graduated, top10_pct_suspect, supply_bug_corrected, dev_buy_pct_corrected, data_quality_score | 7 |
| Derived (computed) | avg_wallet_trade_count, initial_top10_pct_corrected, market_cap_sol_lamports | 3 |
| Provenance | source, source_hash, run_uuid, git_sha, code_config_hash, pipeline_version, producer_version | 7 |
| **Leakage field (NEVER input)** | seconds_to_graduation | 1 |

**pump_outcome_v3 target fields (92 — supervision only, NEVER in input):**

Key targets: ret_1s through ret_300s (bp + double), mfe_bp, mae_bp, peak_bp, hit_plus/minus_N_bp_time (barrier timing), graduated_after_state, collapsed_50pct_within_300s, survived_60s/300s, seconds_to_graduation, right_censored_Ns flags, time_to_peak_seconds, plus_100_before_minus_30, post_grad_price_change_300s_bp.

**counterfactual_trade_v3 (106 cols — execution economics, TARGET only):**

5 pre-computed size scenarios (sz005, sz010, sz025, sz050, sz100) at 250ms latency with: feasible, entry/exit impact, gross/net pnl, return bp. Plus per-row economic_class (SKIP/TOXIC/STRONG/BAD/GOOD/MARGINAL), eligible, exit_feasible, risk_reward_ratio, hold_duration, entry/exit fees, slippage, stop_loss_bp, tp_target_bp.

### 2B. LaserStream Gold v3 — FOUR layers

| Layer | Rows | Columns | Role |
|-------|------|---------|------|
| **L1 (l1_pump_state_v3)** | 3,790,870 | 48 | **CAUSAL INPUT** — slot-level microstructure |
| L2 (l2_outcome_v3) | 3,790,870 | 52 | **TARGET EVIDENCE** — returns 1s-300s, MFE/MAE, barriers, migration |
| L3 (l3_counterfactual_v3) | 94,771,750 | 21 | **TARGET EVIDENCE** — 25 scenarios (5 latencies × 5 sizes) |
| L4 (l4_policy_eval_v3) | 3,790,870 | varies | **TARGET EVIDENCE** — champion vs oracle policy |

**L1 causal fields (48):**
price_lamports_per_rawtok, slot, timestamp_ms, venue, event_type, trade_side, trader_b58, sol_traded_lamports, tokens_traded_raw, curve_virtual_sol/token, curve_real_sol/token, curve_complete, pool_base/quote_reserve, pool_lp_supply, ix_fee_bps, ix_amount_in/out, ix_max_amount_in, ix_min_amount_out, token_decimals, mint_b58, state_id, right_censored_1s/2s/5s/10s/30s/60s/120s/300s, observed_no_trade_1s...300s, price_source, price_scale_tier, curve_account_b58, pool_account_b58, raw_hash, capture_start/end_ms.

**L3 counterfactual (25 scenarios):**
5 latencies: 0ms, 100ms, 500ms, 1000ms, 2000ms × 5 sizes: 0.05, 0.10, 0.25, 0.50, 1.0 SOL.
Per scenario: feasible, entry_executable, exit_executable, sim_return_pct, outcome_class (tp_hit/sl_hit/entry_failed/timeout_no_exit/timeout_or_marginal), capacity_constrained, tokens_bought_raw, exit_sol_received_lamports, entry_price, feasibility_reason.

### 2C. Temporal Reality

| Corpus | Period | Duration | Mints | Concurrent/±60s |
|--------|--------|----------|-------|-----------------|
| Slinky pump_state_v3 | Jun 5 – Jul 14, 2026 | 39 days | 622,870 | 45–106 |
| LaserStream L1 | Aug 24, 2026 | ~5 hours | 7,016 | 200–374 |

**Separate panel pools are correct.** Slinky and LaserStream cover different periods with different mint sets. Cross-corpus panels are unnecessary. Each source builds its own cross-sectional panels.

### 2D. Rust Gold v1

| Split | Commits | Code Changes | Repairs | Trajectories | Repo States |
|-------|---------|-------------|---------|--------------|-------------|
| Train | 289 | 541 | 534 | 163 | 21 |
| Val | 39 | — | — | — | — |
| **Test (eval)** | **29** | — | — | — | — |
| Total | 737 | 541 | 534 | 163 | 21 |

### 2E. Narrative Gold v1.1

| Layer | Records | Training Eligible (A+B) |
|-------|---------|------------------------|
| Content | 1,471 | 847 (pump_specific + solana_memecoin_regime) |
| Claims | 1,471 | 847 |
| States | 1,183 | — |
| Validations | 615 | — |
| Strategy cards | 97 (0 GOLD, 26 SILVER, 71 BRONZE) | — |
| Trajectories | 490 (202 GOLD 4+ stages, 288 SILVER 3 stages) | — |
| Temporal | EX_ANTE 33, RETROSPECTIVE 14, AMBIGUOUS 1,424 | — |

---

## 3. Canonical Candidate Schema

Both Slinky and LaserStream are normalized into a canonical candidate representation. Source-specific fields are optional.

### 3A. Canonical Causal Input (decision-time fields ONLY)

```json
{
  "mint_id": "hashed_mint",
  "source": "slinky | laserstream",
  "timestamp_ms": 1787549744081,
  "venue": "pumpfun | pumpswap",
  "age_seconds": 42.5,
  "causal_state": {
    "price_sol": 0.0001234,
    "market_cap_sol": 45.2,
    "curve_pct_depleted": 0.083,
    "graduation_proximity_pct": 8.3,
    "bonding_curve_virtual_sol": 50.0,
    "bonding_curve_real_sol": 4.17,
    "trade_velocity_5s": 2.5,
    "trade_velocity_30s": 1.8,
    "vol_velocity_5s": 12.3,
    "buy_pressure": 0.65,
    "buy_sell_imbalance_5s": 0.3,
    "buy_sell_imbalance_30s": 0.15,
    "net_flow_sol": 2.3,
    "unique_buyers_5s": 8,
    "unique_wallets_so_far": 45,
    "buyer_concentration_ratio": 0.12,
    "top5_buyer_pct": 0.35,
    "holder_concentration_hhi": 0.08,
    "coordinated_buy_ratio": 0.05,
    "wash_trade_ratio": 0.02,
    "toxic_flow_indicator": 0.01,
    "liquidity_sol": 4.17,
    "liquidity_change_5s_sol": 0.3,
    "curve_depletion_velocity_5s": 0.002,
    "market_active_mints_5m": 120,
    "market_avg_buy_pressure_5m": 0.58,
    "creator_past_tokens": 3,
    "creator_past_rugs": 1,
    "is_mayhem": false,
    "is_zombie": false,
    "is_graduated": false
  },
  "source_specific": {
    "slinky": {
      "seconds_since_launch": 42.5,
      "initial_market_cap_sol": 12.5,
      "rapid_rebuy_count": 3,
      "sybil_cluster_size": 2,
      "trade_count_so_far": 87,
      "data_quality_score": 0.95
    },
    "laserstream": {
      "slot": 123456789,
      "curve_complete": false,
      "pool_base_reserve": null,
      "pool_quote_reserve": null,
      "ix_fee_bps": 100,
      "right_censored_5s": false,
      "observed_no_trade_5s": false,
      "trader_b58": "hashed_trader"
    }
  }
}
```

**Leakage boundary (BINDING):**
- Slinky: `seconds_to_graduation` is NEVER input. All `ret_Ns`, `mfe_bp`, `mae_bp`, `hit_plus/minus_N_bp_time`, `graduated_*`, `collapsed_*`, `survived_*` are TARGET only.
- LaserStream: All L2 fields (returns, MFE/MAE, barriers) and all L3 fields (sim_return, outcome_class, feasible) are TARGET only. L1 contains only current-event microstructure.
- No future return of any horizon is used as decision-time input. `ret_1s` is NOT a velocity proxy — it is leakage.

### 3B. Canonical Target (supervision only)

```json
{
  "mint_id": "hashed_mint",
  "l2_evidence": {
    "ret_1s_bp": 50, "ret_5s_bp": 120, "ret_30s_bp": -200, "ret_300s_bp": -1500,
    "mfe_bp": 300, "mae_bp": -800, "peak_bp": 300,
    "graduated_after_state": false, "collapsed_50pct_within_300s": true,
    "survived_60s": true, "survived_300s": false,
    "right_censored_300s": false, "time_to_peak_seconds": 12.5,
    "plus_100_before_minus_30": true
  },
  "l3_surface_compressed": {
    "feasible_ratio": 0.60,
    "median_net_return_pct": 0.015,
    "worst_net_return_pct": -0.08,
    "best_net_return_pct": 0.12,
    "max_feasible_size_sol": 0.25,
    "latency_sensitivity": 0.04,
    "size_sensitivity": 0.06,
    "latency_breakpoint_ms": 500,
    "capacity_constrained_sizes": [0.50, 1.0],
    "outcome_distribution": {"tp_hit": 9, "sl_hit": 3, "timeout_or_marginal": 5, "entry_failed": 5, "timeout_no_exit": 3}
  },
  "l3_surface_full": "present only in ~10-15% execution-surface examples",
  "slinky_counterfactual": {
    "economic_class": "SKIP",
    "eligible": true,
    "net_return_pct": -0.05,
    "risk_reward_ratio": 0.3,
    "exit_feasible": true,
    "hold_duration_seconds": 45.0
  },
  "continuous_utility": {
    "robust_executable_utility": -0.12,
    "feasibility_score": 0.60,
    "downside_risk": -0.08,
    "upside_potential": 0.12,
    "size_robustness": 0.40,
    "latency_robustness": 0.60,
    "censoring_adjusted": true
  }
}
```

---

## 4. L3 Representation Strategy

### 4A. Normal panels (85-90% of decision examples)

Compress 25 scenarios into **deterministic features** — NO 5×5 table in prompt:

| Feature | Computation | Tokens |
|---------|-------------|--------|
| feasible_ratio | count(feasible=true) / 25 | ~5 |
| median_net_return_pct | median(sim_return_pct where feasible) | ~8 |
| worst_net_return_pct | min(sim_return_pct where feasible) | ~8 |
| best_net_return_pct | max(sim_return_pct where feasible) | ~8 |
| max_feasible_size_sol | largest size with any feasible latency | ~8 |
| latency_sensitivity | std(sim_return_pct) across latency at median size | ~8 |
| size_sensitivity | std(sim_return_pct) across size at median latency | ~8 |
| latency_breakpoint_ms | first latency where feasibility drops below 50% | ~10 |
| capacity_constrained_sizes | list of sizes where capacity_constrained=true | ~15 |
| outcome_distribution | {tp_hit: N, sl_hit: N, ...} | ~30 |

**Total: ~110 tokens** vs ~500+ for a full 5×5 table.

### 4B. Execution-surface detail examples (10-15% of decision examples)

~100-150 dedicated examples with fuller 5×5 scenario detail so Qwen learns the underlying sensitivity structure. These lean LaserStream (which has true 5×5 latency×size grid). Slinky counterfactual only has 5 sizes at 250ms — used for size sensitivity but not full latency surface.

---

## 5. Supervision Design — Continuous Utility is PRIMARY

### 5A. Primary Supervision: Continuous Robust Executable Utility (v1, versioned)

**robust_utility_v1** — exact deterministic formula, frozen at eval freeze time. Future versions may change weights without altering source Gold data. The utility score is NOT the sole ground truth — all underlying continuous evidence is preserved separately in every export record.

```
robust_executable_utility_v1 = (
    0.20 * feasibility_score
  + 0.25 * tanh(3 * net_sol_economics)
  - 0.20 * tanh(3 * abs(downside_risk))
  + 0.10 * tanh(2 * upside_potential)
  + 0.10 * size_robustness
  + 0.10 * latency_robustness_or_null
  + 0.05 * censoring_adjustment
)
```

**Component definitions:**

| Component | Source | Definition | Range |
|----------|--------|------------|--------|
| feasibility_score | L3 | feasible_ratio = count(feasible=true) / total_scenarios | [0, 1] |
| net_sol_economics | L3 | median(sim_return_pct where feasible) — net after fees/slippage | ~[-0.5, 0.5] |
| downside_risk | L2 | mae_bp / 10000 — max adverse excursion as fraction | [0, ∞) |
| upside_potential | L2 | mfe_bp / 10000 — max favorable excursion as fraction | [0, ∞) |
| size_robustness | L3 | count(feasible at largest 3 sizes) / 3 — capacity scaling | [0, 1] |
| latency_robustness | L3 | count(feasible at all 5 latencies for median size) / 5 | [0, 1] or NULL |
| censoring_adjustment | L2 | 1 - max(right_censored flags) — observation quality | [0, 1] |

**Slinky latency_robustness:** NULL (only 250ms available). Contribution = 0.0. Source/capability mask explicitly marks this as missing — never inferred/fabricated.

**Output range:** approximately [-0.35, +0.55]

**Evidence preserved separately (NOT compressed into utility):**
feasibility_ratio, net_sol_economics, downside_risk, upside_potential, size_robustness, latency_robustness, censoring_adjustment, max_feasible_size, latency_breakpoint, outcome_distribution, l2_ret_5s/30s/300s, mfe_bp, mae_bp, peak_bp, collapsed_50pct_within_300s

### 5B. Secondary Supervision: BUY/WATCH/SKIP (absolute quality + panel-relative ranking)

BUY/WATCH/SKIP uses **absolute quality criteria** (not top-10%-only), then panel-relative ranking to select at most 1-3 highest:

- **BUY (absolute, all must hold):**
  - robust_executable_utility > 0.0
  - feasibility_score > 0.40
  - net_sol_economics > 0.0
  - NOT collapsed_50pct_within_300s
  - Then select at most ~1-3 highest-ranked candidates per panel

- **SKIP (any):**
  - robust_executable_utility < -0.10
  - OR feasibility_score < 0.20
  - OR collapsed_50pct_within_300s = true

- **WATCH:** everything else — promising but insufficient/fragile evidence
- **NO_BUY:** when no candidate clears absolute BUY threshold → correct output is NO_BUY. Never force capital deployment.

### 5C. Continuous Labels Preserved

Every candidate carries continuous labels for eval:
- feasibility_ratio, median/worst/best_net_return
- max_feasible_size, latency_sensitivity, size_sensitivity, latency_breakpoint
- l2_return_5s/30s/300s, mfe_pct, mae_pct
- robust_executable_utility (the primary target)
- panel_rank (1 to N within panel)

---

## 6. Eval Freeze Design (MUST be created before training export)

### 6A. Market Eval — Separate Strata, Mint-Disjoint, Spacing Caps

**Two independent eval strata** (Slinky + LaserStream cover different periods):

**Slinky eval stratum:**
- Chronological split: last 20% of time-ordered panels → eval
- Mint-disjoint: no eval mint appears in any training panel (mint assigned entirely to one split)
- Spacing cap: panels spaced ≥30min apart to reduce autocorrelation
- Overlap cap: no candidate set overlap >30% between any two eval panels
- Target: ~200-300 eval panels

**LaserStream eval stratum:**
- Chronological split: last 20% of 5h window → eval
- Mint-disjoint from training
- Spacing cap: panels spaced ≥2min apart
- Target: ~25-40 eval panels

**Total market eval: ~225-340 panels** (each 25-50 mints → ~8,000-17,000 mint-decisions)

### 6B. Market Eval Panel Composition (9 categories — corrected from manifest error)

| # | Category | Target % | Source | Purpose |
|---|----------|---------|--------|---------|
| 1 | NO-EDGE / ordinary periods | 15% | Slinky+LaserStream | Test SKIP calibration on boring markets; true no-edge panels |
| 2 | Runners (graduated / big returns) | 15% | Slinky+LaserStream | Test BUY identification |
| 3 | Rugs (collapsed -50% within 300s) | 15% | Slinky+LaserStream | Test SKIP/risk detection |
| 4 | False positives (onchain strong, outcome bad) | 10% | L1→L2 pairs | Test overconfidence penalty |
| 5 | Missed opportunities (weak signal, big return) | 10% | Slinky | Test WATCH calibration |
| 6 | Similar causal state, different outcome | 10% | Slinky+LaserStream pairs | Test discriminative ability |
| 7 | Capacity/latency-sensitive trades | 10% | LaserStream L3 | Test size×latency reasoning |
| 8 | Migration/PumpSwap cases | 5% | LaserStream | Test regime transition |
| 9 | Hard SKIP / extreme risk | 10% | Slinky (collapsed) | Test extreme risk avoidance |

**Note:** 9 categories, not 8. The manifest is corrected.

### 6C. Rust Repair Eval

- Use existing rust_gold_v1 test split: 29 commits
- Repo state = snapshot at parent of each test commit (PRE-COMMIT state)
- No training example references any test-split commit SHA or repair chain
- `repo_state_v1.jsonl` filtered: exclude any snapshot at/after earliest test commit

### 6D. Eval Metrics

**Market decision metrics:**

| Metric | Description | Target |
|--------|-------------|--------|
| Ranking Kendall-τ | Predicted vs ground-trank rank correlation | > 0.3 |
| Net-SOL utility correlation | Predicted vs actual continuous utility | > 0.4 |
| Regret | Top-1 pick actual utility vs oracle top-1 | Reported |
| BUY precision | % predicted BUYs that are true positive | > 0.6 |
| SKIP recall | % true SKIPs correctly predicted | > 0.8 (safety) |
| Calibration | Brier score on utility confidence | < 0.25 |
| Tail loss | Max realized loss on predicted BUYs | Reported |
| Turnover | Decision stability across adjacent panels | Reported |
| False positive rate | BUY predictions on rugs | < 5% |
| Missed opportunity rate | WATCH/SKIP on true runners | < 30% |
| NO-BUY accuracy | Correctly outputs no-action on no-edge panels | > 70% |
| Latency scenario accuracy | Per-scenario decision across 25 cells | No cell < 50% |
| Capacity awareness | Correctly identifies constrained sizes | > 70% |

**Narrative ablation design:**
- Training schema has optional narrative context field.
- Apply narrative dropout/absence in ~40% of SFT examples so Qwen learns on-chain competence without social context.
- Eval uses PAIRED identical panels: A) narrative OFF, B) narrative ON — to quantify incremental narrative value.
- Delta in ranking accuracy, false-positive rate, and calibration between ON/OFF pairs = narrative value signal.

**Contrast pair metrics:**

| Metric | Description |
|--------|-------------|
| Contrast discrimination | Same causal state, different outcome → different decision |
| Onchain-strong/narrative-weak | Model doesn't over-weight narrative when onchain contradicts |
| Champion false-positive | Model rejects champion's bad calls |
| Champion missed-opportunity | Model identifies runners champion missed |

**Rust repair metrics:**

| Metric | Target |
|--------|--------|
| Diagnosis accuracy (root cause subsystem) | > 0.5 |
| Repair plausibility (correct file/function) | > 0.4 |
| No hallucination (real code structures only) | 100% |

---

## 7. Market Episode Export Design

### 7A. Slinky Panel Construction (from pump_state_v3)

At each anchor timestamp `t` (30min spacing):
1. Select all mints with a pump_state_v3 state within ±60s of `t` across ALL 134 files
2. If count < 25, skip (insufficient cross-section)
3. If count > 50, sample 50 by liquidity-weighted selection (curve_pct_depleted × liquidity_sol)
4. **Never** use any pump_outcome_v3 or counterfactual_trade_v3 field to select the universe
5. Input = 103 causal fields from pump_state_v3 (normalized to canonical schema)
6. Targets = joined from pump_outcome_v3 (by mint + event_time_unix_ms) + counterfactual_trade_v3 (by state_id)

**Slinky counterfactual note:** Only 5 size scenarios at 250ms latency. For L3 compressed features: feasibility_ratio computed from 5 size scenarios (not 25). Latency_sensitivity = null (no latency variation). Size_sensitivity computed from 5 sizes. Full 5×5 latency×size surface available only from LaserStream L3.

### 7B. LaserStream Panel Construction (from L1)

At each anchor timestamp `t` (2min spacing):
1. Select all mints with an L1 state within ±15s of `t`
2. If count < 25, skip; if > 50, sample 50 by liquidity-weighted selection
3. Input = 48 causal fields from L1 (normalized to canonical schema)
4. Targets = joined from L2 (by state_id) + L3 (by state_id, all 25 scenarios)

### 7C. Market Source Balance

| Source | Train Panels | Eval Panels | Rationale |
|--------|-------------|-------------|-----------|
| Slinky | ~600 (stratified by day/regime) | ~200-300 | 39-day regime diversity, 622K mints |
| LaserStream | ~120 (all eligible) | ~25-40 | High-fidelity microstructure, 25-scenario L3 |
| **Total** | **~720** | **~225-340** | Roughly balanced, stratified by regime/mint |

**Sampling rule:** Initial market-decision sampling is roughly balanced Slinky/LaserStream, then stratified by regime/mint rather than row count. Execution/capacity-specific tasks lean LaserStream (has true 25-scenario L3).

### 7D. Panel-Relative Supervision

For each panel:
1. Compute continuous robust_executable_utility for every candidate
2. Rank candidates by utility within the panel
3. Derive BUY/WATCH/SKIP using panel-relative thresholds
4. If no candidate has robust positive utility → correct output is NO_BUY
5. Preserve continuous labels for eval

---

## 8. SFT Mix Design

| Component | Target % | Source | Est. Examples | Est. Tokens |
|-----------|---------|--------|--------------|-------------|
| Cross-sectional Pump decisions (normal compressed L3) | 35% | Slinky+LaserStream panels | 1,000 | 5.0M |
| Cross-sectional Pump decisions (execution-surface detail) | 5% | LaserStream L3 full 5×5 | 100-150 | 0.9M |
| Postmortem/contrast/policy critique | 20% | Slinky+LaserStream pairs + L4 | 600 | 2.7M |
| Rust/Solana repo engineering | 20% | rust_gold_v1 train split | 600 | 4.2M |
| Narrative/meta strategy | 10% | narrative v1.1 (A+B scope) | 300 | 1.2M |
| Hermes/tool interface | 5% | constructed from tool schemas | 150 | 0.5M |
| General retention | 5% | approved trusted corpus (TBD) | 150 | 0.5M |
| **Total SFT** | 100% | — | **~2,900-3,000** | **~15.0M** |

### CPT Mix Design

| Component | Source | Curated Records | Avg Tok/Rec | Total Tokens |
|-----------|--------|----------------|-------------|-------------|
| Per-mint trajectory summaries (cap 3/mint) | Slinky pump_state+outcome | 3,000 | 1,500 | 4.5M |
| Cross-sectional panel snapshots | Slinky pump_state_v3 | 1,200 | 3,000 | 3.6M |
| Regime pattern catalogs | Slinky (rug/runner/mayhem/zombie) | 500 | 4,000 | 2.0M |
| Creator behavior profiles (cap 3/creator) | Slinky | 300 | 3,000 | 0.9M |
| Curve dynamics & graduation mechanics | Slinky | 200 | 5,000 | 1.0M |
| Outcome distribution tables by regime/size | Slinky pump_outcome | 400 | 2,000 | 0.8M |
| Counterfactual economics by class | Slinky counterfactual | 300 | 1,500 | 0.5M |
| Per-mint microstructure (cap 2/mint) | LaserStream L1/L2 | 3,000 | 2,000 | 6.0M |
| L3 execution surface descriptions | LaserStream L3 | 1,000 | 1,500 | 1.5M |
| L4 champion policy decision logs | LaserStream L4 | 500 | 2,000 | 1.0M |
| L2 outcome profile descriptions | LaserStream L2 | 1,000 | 1,000 | 1.0M |
| Code blocks + diffs | rust_gold_v1 (541 changes) | 541 | 8,000 | 4.3M |
| Architecture descriptions | rust_gold_v1 (163 trajectories) | 163 | 5,000 | 0.8M |
| Repair pattern catalogs | rust_gold_v1 (534 repairs) | 534 | 4,000 | 2.1M |
| Repo state descriptions | rust_gold_v1 (21 states) | 21 | 6,000 | 0.1M |
| Trajectory stage text | narrative v1.1 (490 trajectories) | 490 | 2,500 | 1.2M |
| Creator prose (A+B scope, raw) | narrative v1.1 (847 records) | 847 | 1,800 | 1.5M |
| Strategy card frameworks | narrative v1.1 (97 cards) | 97 | 2,000 | 0.2M |
| Wallet/dev/bundle heuristics | narrative v1.1 | 200 | 2,500 | 0.5M |
| Claims layer text (A+B only) | narrative v1.1 | 847 | 800 | 0.7M |
| **Total CPT** | — | **~15,052** | — | **~34.3M** |

**Expandability to ~100M:** Increase per-mint caps (3→5), add more panel snapshots, add outcome distribution tables per day, add more LaserStream microstructure. Schema unchanged.

---

## 9. Export Schemas (Training-Method-Neutral)

### 9A. qwen_cpt_v1 — JSONL (packed text, no instruction format)

```json
{
  "id": "cpt_0001",
  "format": "packed_text",
  "source_corpus": "slinky_gold_v3 | laserstream_gold_v3 | rust_gold_v1 | narrative_gold_v1.1",
  "source_layer": "pump_state_v3 | pump_outcome_v3 | counterfactual_trade_v3 | l1 | l2 | l3 | l4 | code_changes | repairs | trajectories | content | claims",
  "source_ids": ["state_id_X", "state_id_Y"],
  "content": "<natural language market mechanics / code / strategy text>",
  "provenance": {
    "freeze_uuid": "...",
    "source_hash": "...",
    "timestamp_range": ["start_ms", "end_ms"]
  },
  "token_count": 1234,
  "mint_ids_included": ["hashed_mint_A"],
  "scope_tier": "A | B | A+B",
  "narrative_temporal_class": "EX_ANTE | RETROSPECTIVE | AMBIGUOUS"
}
```

### 9B. qwen_sft_v1 — JSONL (instruction-following format)

```json
{
  "id": "sft_0001",
  "format": "instruction",
  "source_corpus": "slinky_gold_v3 | laserstream_gold_v3 | rust_gold_v1 | narrative_gold_v1.1",
  "source_ids": ["state_id_X", "outcome_id_X", "cf_state_id_X"],
  "instruction": "You are a Pump.fun trading decision brain. Given this cross-sectional panel of live candidates with causal state only, rank them by robust executable utility and assign BUY/WATCH/SKIP. If no candidate has robust positive evidence, output NO-BUY.",
  "input": {
    "panel_timestamp": 1787549744081,
    "panel_source": "slinky | laserstream",
    "panel_size": 35,
    "candidates": [
      {
        "mint_id": "hashed_mint_X",
        "causal_state": { "...canonical causal fields..." },
        "source_specific": { "...optional source fields..." }
      }
    ]
  },
  "output": {
    "continuous_rankings": [
      {
        "mint_id": "hashed_mint_X",
        "rank": 1,
        "robust_executable_utility": 0.082,
        "feasibility_ratio": 0.80,
        "max_feasible_size_sol": 0.50,
        "latency_breakpoint_ms": 1000,
        "decision": "BUY",
        "rationale": "Strong organic breadth (45 unique wallets, low concentration), positive net-SOL economics across 20/25 scenarios, feasible through 1000ms at 0.25 SOL. Downside bounded (MAE -200bp), upside asymmetric (MFE +300bp)."
      },
      {
        "mint_id": "hashed_mint_Y",
        "rank": 5,
        "robust_executable_utility": -0.015,
        "feasibility_ratio": 0.40,
        "max_feasible_size_sol": 0.05,
        "latency_breakpoint_ms": 100,
        "decision": "WATCH",
        "rationale": "Decent velocity but narrow trader breadth (8 wallets, top1=35%). Only feasible at 0.05 SOL through 100ms. Capacity-constrained above 0.10 SOL. Mixed execution signals."
      }
    ],
    "no_buy_flag": false,
    "skip_reasons": {
      "hashed_mint_Z": "Collapsed -50% within 300s pattern. Feasible_ratio=0.0. Toxic flow indicator=0.15."
    }
  },
  "provenance": {
    "freeze_uuid": "...",
    "source_hash": "...",
    "panel_id": "panel_001",
    "eval_split": "train | val | eval"
  },
  "token_count": 4500,
  "contrast_pair_id": null,
  "scope_tier": "A+B",
  "l3_detail_level": "compressed | full"
}
```

### 9C. qwen_eval_v1 — JSONL (eval format)

```json
{
  "id": "eval_0001",
  "format": "eval_panel",
  "source_corpus": "slinky_gold_v3 | laserstream_gold_v3",
  "eval_stratum": "slinky | laserstream",
  "panel_timestamp": 1787567740000,
  "panel_size": 35,
  "candidates": [
    {
      "mint_id": "eval_mint_X",
      "causal_state": { "...canonical causal fields..." },
      "source_specific": { "...optional..." }
    }
  ],
  "ground_truth": {
    "continuous_utilities": [
      {
        "mint_id": "eval_mint_X",
        "robust_executable_utility": 0.082,
        "feasibility_ratio": 0.80,
        "median_net_return_pct": 0.015,
        "l2_return_5s": 0.05,
        "l2_return_300s": 0.12,
        "latency_sensitivity": 0.04,
        "size_sensitivity": 0.06,
        "mfe_bp": 300,
        "mae_bp": -200,
        "collapsed_50pct_within_300s": false,
        "panel_rank": 1
      }
    ],
    "derived_labels": {
      "hashed_mint_X": "BUY",
      "hashed_mint_Y": "WATCH",
      "hashed_mint_Z": "SKIP"
    }
  },
  "eval_category": "no_edge | runner | rug | false_positive | missed_opportunity | similar_state_diff_outcome | capacity_latency | migration | hard_skip",
  "narrative_context": "optional: provided or stripped for ablation",
  "provenance": {
    "freeze_uuid": "...",
    "source_hash": "...",
    "eval_split": "eval",
    "chronological_order": 1,
    "mint_disjoint_from_train": true,
    "panel_spacing_minutes": 30,
    "candidate_overlap_with_adjacent": 0.15
  }
}
```

---

## 10. Sampling Rules

### Mint/Creator/Trajectory-Aware Caps

| Rule | Limit | Purpose |
|------|-------|---------|
| Max panels per mint | 3 | Prevent mint domination |
| Max SFT examples per trajectory | 2 | Prevent trajectory domination |
| Max CPT records per creator | 5 | Prevent creator domination |
| Max examples per Rust subsystem | 40 | Prevent subsystem domination |
| Max panels per day (Slinky) | 20 | Prevent day-domination |
| Min panel spacing (Slinky) | 30min | Reduce autocorrelation |
| Min panel spacing (LaserStream) | 2min | Reduce autocorrelation |
| Max candidate overlap between adjacent panels | 30% | Prevent inflated scores |
| Min panel size | 25 | Cross-sectional validity |
| Max panel size | 50 | Inference tractability |

### Stratification

- Slinky panels stratified by: day (39 days), regime (mayhem/zombie/normal), outcome mix (runner/rug/mixed)
- LaserStream panels stratified by: time segment, venue (pumpfun/pumpswap)
- Rust examples stratified by: subsystem, repair type, commit era
- Narrative examples stratified by: scope tier (A/B), temporal class, creator

### Deduplication

- Near-identical panels (same mints, ±5s timestamp) → keep earliest
- Near-identical Rust repair tasks (same bug pattern, same subsystem) → keep one
- Near-identical narrative trajectories (same creator, same thesis) → keep highest-quality

---

## 11. Leakage Checks (Pre-Export Verification)

### 11A. Market Eval Leakage

| Check | Method | Pass Criteria |
|-------|--------|---------------|
| Mint-disjoint (per stratum) | Set intersection eval mints vs train mints | 0 overlap |
| Chronological | Max train timestamp < min eval timestamp (per stratum) | Strict |
| Panel-disjoint | No panel in both train and eval | 0 overlap |
| No future leakage in input | Input contains NO L2/L3/outcome fields | Verified per schema |
| No ret_1s as input | ret_1s/ret_Ns NEVER in causal_state | Verified |
| No seconds_to_graduation in input | Field excluded from canonical schema | Verified |
| Candidate overlap cap | Adjacent panel overlap < 30% | Verified |
| Trajectory-disjoint | No narrative trajectory spans both train and eval mints | 0 overlap |

### 11B. Rust Eval Leakage

| Check | Method | Pass Criteria |
|-------|--------|---------------|
| Commit SHA disjoint | Eval SHAs not in any training example | 0 overlap |
| Repair chain disjoint | No training example references eval repair chain | 0 overlap |
| Repo state protection | No repo_state at/after earliest eval commit in training | Verified |

### 11C. Cross-Corpus Leakage

| Check | Method | Pass Criteria |
|-------|--------|---------------|
| Mint overlap Slinky↔LaserStream | Set intersection | OK (different periods, separate strata) |
| Narrative mint overlap with eval | Narrative mints vs eval mint sets | Eval mints NOT in narrative training examples |

---

## 12. Hard Contrasts Design

| Contrast Type | Input Similarity | Target Difference | Source |
|---------------|------------------|-------------------|--------|
| Same narrative, different outcome | Same creator thesis | Different mint outcomes | Narrative + Slinky |
| Similar causal state, rug vs runner | Similar pump_state_v3 | Graduated vs collapsed | Slinky pairs |
| Onchain strong + narrative weak | Strong causal state, weak narrative | Outcome vs narrative | Slinky + Narrative |
| Onchain weak + narrative strong | Weak causal state, strong narrative | Outcome vs narrative | Slinky + Narrative |
| Early organic vs crowded copied | Similar breadth metrics | Organic vs amplified | pump_state_v3 trader analysis |
| Champion false-positive | Champion says BUY | Outcome = SKIP | L4 policy eval |
| Champion missed opportunity | Champion says SKIP/WATCH | Outcome = runner | L4 policy eval |
| Same token, different size/latency | Same mint, same timestamp | Different feasibility per scenario | L3 counterfactual |

**Target: 100-150 contrast pairs** embedded in SFT set, with ~30-50 held out for eval.

---

## 13. Narrative Integration Rules

| Rule | Implementation |
|------|----------------|
| Narrative = human reasoning, NOT truth | Appears as reasoning examples, never as causal labels or ground truth |
| AMBIGUOUS/RETROSPECTIVE → no causal BUY labels | May appear in strategy/thesis examples, never as decision targets |
| Structured trajectory format | observation → hypothesis → action/skip → rationale → invalidation/revision → outcome |
| 490 trajectories → strategy language | SFT: structured stage format. CPT: raw creator prose (A+B scope only) |
| 97 strategy cards → thesis templates | Used as reasoning frameworks, never as ground truth |
| Creator identity ≠ correctness | Creator name in provenance only, not as input feature |
| generic_crypto (C, 217 records) excluded | From all Pump-specific training |
| No long synthetic chain-of-thought | Concise evidence-grounded rationale; no fabricated reasoning chains |
| pump_specific (A) + solana_memecoin_regime (B) | 847 records training-eligible |

---

## 14. General Retention

- **Source:** Small trusted, contamination-screened general reasoning/coding/instruction retention corpus.
- **NOT** self-generated by Qwen.
- **NOT** trained on eval benchmarks.
- **Source must be approved before final export.**
- Target: ~150 examples, ~0.5M tokens (5% of SFT).

---

## 15. Hermes/Tool Interface SFT (5% bucket)

Constructed examples teaching Qwen to:
- Read causal state from Rust/LaserStream tool output
- Format continuous utility rankings + BUY/WATCH/SKIP in expected schema
- Output NO-BUY when no candidate has robust positive evidence
- Report capacity/latency constraints in standardized format
- Request additional data when causal state is incomplete
- Interface with Hermes supervisor layer for execution

Templated from actual tool schemas, not from training data.

---

## 16. Proposed Example Counts & Token Budgets (REVISED)

### CPT

| Component | Curated Records | Total Tokens |
|-----------|----------------|-------------|
| Slinky (all layers) | ~5,900 | 13.2M |
| LaserStream (all layers) | ~5,500 | 9.5M |
| Rust (all splits-train only) | ~1,259 | 7.4M |
| Narrative v1.1 (A+B scope) | ~2,058 | 4.1M |
| **Total CPT** | **~15,052** | **~34.3M** |
| Target range | — | 25-50M ✓ |
| Expandable to | — | ~100M (no schema change) |

### SFT

| Component | Examples | Total Tokens |
|-----------|---------|-------------|
| Cross-sectional decisions (compressed L3, 35%) | 1,000 | 5.0M |
| Execution-surface detail (full L3, 5%) | 100-150 | 0.9M |
| Postmortem/contrast/policy critique (20%) | 600 | 2.7M |
| Rust/Solana repo engineering (20%) | 600 | 4.2M |
| Narrative/meta strategy (10%) | 300 | 1.2M |
| Hermes/tool interface (5%) | 150 | 0.5M |
| General retention (5%, source TBD) | 150 | 0.5M |
| **Total SFT** | **~2,900-3,000** | **~15.0M** |
| Target range | — | 15-30M ✓ |

### Eval

| Component | Count | Notes |
|-----------|-------|-------|
| Slinky market eval panels | 200-300 | 30min spacing, mint-disjoint |
| LaserStream market eval panels | 25-40 | 2min spacing, mint-disjoint |
| Rust repair eval | 29 | From existing test split |
| Contrast pair eval | 30-50 | Held out from SFT |
| **Total eval** | **~284-419** | Immutable, 2 strata |

---

## 17. Training Execution Plan (after export approval)

### No smoke-test stage. No Hermes/LLM required during training.

The real Qwen 27B full-parameter training run's early steps ARE the feasibility validation. The training run is **self-contained** — requires NO Hermes/LLM calls.

### Pre-launch handoff package (created by Hermes before shutdown)

Hermes creates a complete, self-contained training handoff directory containing:
- `train.py` — full-FT training entrypoint (HuggingFace Trainer + Accelerate + DeepSpeed)
- `accelerate_config.yaml` — 3-GPU ZeRO-3 config
- `deepspeed_config.json` — ZeRO-3 BF16 config (stage 3, all-gather, gradient checkpointing)
- `training_args.json` — epochs, LR, batch, gradient accumulation, checkpoint cadence
- `requirements.txt` — exact package versions
- `launch.sh` — one-command launch script
- `resume.sh` — one-command checkpoint resume script
- `monitor.sh` — GPU/RAM monitoring script (nvidia-smi loop)
- `gpu_check.sh` — verify all 3 GPUs are free before launch
- `RUNBOOK.md` — failure/OOM/distributed recovery procedures
- Expected output/checkpoint locations documented

### Final handoff sequence

1. Hermes finishes + certifies exports/config/scripts
2. User reviews the handoff package
3. Stop Hermes
4. Stop GLM/llama.cpp and verify VRAM released (gpu_check.sh)
5. User manually launches the REAL Accelerate + DeepSpeed ZeRO-3 full-FT run from shell
6. Training/logging/checkpointing continues without Hermes

**Hermes does NOT launch training while GLM/Hermes is active.**

### Early-step feasibility validation (first ~50-100 steps of the real run)
- Verify ZeRO-3 is actually sharding parameters/gradients/optimizer states across 3 GPUs
- Monitor per-GPU VRAM + system RAM usage
- Verify finite loss and non-zero gradients
- Log throughput (tokens/sec, steps/min) and errors
- Checkpoint aggressively early (every 10-20 steps)
- Verify checkpoint resume works (stop, resume from checkpoint, confirm loss matches)

### If configuration/OOM/distributed issues occur
- Fix the issue (ZeRO config, batch size, gradient accumulation, offloading)
- Resume/relaunch the ACTUAL training run from checkpoint
- Do NOT create a separate disposable benchmark/smoke dataset or run

### Training progression
1. Domain CPT (continued pre-training on ~34M curated tokens)
2. SFT (instruction-following on ~15M tokens)
3. Post-training evaluation + shadow testing (BEFORE live promotion)

**Fallback:** If ZeRO-3 full-FT proves infeasible after genuine debugging, fall back to BF16 LoRA on single 96GB RTX PRO 6000 via Unsloth Studio. Exports are method-neutral — no re-export needed.

---

## 18. Open Questions for Review

1. **General retention source:** Need approved trusted, contamination-screened corpus. Candidate sources?
2. **Slinky counterfactual latency gap:** Slinky counterfactual only has 250ms latency (5 sizes, 1 latency). For Slinky panels, L3 compressed features have feasibility_ratio from 5 scenarios (not 25). Latency_sensitivity = null for Slinky. Is this acceptable, or should Slinky execution-surface examples use LaserStream L3 data for the same mints (cross-mint, not cross-corpus)?
3. **Continuous utility function form:** The exact functional form of robust_executable_utility (weights for feasibility, net economics, downside, size/latency robustness) — should this be a fixed formula or learned? Current proposal: fixed formula for label generation, model learns to predict it.
4. **Panel-relative vs absolute BUY thresholds:** BUY/WATCH/SKIP thresholds are panel-relative (top tier of panel). What defines "top tier" — top 10%? Top 3 candidates? Utility > 0?
5. **Narrative ablation design:** Should narrative context be provided to the model as an optional input field (so we can ablate at eval time) or as a separate prompt template?
6. **CPT expansion path:** First run targets ~34M CPT tokens. If more domain adaptation is needed, expansion to ~100M via increased per-mint caps and panel snapshots. Is the first-run scale sufficient or should we target the higher end (40-50M)?

---

## 19. Build Order (after approval)

1. Create immutable eval freeze (Slinky stratum + LaserStream stratum + Rust repair) with full leakage checks
2. Build canonical schema normalizer (Slinky 103→canonical, LaserStream 48→canonical)
3. Build Slinky panels from pump_state_v3 (causal input) joined to pump_outcome_v3 + counterfactual (targets)
4. Build LaserStream panels from L1 (causal input) joined to L2 + L3 (targets)
5. Compute continuous robust_executable_utility for every candidate
6. Derive panel-relative BUY/WATCH/SKIP labels
7. Compress L3 into deterministic features for normal panels; keep full 5×5 for execution-surface examples
8. Build contrast pairs
9. Build Rust repair SFT/CPT examples (train split only)
10. Build narrative SFT/CPT examples (A+B scope, structured trajectory format)
11. Build Hermes/tool interface examples
12. Source and integrate approved general retention corpus
13. Run full leakage verification
14. Export qwen_cpt_v1, qwen_sft_v1, qwen_eval_v1 with provenance manifests
15. Report final counts, token totals, and provenance manifest
16. Launch real ZeRO-3 full-FT training run (early steps = feasibility validation)

**NO TRAINING CONFIG GENERATED. NO TRAINING FILES EXPORTED UNTIL APPROVED.**
