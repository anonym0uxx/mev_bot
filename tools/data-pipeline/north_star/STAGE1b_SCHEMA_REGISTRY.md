# North Star — Stage 1b: Schema Registry & Held-out Freeze

Status: **schema inventory + freeze strategy complete.** The exact frozen ID list is
produced by the exporter (Stage 5), not hand-written here.

---

## 1. Layer schema registry (verified from artifact store)

### LaserStream gold (`gold/laserstream_gold_v3/`)
| Layer | Cols | Purpose | Key fields |
|---|---|---|---|
| `l1_pump_state_v3` | 48 | raw on-chain state snapshots | `state_id`, `mint_b58`, `slot`, `timestamp_ms`, `venue`, `event_type`, `trade_side`, `price_lamports_per_rawtok`, `sol_traded_lamports`, `tokens_traded_raw`, `trader_b58`, curve/pool reserves, `ix_fee_bps`, `right_censored_*`, `observed_no_trade_*` |
| `l2_pump_outcome_v3` | 52 | outcome features | `state_id`, `entry_executable`, `entry_failure_reason`, `return_{1s..300s}`, `mfe_pct`/`mae_pct`, `barrier_first_label`, `peak_return_pct`, `return_gt_{10x,100x,1000x}`, `migration_outcome`, `price_quality` |
| `l3_counterfactual_v3` | 21 | **BUY-label source** | `scenario_id`, `state_id`, `trade_size_sol`/`lamports`, `latency_scenario_ms`, `fee_model`, `execution_model_version`, `tokens_bought_raw`, `entry_price`, `feasible`, `feasibility_reason`, `exit_executable`, `exit_sol_received_lamports`, `sim_return_pct`, `outcome_class`, `capacity_constrained`, `confidence` |
| `l4_policy_eval_v3` | 16 | champion vs oracle ranking | `state_id`, `champion_v1_{config,size,latency,outcome,return,feasible}`, `oracle_best_{scenario,size,return,outcome}`, `champion_v1_policy_*` |

### Slinky gold (`gold/slinky_gold_v3/`)
`disk_manifest_v3.json` → `layers`, `cross_layer_cardinality`, `per_layer_match`,
`expected_per_layer`. Layer files under the manifest's `base_path`.

### Narrative gold (`narrative_gold_v1.1/gold/`)
- `creator_content_v1` — source text + `usefulness_class`, `quality_label`, `identity_confidence`, `wallet_confidence`
- `creator_claim_v1` — `claim_type`, `direction`, `sizing`, `targets`, `narrative_themes`, `failure_modes`, `admission_status`, `temporal_class`
- `human_reasoning_trajectory_v1` — `trajectory_id`, `stages_found`, `stage_sequence`, `temporal_class`, `admission_status`
- (also `narrative_state_v1`, `narrative_validation_v1`, `strategy_card_v1`)

## 2. Identity & temporal keys (freeze/leakage basis)

- **Identity (freeze unit):** `mint_b58` (L1) ≡ `primary_mint` (narrative). Freeze is
  per-mint; a mint in eval is never in train/val.
- **Temporal (causal cutoff):** `timestamp_ms` + `slot` (on-chain); `publish_time_ms`
  vs `first_seen_ms` (narrative). `first_seen_ms` is ex-post observation time, NOT a
  causal input — it marks when *we* saw it, and is excluded from decision inputs.

## 3. Held-out freeze strategy (binding)

1. Freeze unit = **mint address** (hash-pinned list `heldout_mints.sha256`), not rows.
2. Partition: sort eligible mints by first-seen time; reserve a contiguous trailing
   time band as eval (no interleaving with train mints in the same narrative theme).
3. Eval mirrors the class balance target (V6), so BUY quality is measurable.
4. Frozen ID list is generated once and stored with a SHA256; later stages may only
   read it, never regenerate from a downstream-derived signal.
5. The prior `qwen_eval_v1.2` is regression-reference only; North Star eval is its own
   frozen partition.

## 4. Leakage field classification (causal vs label)

**Causal inputs (available strictly before decision time `t`):**
- L1 state: reserves, price, recent trade flow, `observed_no_trade_*` (activity only up
  to `t`), fee bps.
- L3 scenario geometry: `trade_size`, `latency_scenario_ms`, `fee_model` (pre-t).

**Labels / outcomes (never inputs):**
- L2 `return_*`, `mfe_pct`/`mae_pct`, `return_gt_*`, `migration_outcome`.
- L3 `sim_return_pct`, `exit_sol_received_lamports`, `outcome_class`, `feasible` +
  `capacity_constrained` (these are the BUY-label derivation, used only to assign the
  supervised action, not as input features).

**BUY label (derived, on-chain economics — no human history):**
`feasible == true` AND `sim_return_pct` above the cost/latency-adjusted threshold AND
`capacity_constrained == false` → BUY. WATCH = no qualifying signal; SKIP/SELL = exit /
cut / no-trade. This is the source that must reach ≥2–5% balanced share per class (V6).

## 5. Next (Stage 2)

Market/narrative capture toward closing the D11 (decisions), D04 (capacity), D06
(holders), D08/D09 (propagation/meta) gaps, feeding the balanced episode builder.
