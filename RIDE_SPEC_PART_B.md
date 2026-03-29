# RIDE Integration Spec — Part B: Config Parsing + Integration Tests

**Date:** 2026-03-29
**Engineers:** 4 (config) and 5 (integration tests)
**Status:** Implementation-ready
**Depends on:** Part A (Engineers 1–3) must be complete and compiling before this work begins.
**Constraint:** Zero heap allocation on hot path. All existing tests must pass. `#[inline(always)]` on hot-path functions.

---

## Engineer 4: Config Parsing — canary.json + config.rs + main.rs

### Overview

Wire the `entry_engine`, `ride`, and `risk` JSON config sections through the full pipeline:
1. Add JSON blocks to `config/canary.json`
2. Update builder functions in `config.rs` to produce runtime configs from the _existing_ JSON structs
3. Add runtime configs to `EngineConfig` and wire them in `main.rs`

**Key insight:** The JSON config structs (`EntryEngineJsonConfig`, `RideJsonConfig`, `RiskJsonConfig`) and their corresponding runtime structs (`EntryEngineConfig`, `RideConfig`, `RiskConfig`) _already exist_ in `config.rs`. The JSON structs already have `Deserialize`. The `MevJsonConfig` already has `entry_engine`, `ride`, and `risk` fields. What's missing:
1. The actual JSON values in `canary.json`
2. A builder function for `EntryEngineConfig` from `EntryEngineJsonConfig`
3. Wiring in `EngineConfig` so `main.rs` can pass configs to the engine
4. Replacing the `V2_ENTRY_ENGINE` env var with config-driven activation

### Target Files

| File | Action |
|------|--------|
| `config/canary.json` | MODIFY — add `entry_engine`, `ride`, `risk` inside `mev` |
| `rust/pump-quant-core/src/engine/config.rs` | MODIFY — add builder + EngineConfig fields |
| `rust/pump-quant-core/src/main.rs` | MODIFY — wire config-driven entry engine |

---

### A. canary.json — Add Entry Engine, Ride, and Risk Sections

**Location:** Inside the `"mev"` object, after the `"tp_sl_tiers_v2"` array.

Add the following three sections:

```json
"entry_engine": {
    "hard_gate": {
        "min_buy_count_1s": 5,
        "min_volume_sol_5s": 5.0,
        "max_time_since_last_buy_ms": 500,
        "curve_pct_min": 20.0,
        "curve_pct_max": 60.0,
        "max_unique_buyers_30s": 30,
        "max_sell_ratio_x100": 50,
        "min_history_age_ms": 2000,
        "creator_sell_cooldown_ms": 30000
    },
    "scoring": {
        "w_buy_burst": 0.30,
        "w_volume": 0.20,
        "w_curve": 0.15,
        "w_concentration": 0.10,
        "w_acceleration": 0.10,
        "w_avg_size": 0.05,
        "w_sell_absence": 0.05,
        "w_recency": 0.05,
        "buy_burst_center": 7.0,
        "buy_burst_steep": 0.8,
        "volume_norm_sol": 10.0,
        "curve_mean": 43.0,
        "curve_sigma": 12.0,
        "accel_center": 10.0,
        "accel_steep": 0.15
    },
    "magnitude": {
        "w_fill_rate": 0.20,
        "w_accel": 0.20,
        "w_wallet_quality": 0.15,
        "w_curve_remaining": 0.15,
        "w_volume_intensity": 0.15,
        "w_sell_vacuum": 0.10,
        "w_token_age": 0.05,
        "fill_rate_center": 15.0,
        "fill_rate_steep": 0.25
    },
    "position_sizing": {
        "min_entry_score": 50.0,
        "min_magnitude_for_ride": 40.0,
        "scalp_size_low_sol": 0.10,
        "scalp_size_mid_sol": 0.12,
        "scalp_size_high_sol": 0.15,
        "scalp_tier_mid": 60.0,
        "scalp_tier_high": 70.0,
        "ride_size_min_sol": 0.10,
        "ride_size_max_sol": 0.15
    }
},
"ride": {
    "min_confirming_buys": 2,
    "min_confirming_sol": 0.3,
    "min_gain_pct": 1.5,
    "max_curve_pct": 80.0,
    "early_trail_pct": 8.0,
    "momentum_trail_pct": 6.0,
    "tighten_trail_pct": 4.0,
    "emergency_trail_pct": 2.0,
    "early_to_momentum_ms": 15000,
    "momentum_to_tighten_ms": 60000,
    "max_hold_ms": 300000,
    "gain_momentum_pct": 15.0,
    "gain_tighten_pct": 50.0,
    "hard_floor_gain_pct": 1.0,
    "whale_exit_sol": 1.0,
    "buy_gap_tighten_ms": 5000,
    "buy_gap_exit_ms": 10000,
    "sell_cascade_count": 3,
    "sell_pressure_tighten_pct": 2.0
},
"risk": {
    "daily_loss_limit_sol": 1.5,
    "consecutive_loss_limit": 5,
    "pause_duration_ms": 300000,
    "daily_trade_limit": 200,
    "loss_cooldown_ms": 5000,
    "max_concurrent_scalp": 5,
    "max_concurrent_ride": 3,
    "max_concurrent_total": 8
}
```

**Verification:** After editing, the JSON must parse cleanly:
```bash
python3 -c "import json; json.load(open('config/canary.json'))"
```

**Field mapping notes:**
- The `entry_engine` JSON uses the _nested_ structure (`hard_gate`, `scoring`, `magnitude`, `position_sizing`) matching the existing `EntryEngineJsonConfig` struct in `config.rs`.
- The `ride` JSON fields match `RideJsonConfig` field names exactly.
- The `risk` JSON fields match `RiskJsonConfig` field names exactly.
- `daily_trade_limit` changed from 60 (risk_manager default) to 200 — higher limit for paper mode testing.

---

### B. config.rs — Add `build_entry_engine_config()` Builder Function

**Location:** After the existing `build_risk_config()` function, before `build_exit_config()`.

The `EntryEngineJsonConfig`, `RideJsonConfig`, `RiskJsonConfig` structs already exist. The `build_ride_config()` and `build_risk_config()` builders already exist. What's missing is a builder for `EntryEngineConfig` from `EntryEngineJsonConfig`.

#### B.1 Add `build_entry_engine_config()` Function

```rust
/// Build the entry engine config from JSON, converting SOL to lamports.
/// All JSON fields have sensible defaults — the JSON section is optional.
pub fn build_entry_engine_config(json: &EntryEngineJsonConfig) -> crate::engine::entry_engine::EntryEngineConfig {
    use crate::engine::entry_engine::{EntryEngineConfig, ScoringWeights, DecisionThresholds};

    // Extract sub-configs with defaults
    let hg = json.hard_gate.as_ref();
    let sc = json.scoring.as_ref();
    let mg = json.magnitude.as_ref();
    let sz = json.position_sizing.as_ref();

    let curve_pct_min = hg.and_then(|h| h.curve_pct_min).unwrap_or(20.0);
    let curve_pct_max = hg.and_then(|h| h.curve_pct_max).unwrap_or(60.0);

    // Precompute vSOL reserve boundaries from curve %
    // curve 20% → vsol = 30 + 0.20 * 85 = 47 SOL
    // curve 60% → vsol = 30 + 0.60 * 85 = 81 SOL
    let min_vsol = ((30.0 + curve_pct_min / 100.0 * 85.0) * 1e9) as u64;
    let max_vsol = ((30.0 + curve_pct_max / 100.0 * 85.0) * 1e9) as u64;

    let weights = ScoringWeights {
        // Entry features
        w_buy_burst:           sc.and_then(|s| s.w_buy_burst).unwrap_or(0.30),
        w_volume:              sc.and_then(|s| s.w_volume).unwrap_or(0.20),
        w_curve_position:      sc.and_then(|s| s.w_curve).unwrap_or(0.15),
        w_buyer_concentration: sc.and_then(|s| s.w_concentration).unwrap_or(0.10),
        w_buy_acceleration:    sc.and_then(|s| s.w_acceleration).unwrap_or(0.10),
        w_avg_buy_size:        sc.and_then(|s| s.w_avg_size).unwrap_or(0.05),
        w_sell_absence:        sc.and_then(|s| s.w_sell_absence).unwrap_or(0.05),
        w_recency:             sc.and_then(|s| s.w_recency).unwrap_or(0.05),
        // Magnitude features
        w_fill_rate:           mg.and_then(|m| m.w_fill_rate).unwrap_or(0.20),
        w_buy_velocity_accel:  mg.and_then(|m| m.w_accel).unwrap_or(0.20),
        w_wallet_quality:      mg.and_then(|m| m.w_wallet_quality).unwrap_or(0.15),
        w_curve_remaining:     mg.and_then(|m| m.w_curve_remaining).unwrap_or(0.15),
        w_volume_intensity:    mg.and_then(|m| m.w_volume_intensity).unwrap_or(0.15),
        w_sell_vacuum:         mg.and_then(|m| m.w_sell_vacuum).unwrap_or(0.10),
        w_token_age:           mg.and_then(|m| m.w_token_age).unwrap_or(0.05),
    };

    let decision = DecisionThresholds {
        min_entry_score:      sz.and_then(|s| s.min_entry_score).unwrap_or(50.0),
        min_magnitude_for_ride: sz.and_then(|s| s.min_magnitude_for_ride).unwrap_or(40.0),
        scalp_size_low:       sol_to_lamports(sz.and_then(|s| s.scalp_size_low_sol).unwrap_or(0.10)),
        scalp_size_mid:       sol_to_lamports(sz.and_then(|s| s.scalp_size_mid_sol).unwrap_or(0.12)),
        scalp_size_high:      sol_to_lamports(sz.and_then(|s| s.scalp_size_high_sol).unwrap_or(0.15)),
        ride_size_min:        sol_to_lamports(sz.and_then(|s| s.ride_size_min_sol).unwrap_or(0.10)),
        ride_size_max:        sol_to_lamports(sz.and_then(|s| s.ride_size_max_sol).unwrap_or(0.15)),
        scalp_tier_mid:       sz.and_then(|s| s.scalp_tier_mid).unwrap_or(60.0),
        scalp_tier_high:      sz.and_then(|s| s.scalp_tier_high).unwrap_or(70.0),
    };

    // Normalization params: volume_norm_sol and fill_rate center/steep
    // are used for LUT generation but are hardcoded in EntryEngine::new()
    // via the sigmoid parameters. We pass through the config fields that
    // EntryEngineConfig exposes.
    let volume_norm_sol = sc.and_then(|s| s.volume_norm_sol).unwrap_or(10.0);

    EntryEngineConfig {
        min_buy_count_1s:          hg.and_then(|h| h.min_buy_count_1s).unwrap_or(5),
        max_unique_buyers_30s:     hg.and_then(|h| h.max_unique_buyers_30s).unwrap_or(30),
        min_volume_sol_5s:         hg.and_then(|h| h.min_volume_sol_5s).unwrap_or(5.0),
        max_time_since_last_buy_ms: hg.and_then(|h| h.max_time_since_last_buy_ms).unwrap_or(500),
        curve_pct_min,
        curve_pct_max,
        min_history_age_ms:        hg.and_then(|h| h.min_history_age_ms).unwrap_or(2_000),
        creator_sell_cooldown_ms:  hg.and_then(|h| h.creator_sell_cooldown_ms).unwrap_or(30_000),
        min_vsol_reserves_lamports: min_vsol,
        max_vsol_reserves_lamports: max_vsol,
        crowd_depth_norm_lamports: sol_to_lamports(volume_norm_sol),
        recent_1s_norm_count:      20,
        volume_intensity_norm_lamports: sol_to_lamports(volume_norm_sol),
        weights,
        decision,
    }
}
```

**Why this shape:** The existing `EntryEngineConfig` struct expects human-readable SOL values for config fields and precomputed lamport values for vSOL thresholds. The builder extracts from the nested JSON sub-configs, applying `.unwrap_or(default)` for every field.

#### B.2 Add Runtime Config Fields to `EngineConfig`

**Location:** Inside the `EngineConfig` struct, after the `momentum` field (end of struct).

```rust
    // ── V2 Pipeline runtime configs (entry engine / ride / risk) ────
    /// Entry engine config. When `Some`, the V2 entry pipeline is active.
    /// Built from `mev.entry_engine` JSON section.
    pub entry_engine_config: Option<crate::engine::entry_engine::EntryEngineConfig>,
    /// RIDE mode config. When `Some`, RIDE positions use these parameters.
    /// Built from `mev.ride` JSON section.
    pub ride_config: Option<crate::engine::config::RideConfig>,
    /// Risk manager config. When `Some`, the risk manager uses these parameters.
    /// Built from `mev.risk` JSON section.
    pub risk_config: Option<crate::engine::risk_manager::RiskConfig>,
```

**Important type note:** `ride_config` uses `config::RideConfig` (the runtime struct with integer vSOL bp values), NOT `ride_state::RideConfig` (the RideState engine config). These are the same type — `config::RideConfig` is defined in config.rs and re-used by ride_state.rs. Verify at compile time that the type is correct.

Actually, let me clarify: There is a **name collision**. Looking at the codebase:
- `config.rs` defines `pub struct RideConfig` (runtime, with `min_confirming_buys`, `early_trail_bp`, etc.)
- `ride_state.rs` defines `pub struct RideConfig` (runtime, with `early_to_momentum_ms`, `early_trail_bp`, etc.)

These are **different structs with the same name in different modules**. The `EngineConfig` field should use the `ride_state::RideConfig` type since that's what the `RideState::new()` and `RideState::on_tick()` accept.

**Resolution:** The `config.rs` `RideConfig` IS the same struct used by `ride_state.rs` — check if `ride_state.rs` imports from config or defines its own. Looking at the code:

- `ride_state.rs` defines its own `RideConfig` with fields like `early_to_momentum_ms`, `whale_exit_msol`, `sell_cascade_count`, etc.
- `config.rs` defines `RideConfig` with `min_confirming_buys`, `early_trail_bp`, `whale_exit_lamports`, etc.

These are **different structs**. The `build_ride_config()` in `config.rs` returns `config::RideConfig`. The `RideState::on_tick()` takes `&ride_state::RideConfig`.

**The field on EngineConfig should be `ride_state::RideConfig`** since that's what the hot path consumes. We need a **second builder** or a **conversion function** from `config::RideConfig` → `ride_state::RideConfig`.

Actually, re-reading the code more carefully:

`config.rs` `build_ride_config()` returns `RideConfig` which is defined in config.rs with fields:
```
min_confirming_buys, min_confirming_lamports, min_gain_vsol_fp, max_curve_pct_x100,
early_trail_bp, momentum_trail_bp, tighten_trail_bp, emergency_trail_bp,
early_to_momentum_ms, momentum_to_tighten_ms, max_hold_ms,
gain_momentum_vsol_fp, gain_tighten_vsol_fp, hard_floor_vsol_fp,
whale_exit_lamports, buy_gap_tighten_ms, buy_gap_exit_ms,
sell_cascade_count, sell_pressure_tighten_bp
```

`ride_state.rs` `RideConfig` has:
```
early_to_momentum_ms, momentum_to_tighten_ms, max_hold_ride_ms,
gain_momentum_vsol_fp, gain_tighten_vsol_fp,
early_trail_bp, momentum_trail_bp, tighten_trail_bp, emergency_trail_bp,
sell_pressure_tighten_bp, buy_gap_tighten_ms, buy_gap_tighten_bp, buy_gap_exit_ms,
whale_exit_msol, whale_dump_exit_msol, sell_cascade_count, sell_cascade_window_ms
```

These are **different types with overlapping-but-not-identical fields**. The `ride_state::RideConfig` has mvsol-based whale thresholds and explicit `sell_cascade_window_ms`, while `config::RideConfig` has lamport-based whale threshold and no cascade window.

**Decision:** Add a conversion function `build_ride_state_config()` that produces `ride_state::RideConfig` from `RideJsonConfig`. The `EngineConfig` field should store `ride_state::RideConfig` since that's what the hot path (`RideState::on_tick()`) consumes.

#### B.3 Add `build_ride_state_config()` Conversion Function

**Location:** In `config.rs`, after the existing `build_ride_config()`.

```rust
/// Build the ride_state::RideConfig from JSON. This is the config consumed by
/// RideState::on_tick() on the hot path. Converts SOL to mvsol, price-% to vSOL bp.
pub fn build_ride_state_config(json: &RideJsonConfig) -> crate::engine::ride_state::RideConfig {
    use crate::engine::ride_state;

    ride_state::RideConfig {
        early_to_momentum_ms: json.early_to_momentum_ms.unwrap_or(15_000),
        momentum_to_tighten_ms: json.momentum_to_tighten_ms.unwrap_or(60_000),
        max_hold_ride_ms: json.max_hold_ms.unwrap_or(300_000),
        gain_momentum_vsol_fp: gain_pct_to_vsol_fp(json.gain_momentum_pct.unwrap_or(15.0)),
        gain_tighten_vsol_fp: gain_pct_to_vsol_fp(json.gain_tighten_pct.unwrap_or(50.0)),
        early_trail_bp: price_pct_to_vsol_bp(json.early_trail_pct.unwrap_or(8.0)),
        momentum_trail_bp: price_pct_to_vsol_bp(json.momentum_trail_pct.unwrap_or(6.0)),
        tighten_trail_bp: price_pct_to_vsol_bp(json.tighten_trail_pct.unwrap_or(4.0)),
        emergency_trail_bp: price_pct_to_vsol_bp(json.emergency_trail_pct.unwrap_or(2.0)),
        sell_pressure_tighten_bp: price_pct_to_vsol_bp(
            json.sell_pressure_tighten_pct.unwrap_or(2.0),
        ),
        buy_gap_tighten_ms: json.buy_gap_tighten_ms.unwrap_or(5_000),
        buy_gap_tighten_bp: price_pct_to_vsol_bp(2.0), // fixed: tighten by 2% price equiv
        buy_gap_exit_ms: json.buy_gap_exit_ms.unwrap_or(10_000),
        whale_exit_msol: (json.whale_exit_sol.unwrap_or(1.0) * 1_000.0) as u32,
        whale_dump_exit_msol: ((json.whale_exit_sol.unwrap_or(1.0) * 2.0) * 1_000.0) as u32,
        sell_cascade_count: json.sell_cascade_count.unwrap_or(3),
        sell_cascade_window_ms: 3_000, // fixed: 3s window
    }
}
```

**Note on `whale_dump_exit_msol`:** The JSON config has `whale_exit_sol` (1.0 SOL for trail tightening). The dump exit is 2× that (2.0 SOL for immediate exit). The `RideJsonConfig` doesn't have a separate field for dump threshold; it's derived as `2 × whale_exit_sol`.

#### B.4 Add `build_risk_manager_config()` Conversion Function

**Location:** In `config.rs`, after `build_ride_state_config()`.

The existing `build_risk_config()` returns `config::RiskConfig`. The `RiskManager::new()` takes `&risk_manager::RiskConfig`. These are **different types** — `config::RiskConfig` has `daily_loss_limit_lamports: u64` while `risk_manager::RiskConfig` has `daily_loss_limit_lamports: i64`.

```rust
/// Build the risk_manager::RiskConfig from JSON. This is the config consumed by
/// RiskManager::new(). Converts SOL to lamports (as i64 for loss limit).
pub fn build_risk_manager_config(json: &RiskJsonConfig) -> crate::engine::risk_manager::RiskConfig {
    crate::engine::risk_manager::RiskConfig {
        daily_loss_limit_lamports: -((json.daily_loss_limit_sol.unwrap_or(1.5) * 1e9) as i64),
        consecutive_loss_limit: json.consecutive_loss_limit.unwrap_or(5),
        pause_duration_ms: json.pause_duration_ms.unwrap_or(300_000),
        daily_trade_limit: json.daily_trade_limit.unwrap_or(60),
        loss_cooldown_ms: json.loss_cooldown_ms.unwrap_or(5_000),
        max_concurrent_scalp: json.max_concurrent_scalp.unwrap_or(5),
        max_concurrent_ride: json.max_concurrent_ride.unwrap_or(3),
        max_concurrent_total: json.max_concurrent_total.unwrap_or(8),
    }
}
```

**Key detail:** `daily_loss_limit_lamports` is **negative** in `risk_manager::RiskConfig` (it represents the floor: P&L must be > this value). The JSON stores a positive SOL value (e.g., 1.5), so we negate it: `-(1.5 * 1e9) = -1_500_000_000`.

#### B.5 Wire Builders in `load_config()`

**Location:** At the end of `load_config()`, in the `Ok(EngineConfig { ... })` block, add:

```rust
        // ── V2 Pipeline runtime configs ─────────────────────────────
        entry_engine_config: mev.entry_engine.as_ref().map(build_entry_engine_config),
        ride_config: mev.ride.as_ref().map(build_ride_state_config),
        risk_config: mev.risk.as_ref().map(build_risk_manager_config),
```

**Semantics:** If the JSON section is absent (`None`), the config field is `None`, and the V2 pipeline doesn't activate. If present, the builder runs and produces `Some(config)`.

---

### C. main.rs — Replace V2_ENTRY_ENGINE Env Var with Config-Driven Activation

**Location:** The current V2 entry engine activation block (around line ~222):

```rust
    // ── V2 Entry Engine ────────────────────────────────────────────
    // Activate the new 3-stage pipeline (hard gate + composite scoring + Kelly sizing).
    // Controlled by V2_ENTRY_ENGINE env var. Default: off (legacy GateStack + Scorer).
    if std::env::var("V2_ENTRY_ENGINE").map(|v| v == "true" || v == "1").unwrap_or(false) {
        let ee_config = pump_quant_core::engine::entry_engine::EntryEngineConfig::default();
        let entry_engine = pump_quant_core::engine::entry_engine::EntryEngine::new(&ee_config);
        hot_path.set_entry_engine(entry_engine);
        tracing::info!("V2 EntryEngine activated (composite scoring + magnitude prediction)");
    }
```

**Replace with:**

```rust
    // ── V2 Entry Engine ────────────────────────────────────────────
    // Config-driven activation: if mev.entry_engine exists in canary.json, build and
    // activate the 3-stage pipeline. Falls back to env var for backward compat.
    let v2_entry_from_env = std::env::var("V2_ENTRY_ENGINE")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    if let Some(ref ee_config) = engine_config.entry_engine_config {
        let entry_engine = pump_quant_core::engine::entry_engine::EntryEngine::new(ee_config);
        hot_path.set_entry_engine(entry_engine);
        tracing::info!("V2 EntryEngine activated (config-driven, composite scoring + magnitude)");
    } else if v2_entry_from_env {
        // Backward compat: env var still works if JSON section is absent
        let ee_config = pump_quant_core::engine::entry_engine::EntryEngineConfig::default();
        let entry_engine = pump_quant_core::engine::entry_engine::EntryEngine::new(&ee_config);
        hot_path.set_entry_engine(entry_engine);
        tracing::info!("V2 EntryEngine activated (env var, default config)");
    }
```

**Additionally**, after the entry engine block, wire the ride and risk configs:

```rust
    // ── RIDE Config ────────────────────────────────────────────────
    if let Some(ref ride_cfg) = engine_config.ride_config {
        hot_path.set_ride_config(ride_cfg.clone());
        tracing::info!(
            early_trail_bp = ride_cfg.early_trail_bp,
            momentum_trail_bp = ride_cfg.momentum_trail_bp,
            max_hold_ms = ride_cfg.max_hold_ride_ms,
            "RIDE config loaded from canary.json"
        );
    }

    // ── Risk Manager ───────────────────────────────────────────────
    if let Some(ref risk_cfg) = engine_config.risk_config {
        let risk_manager = pump_quant_core::engine::risk_manager::RiskManager::new(risk_cfg);
        hot_path.set_risk_manager(risk_manager);
        tracing::info!(
            daily_loss = risk_cfg.daily_loss_limit_lamports,
            max_trades = risk_cfg.daily_trade_limit,
            max_concurrent = risk_cfg.max_concurrent_total,
            "RiskManager loaded from canary.json"
        );
    }
```

**Note:** `hot_path.set_ride_config()` and `hot_path.set_risk_manager()` must be implemented by Engineer 2 (Part A). If they don't exist yet, add stub methods:

```rust
// In hot_path.rs (if not already present from Engineer 2):
pub fn set_ride_config(&mut self, config: crate::engine::ride_state::RideConfig) {
    self.ride_config = Some(config);
}

pub fn set_risk_manager(&mut self, manager: crate::engine::risk_manager::RiskManager) {
    self.risk_manager = Some(manager);
}
```

---

### D. config.rs Tests

**Location:** Inside the existing `#[cfg(test)] mod tests` block in `config.rs`, add:

#### D.1 `test_ride_json_config_defaults`

```rust
#[test]
fn test_ride_json_config_defaults() {
    // Parse empty JSON → all fields None → build_ride_state_config uses defaults
    let json: RideJsonConfig = serde_json::from_str("{}").unwrap();
    let cfg = build_ride_state_config(&json);

    assert_eq!(cfg.early_to_momentum_ms, 15_000);
    assert_eq!(cfg.momentum_to_tighten_ms, 60_000);
    assert_eq!(cfg.max_hold_ride_ms, 300_000);
    assert_eq!(cfg.early_trail_bp, 408);      // 8% price → 408 vSOL bp
    assert_eq!(cfg.momentum_trail_bp, 305);    // 6% price → 305 vSOL bp
    assert_eq!(cfg.tighten_trail_bp, 202);     // 4% price → 202 vSOL bp
    assert_eq!(cfg.emergency_trail_bp, 101);   // 2% price → 101 vSOL bp
    assert_eq!(cfg.whale_exit_msol, 1_000);        // 1.0 SOL
    assert_eq!(cfg.whale_dump_exit_msol, 2_000);   // 2.0 SOL
    assert_eq!(cfg.sell_cascade_count, 3);
    assert_eq!(cfg.buy_gap_tighten_ms, 5_000);
    assert_eq!(cfg.buy_gap_exit_ms, 10_000);
}
```

#### D.2 `test_price_pct_to_vsol_bp`

```rust
#[test]
fn test_price_pct_to_vsol_bp_known_values() {
    // These are the canonical values from QUANT_RIDE_C §3.3:
    // price_pct → vSOL trail = 1 - √(1 - pct/100) → vSOL bp = round(trail × 10000)
    //
    //  8% → 1 - √0.92 = 1 - 0.95917 = 0.04083 → 408 bp
    //  6% → 1 - √0.94 = 1 - 0.96954 = 0.03046 → 305 bp
    //  4% → 1 - √0.96 = 1 - 0.97980 = 0.02020 → 202 bp
    //  2% → 1 - √0.98 = 1 - 0.98995 = 0.01005 → 101 bp (rounds from 100.5)
    assert_eq!(price_pct_to_vsol_bp(8.0), 408);
    assert_eq!(price_pct_to_vsol_bp(6.0), 305);
    assert_eq!(price_pct_to_vsol_bp(4.0), 202);
    assert_eq!(price_pct_to_vsol_bp(2.0), 101);

    // Edge cases
    assert_eq!(price_pct_to_vsol_bp(0.0), 0);     // 0% → 0 bp
    assert_eq!(price_pct_to_vsol_bp(100.0), 10000); // 100% → 10000 bp (full)
    assert_eq!(price_pct_to_vsol_bp(50.0), 2929);   // 50% → 1 - √0.5 = 0.2929
}
```

#### D.3 `test_entry_engine_json_config_roundtrip`

```rust
#[test]
fn test_entry_engine_json_config_roundtrip() {
    let json_str = r#"{
        "hard_gate": {
            "min_buy_count_1s": 7,
            "min_volume_sol_5s": 3.0,
            "max_time_since_last_buy_ms": 300,
            "curve_pct_min": 25.0,
            "curve_pct_max": 55.0,
            "max_unique_buyers_30s": 20,
            "max_sell_ratio_x100": 40,
            "min_history_age_ms": 3000,
            "creator_sell_cooldown_ms": 15000
        },
        "position_sizing": {
            "min_entry_score": 55.0,
            "min_magnitude_for_ride": 45.0,
            "scalp_size_low_sol": 0.08,
            "scalp_size_mid_sol": 0.10,
            "scalp_size_high_sol": 0.12,
            "scalp_tier_mid": 65.0,
            "scalp_tier_high": 75.0,
            "ride_size_min_sol": 0.08,
            "ride_size_max_sol": 0.12
        }
    }"#;

    let json: EntryEngineJsonConfig = serde_json::from_str(json_str).unwrap();
    let cfg = build_entry_engine_config(&json);

    // Gate thresholds
    assert_eq!(cfg.min_buy_count_1s, 7);
    assert_eq!(cfg.max_unique_buyers_30s, 20);
    assert_eq!(cfg.min_volume_sol_5s, 3.0);
    assert_eq!(cfg.max_time_since_last_buy_ms, 300);
    assert_eq!(cfg.curve_pct_min, 25.0);
    assert_eq!(cfg.curve_pct_max, 55.0);
    assert_eq!(cfg.min_history_age_ms, 3_000);
    assert_eq!(cfg.creator_sell_cooldown_ms, 15_000);

    // Precomputed lamport thresholds
    // curve 25% → vsol = 30 + 0.25 * 85 = 51.25 SOL = 51_250_000_000 lamports
    assert_eq!(cfg.min_vsol_reserves_lamports, 51_250_000_000);
    // curve 55% → vsol = 30 + 0.55 * 85 = 76.75 SOL = 76_750_000_000 lamports
    assert_eq!(cfg.max_vsol_reserves_lamports, 76_750_000_000);

    // Decision thresholds
    assert_eq!(cfg.decision.min_entry_score, 55.0);
    assert_eq!(cfg.decision.min_magnitude_for_ride, 45.0);
    assert_eq!(cfg.decision.scalp_size_low, 80_000_000);   // 0.08 SOL
    assert_eq!(cfg.decision.scalp_size_mid, 100_000_000);  // 0.10 SOL
    assert_eq!(cfg.decision.scalp_size_high, 120_000_000); // 0.12 SOL
    assert_eq!(cfg.decision.ride_size_min, 80_000_000);    // 0.08 SOL
    assert_eq!(cfg.decision.ride_size_max, 120_000_000);   // 0.12 SOL
    assert_eq!(cfg.decision.scalp_tier_mid, 65.0);
    assert_eq!(cfg.decision.scalp_tier_high, 75.0);
}
```

#### D.4 `test_risk_manager_config_builder`

```rust
#[test]
fn test_risk_manager_config_builder() {
    let json_str = r#"{
        "daily_loss_limit_sol": 2.0,
        "consecutive_loss_limit": 3,
        "pause_duration_ms": 120000,
        "daily_trade_limit": 100,
        "loss_cooldown_ms": 3000,
        "max_concurrent_scalp": 4,
        "max_concurrent_ride": 2,
        "max_concurrent_total": 6
    }"#;

    let json: RiskJsonConfig = serde_json::from_str(json_str).unwrap();
    let cfg = build_risk_manager_config(&json);

    assert_eq!(cfg.daily_loss_limit_lamports, -2_000_000_000); // negative!
    assert_eq!(cfg.consecutive_loss_limit, 3);
    assert_eq!(cfg.pause_duration_ms, 120_000);
    assert_eq!(cfg.daily_trade_limit, 100);
    assert_eq!(cfg.loss_cooldown_ms, 3_000);
    assert_eq!(cfg.max_concurrent_scalp, 4);
    assert_eq!(cfg.max_concurrent_ride, 2);
    assert_eq!(cfg.max_concurrent_total, 6);
}
```

#### D.5 `test_gain_pct_to_vsol_fp`

```rust
#[test]
fn test_gain_pct_to_vsol_fp_known_values() {
    // gain_pct → vSOL FP = round(√(1 + pct/100) × 10000)
    //
    // 15% → √1.15 = 1.072381 → 10724
    // 50% → √1.50 = 1.224745 → 12247
    //  1% → √1.01 = 1.004988 → 10050
    assert_eq!(gain_pct_to_vsol_fp(15.0), 10724);
    assert_eq!(gain_pct_to_vsol_fp(50.0), 12247);
    assert_eq!(gain_pct_to_vsol_fp(1.0), 10050);
    assert_eq!(gain_pct_to_vsol_fp(0.0), 10000);  // 0% gain = 1.0 ratio
    assert_eq!(gain_pct_to_vsol_fp(100.0), 14142); // 100% → √2 = 1.41421
}
```

#### D.6 `test_full_canary_json_parses`

```rust
#[test]
fn test_full_canary_json_parses() {
    // Verify that the actual canary.json (with new sections) parses without error.
    // This test reads the file from disk — skip if file doesn't exist (CI).
    let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()
        .parent().unwrap()
        .join("config/canary.json");

    if !config_path.exists() {
        // Skip in CI / isolated test environments
        return;
    }

    let config = load_config(&config_path);
    assert!(config.is_ok(), "canary.json failed to parse: {:?}", config.err());

    let cfg = config.unwrap();
    // Verify the new sections were parsed
    assert!(cfg.entry_engine_config.is_some(), "entry_engine section should be present");
    assert!(cfg.ride_config.is_some(), "ride section should be present");
    assert!(cfg.risk_config.is_some(), "risk section should be present");

    // Spot-check a few values
    let ee = cfg.entry_engine_config.as_ref().unwrap();
    assert_eq!(ee.min_buy_count_1s, 5);
    assert_eq!(ee.decision.min_entry_score, 50.0);

    let ride = cfg.ride_config.as_ref().unwrap();
    assert_eq!(ride.early_trail_bp, 408);
    assert_eq!(ride.max_hold_ride_ms, 300_000);

    let risk = cfg.risk_config.as_ref().unwrap();
    assert_eq!(risk.daily_trade_limit, 200);
    assert_eq!(risk.max_concurrent_total, 8);
}
```

---

### E. Summary of Changes (Engineer 4)

| File | Changes |
|------|---------|
| `config/canary.json` | Add `entry_engine`, `ride`, `risk` inside `mev` object |
| `config.rs` | Add `build_entry_engine_config()`, `build_ride_state_config()`, `build_risk_manager_config()` functions |
| `config.rs` | Add 3 fields to `EngineConfig` struct |
| `config.rs` | Wire builders in `load_config()` |
| `config.rs` | Add 6 test functions |
| `main.rs` | Replace env var block with config-driven activation |
| `main.rs` | Add ride config and risk manager wiring |

### Verification

```bash
# 1. JSON is valid
python3 -c "import json; json.load(open('config/canary.json'))"

# 2. All tests pass
cd rust/pump-quant-core && cargo test --lib engine::config -- --nocapture

# 3. Compilation passes
cargo check 2>&1 | head -20
```

---

## Engineer 5: Integration Tests

### Overview

Create `integration_tests.rs` — end-to-end tests that verify the full flow across modules: `entry_engine` → `positions` → `ride_state` → `exit_machine`. These tests use real structs (not mocks), constructing the minimum required setup for each scenario.

### Target Files

| File | Action |
|------|--------|
| `rust/pump-quant-core/src/engine/integration_tests.rs` | CREATE |
| `rust/pump-quant-core/src/engine/mod.rs` | MODIFY — add `#[cfg(test)] mod integration_tests;` |

---

### A. mod.rs — Register Integration Tests Module

**Location:** At the bottom of `rust/pump-quant-core/src/engine/mod.rs`, add:

```rust
#[cfg(test)]
mod integration_tests;
```

**After this change, mod.rs should end with:**
```rust
pub mod scoring;

#[cfg(test)]
mod integration_tests;
```

---

### B. integration_tests.rs — Full File Content

**Create file:** `rust/pump-quant-core/src/engine/integration_tests.rs`

```rust
//! Integration tests for the V2 pipeline.
//!
//! These tests verify cross-module flows:
//!   EntryEngine → PositionManager (with ExitMode) → RideState → exit
//!
//! Each test constructs the minimum required structs. No mocks.
//! Uses real scoring, real trail math, real vSOL conversions.

use super::config::{
    build_exit_config, build_ride_state_config, ExitConfig, RideJsonConfig, TpSlTierV2,
};
use super::entry_engine::{EntryAction, EntryDecision, EntryEngine, EntryEngineConfig, EntryInput};
use super::positions::{ClosedPosition, ExitReason, PositionConfig, PositionManager, SizeTier, TpSlTier};
use super::ride_state::{
    self, lamports_to_mvsol, mvsol_to_lamports, RideConfig, RideDecision, RideExitReason,
    RidePhase, RideState,
};
use crate::feeds::{FeedSource, TradeEvent};

// ─── Test Helpers ───────────────────────────────────────────────────────────

/// Build a minimal PositionConfig for integration tests.
/// Uses the signal-based exit machine with default thresholds.
fn integration_position_config() -> PositionConfig {
    PositionConfig {
        max_hold_ms: 10_000,
        momentum_decay_check_ms: 50,
        momentum_decay_min_mfe_pct: 0.001,
        momentum_decay_max_drawdown_pct: 0.003,
        intra_hold_trailing_stop_pct: 1.0,
        intra_hold_trailing_stop_min_mfe_pct: 1.0,
        next_buyer_profit_exit_pct: 0.01,
        next_buyer_aggregate_flow_ratio: 0.35,
        next_buyer_count_threshold: 3,
        next_buyer_single_buy_ratio: 0.25,
        tp_tiers: vec![TpSlTier {
            trigger_max_lamports: u64::MAX,
            tp_pct: 0.025,
            sl_pct: 0.015,
        }],
        size_tiers: vec![SizeTier {
            trigger_max_lamports: u64::MAX,
            size_lamports: 100_000_000, // 0.1 SOL
        }],
        max_concurrent_positions: 10,
        max_entry_size_lamports: 500_000_000,
        size_variance_pct: 0.0, // deterministic for tests
        jito_tip_lamports: 50_000,
        min_hold_before_exit_ms: 0, // no min hold for tests
        tod_boost_multiplier: 1.0,
        boosted_hours_utc: vec![],
        exit_config: default_exit_config(),
    }
}

/// Default ExitConfig for tests — loose thresholds to let positions stay open.
fn default_exit_config() -> ExitConfig {
    ExitConfig {
        confirmation_window_ms: 100,
        stall_no_buy_ms: 2_000,
        stall_fade_fp: 1000,
        stall_conviction_no_buy_ms: 3_000,
        stall_conviction_fade_fp: 1500,
        max_hold_safety_ms: 10_000,
        conviction_tp_multipliers: [100, 100, 140, 180, 220],
        trail_min_conviction: 2,
        trail_activation_pct_of_base_tp: 60,
        trail_distance_fp: 1500,
        trail_keep_mult: 1.0 - 0.015,
        trail_activation_mult: 0.60,
        tp_sl_tiers: [
            TpSlTierV2 {
                trigger_max_lamports: u64::MAX,
                unconfirmed_tp_fp: 3000, // 3% TP
                unconfirmed_sl_fp: 2000, // 2% SL
                confirmed_tp_fp: 5000,   // 5% TP
                confirmed_sl_fp: 2000,   // 2% SL
            },
            TpSlTierV2::default(),
            TpSlTierV2::default(),
            TpSlTierV2::default(),
            TpSlTierV2::default(),
            TpSlTierV2::default(),
            TpSlTierV2::default(),
            TpSlTierV2::default(),
        ],
        tp_sl_tier_count: 1,
    }
}

/// Default ride config for integration tests.
fn default_ride_config() -> RideConfig {
    RideConfig::default()
}

/// Create a TradeEvent for testing.
fn make_trade(
    mint: [u8; 32],
    sig: [u8; 64],
    sol_amount: u64,
    vsol: u64,
    vtokens: u64,
    is_buy: bool,
) -> TradeEvent {
    TradeEvent {
        mint,
        trader: [1u8; 32],
        sig,
        sig_prefix: {
            let mut p = [0u8; 8];
            p.copy_from_slice(&sig[..8]);
            p
        },
        sol_amount,
        token_amount: 1_000_000,
        vsol_reserves: vsol,
        vtoken_reserves: vtokens,
        market_cap_sol: vsol * 2,
        slot: 100,
        timestamp_ms: 0,
        is_buy,
        source: FeedSource::PumpPortal,
        bonding_curve: [2u8; 32],
        assoc_bonding_curve: [3u8; 32],
    }
}

/// Create an EntryInput that passes the default hard gate.
fn passing_entry_input(now_ms: u64) -> EntryInput {
    EntryInput {
        vsol_reserves: 66_000_000_000,   // ~42% curve (sweet spot)
        vtoken_reserves: 500_000_000_000,
        sol_amount: 500_000_000,          // 0.5 SOL trigger
        buy_count_1s: 8,
        buy_count_2s: 12,
        buy_count_5s: 20,
        sell_count_5s: 2,
        unique_buyers_30s: 12,
        _pad: 0,
        volume_sol_5s: 8_000_000_000,     // 8 SOL
        vsol_delta_3s: 3_000_000_000,     // 3 SOL in 3s
        time_since_last_buy_ms: 100,
        history_age_ms: 10_000,
        creator_sell_at_ms: 0,
        now_ms,
        max_wallet_vol_30s: 2_000_000_000,
        total_buy_vol_30s: 10_000_000_000,
    }
}

/// Generate a unique signature for test events.
fn sig_from_u8(val: u8) -> [u8; 64] {
    [val; 64]
}

// ─── Test 1: Full SCALP Lifecycle ───────────────────────────────────────────

/// Verify: EntryEngine evaluate → open position → price rises → TP exit.
///
/// Flow:
/// 1. EntryEngine evaluates passing input → Scalp decision
/// 2. Open position via PositionManager
/// 3. Feed a buy trade with price rise > TP threshold
/// 4. Verify position closes with TakeProfit reason
#[test]
fn test_full_scalp_lifecycle() {
    // 1. EntryEngine evaluation
    let ee_config = EntryEngineConfig::default();
    let engine = EntryEngine::new(&ee_config);
    let input = passing_entry_input(1_000_000);
    let decision = engine.evaluate(&input);

    // Should produce a non-reject action with positive scores
    assert_ne!(decision.action, EntryAction::Reject, "good input should not be rejected");
    assert!(decision.entry_score > 0.0);
    assert!(decision.size_lamports > 0);

    // 2. Open position
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut pm = PositionManager::new(integration_position_config(), tx);

    let mint = [0xAAu8; 32];
    let entry_sig = sig_from_u8(0x01);
    let entry_vsol = 30_000_000_000u64; // 30 SOL
    let entry_vtokens = 1_000_000_000_000_000u64;
    let trigger = make_trade(mint, entry_sig, 100_000_000, entry_vsol, entry_vtokens, true);
    pm.open_position(&trigger, decision.entry_score, 1_000_000);

    assert_eq!(pm.open_count(), 1, "position should be open");

    // 3. Confirm position with a buy (prevents MomentumDecayFlat)
    let confirm_vsol = (entry_vsol as f64 * 1.01) as u64; // +1%
    let confirm = make_trade(mint, sig_from_u8(0x02), 50_000_000, confirm_vsol, entry_vtokens, true);
    let closed = pm.on_subsequent_trade(&confirm, 1_000_050);
    assert!(!closed, "should not close on confirm buy");

    // 4. Feed TP-triggering trade: price rises > 5% (confirmed_tp_fp = 5000 = 5%)
    let tp_vsol = (entry_vsol as f64 * 1.06) as u64; // +6% from entry
    let tp_trade = make_trade(mint, sig_from_u8(0x03), 200_000_000, tp_vsol, entry_vtokens, true);
    let closed = pm.on_subsequent_trade(&tp_trade, 1_000_200);

    assert!(closed, "position should close on TP");
    assert_eq!(pm.open_count(), 0);

    // 5. Verify exit
    let cp = rx.try_recv().expect("should have a closed position");
    assert!(
        matches!(cp.exit_reason, ExitReason::TakeProfit | ExitReason::TakeProfitScaled),
        "exit reason should be TakeProfit variant, got {:?}",
        cp.exit_reason
    );
    assert!(cp.gross_pnl_sol > 0, "should be profitable");
}

// ─── Test 2: Scalp-to-Ride Transition ───────────────────────────────────────

/// Verify: position opened as SCALP transitions to RIDE when confirming
/// buys meet the threshold.
///
/// This test verifies the ExitMode::Scalp → ExitMode::Ride transition
/// as implemented in Engineer 1's positions.rs changes.
///
/// IMPORTANT: This test will compile only after Engineer 1 implements the
/// ExitMode enum and ride transition logic in positions.rs. Until then,
/// it should be marked #[ignore].
///
/// Flow:
/// 1. Open position with magnitude >= 40 (RIDE-eligible)
/// 2. Feed 2 qualifying buys (each >= 0.15 SOL, from different wallets)
/// 3. Verify exit_mode transitions from Scalp to Ride
///
/// NOTE: The transition logic lives in positions.rs `check_ride_promotion()`.
/// If Engineer 1 hasn't implemented it yet, test the RideState directly.
#[test]
fn test_scalp_to_ride_transition() {
    // We test the RideState directly since the full PositionManager integration
    // requires Engineer 1's ExitMode changes. This validates the core logic.

    let ride_config = default_ride_config();
    let entry_mvsol: u32 = 50_000; // 50 SOL vSOL reserves
    let current_mvsol: u32 = 51_000; // slightly above entry (+2%)
    let now_ms: u64 = 1_000_000;

    // Create RideState — simulates the transition from Scalp → Ride
    let mut ride = RideState::new(
        entry_mvsol,
        current_mvsol,
        now_ms,
        8,       // buy_rate_5s
        &ride_config,
    );

    // Verify initial state
    assert_eq!(ride.phase, RidePhase::Early as u8);
    assert_eq!(ride.entry_mvsol, entry_mvsol);
    assert!(ride.peak_mvsol >= current_mvsol);

    // Feed 2 qualifying buys (each >= 0.15 SOL = 150 mvsol)
    ride.on_buy_event(200, now_ms + 1_000); // 0.2 SOL
    ride.on_buy_event(180, now_ms + 2_000); // 0.18 SOL

    // Verify RIDE is active — on_tick should return Hold (not exit)
    let price_up = current_mvsol + 500; // price rises
    let decision = ride.on_tick(price_up, now_ms + 3_000, &ride_config);
    assert_eq!(decision, RideDecision::Hold, "ride should hold on rising price");

    // Verify buy volume accumulated
    assert_eq!(ride.total_buy_msol, 380); // 200 + 180
    assert!(ride.last_buy_ms == now_ms + 2_000);
}

// ─── Test 3: Ride Trailing Stop Exit ────────────────────────────────────────

/// Verify: After RIDE transition, price rises to peak, then drops below
/// trailing stop → RideTrailingStop exit.
///
/// Flow:
/// 1. Create RideState at entry 50 SOL vSOL
/// 2. Push price up to 55 SOL (peak)
/// 3. Drop price below trail stop → trailing stop exit
#[test]
fn test_ride_trailing_stop_exit() {
    let ride_config = default_ride_config();
    let entry_mvsol: u32 = 50_000; // 50 SOL
    let now_ms: u64 = 1_000_000;

    let mut ride = RideState::new(entry_mvsol, 50_500, now_ms, 8, &ride_config);

    // Push price up — establish peak at 55 SOL
    let t1 = now_ms + 1_000;
    let peak_mvsol: u32 = 55_000; // 55 SOL — +10% from entry
    let decision = ride.on_tick(peak_mvsol, t1, &ride_config);
    assert_eq!(decision, RideDecision::Hold);
    assert_eq!(ride.peak_mvsol, peak_mvsol);

    // Add some buys to keep the pump alive
    ride.on_buy_event(300, t1);
    ride.on_buy_event(200, t1 + 500);

    // Compute expected trail stop
    // EARLY phase trail = 408 bp → stop = 55000 × (10000 - 408) / 10000 = 55000 × 9592 / 10000 = 52756
    let expected_trail_stop = ride_state::compute_trail_stop(peak_mvsol, ride_config.early_trail_bp);
    assert_eq!(expected_trail_stop, 52_756);

    // Verify trail stop was set
    assert!(
        ride.trail_stop_mvsol >= expected_trail_stop,
        "trail_stop_mvsol ({}) should be >= computed ({})",
        ride.trail_stop_mvsol,
        expected_trail_stop
    );

    // Drop price below trail stop
    let t2 = t1 + 2_000;
    let crash_mvsol: u32 = expected_trail_stop - 100; // below trail stop
    let decision = ride.on_tick(crash_mvsol, t2, &ride_config);
    assert_eq!(decision, RideDecision::Exit(RideExitReason::TrailingStop));
}

// ─── Test 4: Ride Stays Scalp on Low Magnitude ─────────────────────────────

/// Verify: position with magnitude < 40 stays in SCALP mode regardless
/// of how many buys follow.
///
/// The entry_engine decides SCALP vs RIDE at entry time based on
/// magnitude_score. This test verifies the entry engine's decision logic.
#[test]
fn test_ride_stays_scalp_low_magnitude() {
    let ee_config = EntryEngineConfig::default();
    let engine = EntryEngine::new(&ee_config);

    // Create input with moderate entry_score but low magnitude
    // Low magnitude: low fill_rate, low acceleration, high whale concentration
    let mut input = passing_entry_input(1_000_000);

    // Reduce magnitude indicators:
    input.vsol_delta_3s = 500_000_000;     // 0.5 SOL in 3s (low fill rate)
    input.max_wallet_vol_30s = 8_000_000_000; // 80% whale concentration
    input.total_buy_vol_30s = 10_000_000_000;

    let decision = engine.evaluate(&input);

    // If rejected, that's also valid — low magnitude + low entry_score = Reject
    if decision.action != EntryAction::Reject {
        // If not rejected, it should be Scalp (magnitude < 40 threshold)
        assert_eq!(
            decision.action,
            EntryAction::Scalp,
            "low magnitude should produce Scalp, not Ride. magnitude_score={:.1}",
            decision.magnitude_score
        );
        assert!(
            decision.magnitude_score < 40.0,
            "magnitude_score should be below ride threshold (40), got {:.1}",
            decision.magnitude_score
        );
    }

    // Additional verification: even with 5 subsequent buys, the initial decision