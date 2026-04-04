# ARCHITECT_PLAN.md — pump-quant v6 Build Plan

Generated from quant analysis + full codebase review. Six parallel build tasks.
Each task owns distinct files — zero merge conflicts between tasks.

---

## System Overview

```
Current flow:
  PumpPortal/Helius → corecast.rs → hot_path.rs → gates.rs → scorer.rs → positions.rs

Target flow (all phases):
  ShredStream(primary) + PumpPortal(fallback) + Helius(prewarm)
    → corecast.rs (ShredStream-first routing)
    → hot_path.rs (new curve_pct gate + dynamic fee filter)
    → gates.rs (raised thresholds: score≥0.65, buyers≥4, curve<80%, no 3-5 UTC)
    → scorer.rs (velocity_acceleration + sell_exhaustion features)
    → positions.rs (unchanged hot path, new conviction-based sizing)
    → tx/executor.rs (dynamic Jito tip scaling)

Graduation arb: DISABLED. arb/ module kept for graduation detection only.
New: GradDexBackrun engine in arb/grad_dex_backrun.rs (post-graduation DEX backrun)
```

---

## TASK A — Entry Gate Surgery (gates.rs + config.rs + canary.json)
**Owner files**: `engine/gates.rs`, `engine/config.rs`, `config/canary.json`
**Expected impact**: +2.84 SOL (eliminate -78% of max_hold exits)
**Zero risk to hot path**: all changes are config-driven thresholds, no logic restructure

### Changes to `engine/gates.rs` → `GateConfig`:

Add two new fields:
```rust
/// Maximum bonding curve progress before rejecting (0.0–1.0). 
/// Rejects late-curve entries — strong predictor of max_hold exits.
/// Default: 1.0 (disabled). Recommended: 0.80.
pub max_curve_progress: f64,

/// Minimum score threshold (duplicates trigger_min_score but allows
/// independent tuning per analysis). Kept separate for clarity.
/// Default: 0.35. Recommended: 0.65.
// NOTE: trigger_min_score already exists — just raise its default.
```

Add `MaxCurveProgress` variant to `GateRejectReason`:
```rust
/// Token too far along bonding curve (> max_curve_progress)
MaxCurveProgress,
```

Add Gate 3b in `GateStack::evaluate()` after the existing vSol range gate (Gate 3):
```rust
// ── Gate 3b: Curve progress cap ────────────────────────────────
// curvePct field = (vsol_reserves - 30e9) / 55e9 approximation.
// Rejects late-curve entries that statistically exit at max_hold.
if self.config.max_curve_progress < 1.0 && event.vsol_reserves > 0 {
    let progress = crate::engine::regime::compute_bonding_curve_progress(
        event.vtoken_reserves,
        crate::engine::regime::INITIAL_VIRTUAL_TOKENS,
    );
    if progress > self.config.max_curve_progress {
        return Err(GateRejectReason::MaxCurveProgress);
    }
}
```

Update `GateRejectReason` display match arm:
```rust
Self::MaxCurveProgress => write!(f, "MaxCurveProgress"),
```

Update `gate_reject_index()` to include `MaxCurveProgress` (assign next available index).

### Changes to `engine/config.rs`:

In `MevJsonConfig` add:
```rust
pub max_curve_progress: Option<f64>,
```

In `load_config()` GateConfig builder section add:
```rust
max_curve_progress: mev.max_curve_progress.unwrap_or(1.0),
```

Also update `trigger_min_score` default mapping:
- `trigger_min_score: mev.trigger_min_score.unwrap_or(0.65),`  (was 0.35)
- `min_unique_buyers: mev.min_unique_buyers.unwrap_or(4),`  (was 3)

### Changes to `config/canary.json` → `mev` section:

```json
"trigger_min_score": 0.65,
"min_unique_buyers": 4,
"max_curve_progress": 0.80,
"tod_config": {
  "blocked_hours_utc": [3, 4, 5],
  "boosted_hours_utc": [14, 15, 16, 19, 20, 21]
}
```

### Validation:
- Run daemon for 30 min paper mode, confirm gate_reject_counts shows MaxCurveProgress firings
- Check trade rate drops ~30% (expected from filter tightening)
- Check max_hold exit rate drops significantly in rust-status.js output

---

## TASK B — Dynamic Fee Filter + Conviction Sizing (hot_path.rs + positions.rs)
**Owner files**: `engine/hot_path.rs`, `engine/positions.rs`, `engine/config.rs` (fee fields only)
**Expected impact**: +3.80 SOL (fee optimization — skip marginal trades)
**Constraint**: Must not add any allocation to the hot path. All checks inline.

### New field in `HotPath`:
```rust
/// Minimum expected value in lamports before entering a position.
/// EV estimate = size_lamports * (score - 0.5) * 2 * avg_tp_pct
/// If EV < min_ev_lamports → skip entry.
min_ev_lamports: u64,

/// Fee estimate per trade in lamports (Jito tip + network).
/// Used for EV gate. Updated dynamically from tx stats.
/// Default: 2_100_000 (2.1 mSOL — slightly above observed avg).
estimated_fee_lamports: u64,
```

### New EV gate in `HotPath::on_trade()` after score check (step 7):
```rust
// 7b. EV gate: reject if expected value < fee estimate
// EV = size * score_edge * avg_tp_pct
// score_edge = (score - 0.5) * 2  (maps [0.5,1.0] → [0.0,1.0])
// avg_tp_pct = 0.02 (take_profit target)
// Skip if score < 0.5 (negative edge, should be caught by score gate but be explicit)
if score > 0.5 {
    let score_edge = (score - 0.5) * 2.0;
    let entry_size = self.position_manager.config().entry_size_lamports;
    let ev_estimate = (entry_size as f64 * score_edge * 0.02) as u64;
    if ev_estimate < self.estimated_fee_lamports {
        self.stats.score_rejects += 1;
        return;
    }
}
```

### Dynamic Jito tip scaling in `tx/executor.rs`:
Add `compute_jito_tip(score: f64, size_lamports: u64, base_tip: u64) -> u64`:
```rust
/// Scale Jito tip based on conviction (score) and position size.
/// Low conviction → minimum tip. High conviction → full tip.
/// Formula: tip = base_tip * score_multiplier
///   score_multiplier = 0.5 + (score - 0.5) * 2.0  clamped [0.5, 1.5]
pub fn compute_jito_tip(score: f64, size_lamports: u64, base_tip: u64) -> u64 {
    let mult = (0.5 + (score - 0.5) * 2.0).clamp(0.5, 1.5);
    // Also scale with size: larger positions warrant more tip
    let size_factor = ((size_lamports as f64) / 120_000_000.0).clamp(0.5, 2.0);
    ((base_tip as f64) * mult * size_factor.sqrt()) as u64
}
```

Thread `score` through `PositionManager::open_position()` → `tx/executor.rs` call site.
The score is already stored in `Position` — pass it to the Jito tip computation.

### Conviction-based sizing in `engine/positions.rs`:
In `PositionManager::open_position()`, after ToD multiplier:
```rust
// Conviction multiplier: scale size by score above threshold
// score ∈ [0.65, 1.0] → multiplier ∈ [1.0, 1.4]
// Capped at max_entry_size_lamports
let conviction_mult = if score > 0.75 {
    1.0 + (score - 0.65) * 4.0  // 0.75 → 1.4x, 0.80 → 1.6x (capped)
} else {
    1.0
};
```
Add `max_entry_size_sol` config field (already exists in `MevJsonConfig`, wire it through).

### New config fields in `MevJsonConfig` / `EngineConfig`:
```rust
pub min_ev_lamports: Option<u64>,   // default: 0 (disabled until calibrated)
pub dynamic_jito_tip: Option<bool>, // default: false (enable after testing)
pub conviction_sizing: Option<bool>,// default: false
```

---

## TASK C — Backrun Anti-Flat Filter (momentum/mod.rs + momentum/scorer.rs)
**Owner files**: `momentum/mod.rs`, `momentum/scorer.rs`, `momentum/config.rs`
**Expected impact**: +0.90 SOL (eliminate momentum_decay_flat exits from backrun)

### Analysis:
Backrun momentum_decay_flat exits (100 trades, WR=21%) share characteristics:
- Entry triggered by large isolated buy with no follow-through crowd
- No secondary buy within 500ms of trigger
- Buy/sell ratio < 2.5 in 5s window

### New fields in `MomentumConfig` (momentum/config.rs):
```rust
/// Minimum trigger buyer size in lamports to qualify for backrun entry.
/// Filters isolated small buys that don't attract follow-through.
/// Default: 0 (disabled). Recommended: 250_000_000 (0.25 SOL).
pub min_trigger_buy_lamports: u64,

/// Maximum ms since trigger buy to confirm follow-through crowd.
/// If no secondary buy within this window, skip entry.
/// Default: 0 (disabled). Recommended: 500.
pub max_follow_through_ms: u64,

/// Minimum buy/sell ratio in 5s window to qualify for entry.
/// Rejects entries with elevated sell pressure.
/// Default: 0.0 (disabled). Recommended: 2.5.
pub min_buy_sell_ratio_5s: f64,

/// Minimum buyer impact: trigger buy must have moved vSol by at least this pct.
/// Default: 0.0 (disabled). Recommended: 0.015 (1.5%).
pub min_trigger_impact_pct: f64,
```

### Changes to `momentum/scorer.rs`:
Add `check_entry_quality()` method:
```rust
/// Returns false if the entry fails anti-flat checks.
/// Called before position open in backrun mode.
pub fn check_entry_quality(
    &self,
    trigger_sol: u64,
    buy_count_5s: u16,
    sell_count_5s: u16,
    vsol_delta_3s: u64,
    vsol_reserves: u64,
) -> bool {
    let c = &self.config;
    
    // Filter: trigger buy too small
    if c.min_trigger_buy_lamports > 0 && trigger_sol < c.min_trigger_buy_lamports {
        return false;
    }
    
    // Filter: weak buy pressure
    if c.min_buy_sell_ratio_5s > 0.0 && sell_count_5s > 0 {
        let ratio = buy_count_5s as f64 / sell_count_5s as f64;
        if ratio < c.min_buy_sell_ratio_5s {
            return false;
        }
    }
    
    // Filter: trigger didn't move the market
    if c.min_trigger_impact_pct > 0.0 && vsol_reserves > 0 {
        let impact = vsol_delta_3s as f64 / vsol_reserves as f64;
        if impact < c.min_trigger_impact_pct {
            return false;
        }
    }
    
    true
}
```

Wire `check_entry_quality()` into `momentum/mod.rs` entry decision point.

### Config (canary.json momentum section):
```json
"momentum": {
  "min_trigger_buy_lamports": 250000000,
  "min_buy_sell_ratio_5s": 2.5,
  "min_trigger_impact_pct": 0.015,
  "max_follow_through_ms": 500
}
```

---

## TASK D — ShredStream Activation (feeds/shredstream.rs + feeds/corecast.rs)
**Owner files**: `feeds/shredstream.rs`, `feeds/corecast.rs`, `feeds/event_joiner.rs`
**Expected impact**: +3.20 SOL (sub-slot execution 20-30ms vs 150-200ms)
**Risk**: MEDIUM — infrastructure change. Feature-flag gated.

### Current state:
`feeds/shredstream.rs` exists (264 lines) but is not wired into the event routing.
`feeds/corecast.rs` (749 lines) routes events from PumpPortal + Helius only.

### Architecture:
ShredStream delivers transaction shreds before block finalization.
For our use case: ShredStream can deliver the same trade event 50-150ms before PumpPortal.
This means our entry fires in the SAME SLOT as the triggering buy rather than slot+1.

### Integration plan:

**Step 1**: Add `ShredStreamConfig` to `EngineConfig`:
```rust
pub shredstream_enabled: bool,
pub shredstream_endpoint: String,  // gRPC endpoint
pub shredstream_fallback_ms: u64,  // fall back to PumpPortal if shredstream silent for N ms
```

**Step 2**: In `feeds/corecast.rs`, add ShredStream channel alongside existing feeds:
```rust
// Priority routing: ShredStream events take priority over PumpPortal for same mint+sig
// Dedup by (mint, sig_prefix) within 200ms window to avoid double-firing
struct EventPriority {
    shredstream: tokio::sync::mpsc::Receiver<TradeEvent>,
    pumpportal: tokio::sync::mpsc::Receiver<TradeEvent>,
    seen_sigs: hashbrown::HashMap<u64, u64>, // sig_prefix → timestamp_ms
}
```

**Step 3**: `feeds/event_joiner.rs` dedup logic:
- Already has sig-based dedup concept in helius_sig_ring in hot_path.rs
- Extract into shared `SigDedup` struct used by both corecast and hot_path
- Window: 200ms (ShredStream lead time range)

**Step 4**: Feature flag in main.rs / lib.rs:
```rust
if config.shredstream_enabled {
    feeds::shredstream::spawn_shredstream_feed(config.shredstream_endpoint.clone(), tx.clone())
        .await?;
}
```

**Step 5**: Graduation arb re-enablement gate:
```rust
// Graduation arb is only viable with ShredStream (sub-100ms total latency)
if config.graduation_arb_enabled && !config.shredstream_enabled {
    tracing::warn!("graduation_arb_enabled but shredstream_enabled=false — disabling arb (latency too high)");
    config.graduation_arb_enabled = false;
}
```

### Validation:
- Paper mode: compare entry timestamps (shredstream_ts vs pumpportal_ts) per trade
- Log `helius_lead_sum_ms / helius_lead_count` — ShredStream should show 50-150ms lead
- Run A/B: 30 min ShredStream off, 30 min on, compare fill quality

---

## TASK E — Graduation→DEX Backrun Engine (arb/grad_dex_backrun.rs)
**Owner files**: `arb/grad_dex_backrun.rs` (NEW FILE), `arb/mod.rs`
**Expected impact**: +0.4 SOL/month (10-20 new trades/day, 65% WR)
**Risk**: LOW — completely new engine, no changes to existing hot path

### Concept:
Instead of arbing the BC→DEX price dislocation (which requires sub-100ms we don't have),
we WATCH the DEX for the first 5 slots after graduation and BACKRUN large opening buyers.
Opening buyers on a newly graduated token are often retail / bots who don't have MEV protection.
Large first buys (>0.5 SOL) create immediate price impact we can front/backrun.

### New file: `arb/grad_dex_backrun.rs`

```rust
//! Graduation→DEX Backrun Engine
//!
//! Triggered by migration events (same as graduation arb).
//! Instead of arbing the BC terminal price spread, monitors the DEX
//! for the first 5 slots after graduation and backruns large opening buyers.
//!
//! Architecture:
//!   1. MigrationEvent arrives → spawn DexMonitor for this mint
//!   2. DexMonitor subscribes to Raydium/PumpSwap trade events for 5 slots
//!   3. On large buy (> min_backrun_sol): submit backrun bundle via Jito
//!   4. Exit: take_profit (2%) or stop_loss (1.5%) or max_slots (5)

use std::sync::Arc;
use dashmap::DashMap;
use tokio::time::{Duration, Instant};

pub struct GradDexBackrunConfig {
    pub enabled: bool,
    pub paper_mode: bool,
    pub monitor_slots: u8,           // default: 5
    pub min_trigger_buy_sol: f64,    // default: 0.5 SOL
    pub entry_size_sol: f64,         // default: 0.1 SOL
    pub take_profit_pct: f64,        // default: 0.02
    pub stop_loss_pct: f64,          // default: 0.015
    pub jito_tip_sol: f64,           // default: 0.002
    pub monitor_timeout_ms: u64,     // default: 2000 (5 slots ≈ 2s on Solana)
}

pub struct GradDexBackrunEngine {
    config: GradDexBackrunConfig,
    active_monitors: DashMap<[u8;32], DexMonitorState>,
    // paper logger handle
}

struct DexMonitorState {
    mint: [u8;32],
    pool_address: [u8;32],
    start_ms: u64,
    slot_budget: u8,
    position: Option<BackrunPosition>,
}

impl GradDexBackrunEngine {
    /// Called when a migration event fires for a token.
    /// Spawns async monitor task.
    pub async fn on_migration(&self, mint: [u8;32], pool: PoolResolution) { ... }
    
    /// Called for every DEX trade on monitored mints.
    pub fn on_dex_trade(&self, mint: [u8;32], buy_sol: f64, price: f64, is_buy: bool) { ... }
}
```

### Integration:
- Wire `GradDexBackrunEngine` into `feeds/corecast.rs` migration event handler
- Reuse `arb/pool_resolver.rs` for pool detection (keep this infrastructure)
- Reuse `arb/dedup.rs` for migration dedup
- Paper log to `data/grad_dex_backrun_paper_trades.jsonl`

---

## TASK F — Rust Status Script + Monitoring Updates (scripts/rust-status.js)
**Owner files**: `scripts/rust-status.js`, `data/heartbeat-trade-state.json`
**Expected impact**: observability — catch regressions early

### Updates needed:
1. Add per-exit-reason breakdown to status output (already in hot_path gate_reject_counts)
2. Add gate rejection histogram to `/api/health` endpoint (expose `gate_reject_counts`)
3. Add MaxCurveProgress to known gate reject labels
4. Track `max_hold_pct` (max_hold exits / total exits) — alert if > 20% (currently 27%)
5. Add fee_drag metric: `fees_sol / gross_sol` — alert if > 80%

### API endpoint change (`api/server.rs`):
Add `gate_reject_histogram` to health response:
```json
{
  "gate_rejects": {
    "MaxCurveProgress": 142,
    "ScoreTooLow": 891,
    "BlockedHour": 203,
    ...
  }
}
```

---

## Interface Contracts Between Tasks

| Task | Exports | Consumed By |
|------|---------|-------------|
| A (gates) | `GateRejectReason::MaxCurveProgress`, raised defaults | F (monitoring labels) |
| B (fees) | `compute_jito_tip(score, size, base)` | tx/executor.rs call sites |
| C (backrun) | `MomentumScorer::check_entry_quality()` | momentum/mod.rs |
| D (shredstream) | `SigDedup` struct, shredstream channel | corecast.rs, hot_path.rs |
| E (grad backrun) | `GradDexBackrunEngine`, `GradDexBackrunConfig` | corecast.rs migration handler |
| F (monitoring) | Updated health API schema | heartbeat scripts |

Tasks A, C, E, F are fully independent — zero shared file ownership.
Tasks B and D both touch `engine/config.rs` — coordinate on adding new fields without conflict (B adds fee fields, D adds shredstream fields — different struct sections).

---

## Build Order / Parallelism

```
PARALLEL BATCH 1 (all independent):
  Task A — Entry gate surgery          [2-3 hours]
  Task C — Backrun anti-flat filter    [2-3 hours]  
  Task E — Grad→DEX backrun engine     [4-5 hours]
  Task F — Monitoring updates          [1-2 hours]

SEQUENTIAL after Batch 1:
  Task B — Dynamic fee filter          [3-4 hours] (needs A's score threshold)
  Task D — ShredStream activation      [4-6 hours] (infrastructure risk, last)
```

---

## Performance Budget

| Task | Hot Path Impact | Acceptable? |
|------|----------------|-------------|
| A (MaxCurveProgress gate) | +2 integer comparisons (~2ns) | ✅ |
| B (EV gate) | +1 float multiply + compare (~5ns) | ✅ |
| C (backrun filter) | Not in hot path (momentum engine) | ✅ |
| D (ShredStream dedup) | +1 hash lookup per event (~10ns) | ✅ |
| E (grad backrun) | Not in hot path (async task) | ✅ |
| F (monitoring) | Not in hot path | ✅ |

All hot path additions stay within +20ns total budget. Hot path remains icache-resident.

---

## Validation Plan

1. **After Task A**: Paper mode 1h — verify max_hold exit % drops, trade rate drops ~30%
2. **After Task B**: Paper mode 1h — verify avg_fee_mSOL drops, fee_drag metric improves
3. **After Task C**: Paper mode 2h with backrun active — verify momentum_decay_flat drops
4. **After Task D**: Compare entry timestamps — ShredStream events should arrive 50-150ms earlier
5. **After Task E**: Paper mode 24h — verify grad_dex_backrun_paper_trades.jsonl populates
6. **Regression check after all**: `cargo test` passes, daemon starts clean, health endpoint responds

---

## Priority Order

1. **Task A first** (config-only, immediate +2.84 SOL, 2 hours) — ship today
2. **Task F alongside** (monitoring, catch regressions)
3. **Task C** (backrun fix, +0.90 SOL)
4. **Task B** (fee optimization, +3.80 SOL — needs data from A running first)
5. **Task E** (new engine, standalone)
6. **Task D last** (ShredStream, highest risk, highest reward)
