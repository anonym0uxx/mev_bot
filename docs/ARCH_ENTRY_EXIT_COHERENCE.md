# ARCH_ENTRY_EXIT_COHERENCE.md — Engineer 4: Entry-Exit Coherence Wiring

## Overview

**Primary file:** `rust/pump-quant-core/src/engine/positions.rs` (MODIFY)

**Integration touchpoints:**
1. `ride_state.rs` (Engineer 2) — new `RideState::new()` signature, new `on_buy_event()` / `on_sell_event()` signatures
2. `persistence/paper_logger.rs` — bump to dv8, add Bayesian fields, wire existing TODO zeros

**Goal:** Wire EntryConviction through to Bayesian prior initialization at position open, capture Bayesian state at position close, update paper_logger.rs to dv8 with full Bayesian + Kelly fields, and implement the Kelly-proportional trail integration.

---

## positions.rs Changes

### 1. ClosedPosition: Add Bayesian Exit Fields

```rust
/// A closed position with full PnL accounting.
pub struct ClosedPosition {
    // ... ALL existing fields unchanged ...

    // ── NEW: Bayesian exit state fields ──

    /// Bayesian half-Kelly fraction at exit × 1000.
    /// Positive = positive EV was remaining. Negative = EV had gone negative.
    /// Computed from α, β, R at close time.
    pub bayesian_f_at_exit: i16,

    /// Beta α parameter × 16 at exit.
    pub alpha_at_exit: u16,

    /// Beta β parameter × 16 at exit.
    pub beta_at_exit: u16,
}
```

**Size impact:** +6 bytes on ClosedPosition. ClosedPosition is heap-allocated (sent via crossbeam channel) so size is not cache-critical. It's a cold-path struct.

### 2. open_position() — Pass Conviction to RideState::new()

**Current call site in `open_position()`:**
```rust
let ride_state = RideState::new(
    entry_mvsol, entry_mvsol, now_ms,
    conviction.f_permille as u32,
    &self.config.ride_config,
);
```

**New call site (v3):**
```rust
let ride_state = RideState::new(
    entry_mvsol,
    now_ms,
    conviction.f_permille as u32,
    conviction.p_permille,            // NEW: win prob for Bayesian prior
    conviction.r_x100,                // NEW: reward ratio for Bayesian prior
    conviction.conviction_tier,       // NEW: prior strength tier
    self.config.ride_config.use_bayesian_signal, // NEW: feature flag
    &self.config.ride_config,
);
```

**RideConfig addition:**
```rust
// In engine/config.rs, add to RideConfig:
pub use_bayesian_signal: bool,  // default: false
```

### 3. on_subsequent_trade() — Pass Source + Dedup

**Current buy path:**
```rust
rs.on_buy_event(buy_mvsol, now_ms, wallet_hash);
```

**New buy path (v3):**
```rust
let source_u8 = event.source.as_u8();
let skip_bayesian = deduped; // from Engineer 3's dedup parameter
rs.on_buy_event(buy_mvsol, now_ms, wallet_hash, source_u8, skip_bayesian);
```

**Current sell path:**
```rust
if let Some(reason) = rs.on_sell_event(sell_mvsol, now_ms, &self.config.ride_config) {
```

**New sell path (v3):**
```rust
let source_u8 = event.source.as_u8();
let is_creator_sell = false; // Regular trade path; creator sell comes via on_creator_sell()
if let Some(reason) = rs.on_sell_event(sell_mvsol, now_ms, source_u8, is_creator_sell, &self.config.ride_config) {
```

**Function signature change:**
```rust
// OLD:
pub fn on_subsequent_trade(&mut self, event: &TradeEvent, now_ms: u64) -> bool

// NEW:
pub fn on_subsequent_trade(&mut self, event: &TradeEvent, now_ms: u64, deduped: bool) -> bool
```

### 4. close_position_inner() — Capture Bayesian Exit State

**Add to `close_position_inner()`, after existing signal v2 extraction:**

```rust
// Extract Bayesian exit state from RideState
let (bayesian_f_at_exit, alpha_at_exit, beta_at_exit) = match &pos.exit_mode {
    ExitMode::Ride(rs) => {
        // Compute f̂ at exit from inline Bayesian fields
        let total = rs.alpha_x16 as u32 + rs.beta_x16 as u32;
        let p_x1000 = if total > 0 {
            (rs.alpha_x16 as u32 * 1000) / total
        } else {
            500 // fallback: 50% if somehow both are 0
        };
        let r = rs.r_est_x100.max(1) as i32;
        let r_plus_1 = r + 100;
        let numerator = (p_x1000 as i32 * r_plus_1 / 1000) - 100;
        let f_raw = numerator * 1000 / r;
        let f_hat = (f_raw / 2) as i16;

        (f_hat, rs.alpha_x16, rs.beta_x16)
    }
};
```

**Wire into ClosedPosition construction:**
```rust
let closed = ClosedPosition {
    // ... all existing fields ...

    // NEW Bayesian fields
    bayesian_f_at_exit,
    alpha_at_exit,
    beta_at_exit,
};
```

### 5. Kelly Trail Integration Formula

The trail width formula changes from:
```
trail_bp = (base × kelly_mult × phase_mult) >> 16
```
to (in Bayesian mode):
```
trail_bp = base_bp × clamp(f_hat × 256 / f_entry, 64, 400) >> 8
```

**This is implemented in `ride_state.rs` by Engineer 2.** Engineer 4 only needs to know the formula exists and that `entry_f_permille` is passed through correctly.

**Detailed formula (for Engineer 2 reference — duplicated from ARCH_RIDESTATE_V3.md):**
```rust
let f_hat = self.current_f_permille(); // i16
if f_hat <= 0 || base_bp == 0 {
    self.current_trail_bp = 0; // Will trigger exit on next check
} else {
    let ratio = ((f_hat as u32) * 256 / self.entry_f_permille.max(1) as u32)
        .max(64)   // Minimum 25% of base trail (64/256)
        .min(400); // Maximum 156% of base trail (400/256)
    self.current_trail_bp = ((base_bp as u32 * ratio) >> 8)
        .max(config.kelly_min_trail_bp as u32)
        .min(config.kelly_max_trail_bp as u32) as u16;
}
```

**Clamping rationale:**
- `min(64/256 = 25%)`: Even with degraded conviction, don't make trail narrower than 25% of base. Prevents jitter exits from noise.
- `max(400/256 = 156%)`: Don't let trail exceed 156% of base even with very strong conviction. Prevents over-wide trails that miss real exits.

### 6. Online EMA Recalibration Hook

When a position closes, compare the Bayesian outcome with the LUT prior to detect systematic bias:

```rust
/// Called after close_position_inner emits a ClosedPosition.
/// Checks if the Bayesian posterior disagreed with the LUT prior.
/// If so, logs a recalibration hint (actual LUT update is offline).
///
/// This is a COLD PATH — called once per position close (~10-50/hour).
/// No performance concern.
#[cold]
fn log_recalibration_hint(pos: &ClosedPosition) {
    // Compare entry prior p with observed outcome
    // Outcome: win if net_pnl_sol > 0, loss otherwise
    let is_win = pos.net_pnl_sol > 0;
    let entry_p = pos.entry_p_permille;
    let bayesian_p_at_exit = if pos.alpha_at_exit as u32 + pos.beta_at_exit as u32 > 0 {
        (pos.alpha_at_exit as u32 * 1000) /
            (pos.alpha_at_exit as u32 + pos.beta_at_exit as u32)
    } else {
        500
    } as u16;

    // If entry said p=600 but Bayesian posterior at exit says p=350, that's a
    // large disagreement suggesting the LUT cell is miscalibrated
    let disagreement = (entry_p as i32 - bayesian_p_at_exit as i32).unsigned_abs();
    if disagreement > 150 {
        tracing::debug!(
            mint = %bs58::encode(&pos.mint).into_string(),
            entry_p_permille = entry_p,
            bayesian_p_at_exit = bayesian_p_at_exit,
            is_win,
            disagreement,
            "RECALIBRATION HINT: LUT prior disagrees with Bayesian posterior by {}‰",
            disagreement
        );
    }
}
```

This is logged only; actual LUT updates happen offline from JSONL analysis.

---

## paper_logger.rs Changes (dv8)

### Bump Data Version

```rust
// OLD:
"dataVersion": 7,

// NEW:
"dataVersion": 8,
```

### Add Bayesian Fields to JSONL

```rust
// In the json!({}) block, ADD these fields:

// ── Bayesian signal fields (dv8) ──
"bayesianFAtExit": pos.bayesian_f_at_exit,     // i16: half-Kelly fraction at exit (permille)
"alphaAtExit": pos.alpha_at_exit,               // u16: Beta α×16 at exit
"betaAtExit": pos.beta_at_exit,                 // u16: Beta β×16 at exit
```

### Wire Existing TODO Zeros

The current paper_logger.rs has:
```rust
// TODO: replace 0 defaults with closed.entry_p_permille, etc.
"entryPPermille": 0u16,
"entryRx100": 0u16,
"entryFPermille": 0u16,
"convictionTier": 0u8,
```

**Replace with actual values from ClosedPosition:**
```rust
// Kelly conviction at entry (dv8 — wired from ClosedPosition)
"entryPPermille": pos.entry_p_permille,
"entryRx100": pos.entry_r_x100,
"entryFPermille": pos.entry_f_permille,
"convictionTier": pos.conviction_tier,
```

These fields already exist on `ClosedPosition` (added in the current v2 codebase) — they just weren't wired in the logger. The ClosedPosition struct already has:
```rust
pub entry_p_permille: u16,
pub entry_r_x100: u16,
pub entry_f_permille: u16,
pub conviction_tier: u8,
```

And `close_position_inner()` already populates them:
```rust
entry_p_permille: pos.conviction.p_permille,
entry_r_x100: pos.conviction.r_x100,
entry_f_permille: pos.conviction.f_permille,
conviction_tier: pos.conviction.conviction_tier,
```

So the ONLY change is in `paper_logger.rs`: replace the `0` literals with `pos.entry_p_permille` etc.

### Full dv8 JSONL Schema Additions

```json
{
  // ... all existing dv7 fields unchanged ...

  // dv8 additions:
  "bayesianFAtExit": -15,        // i16: Bayesian f̂ at exit (permille). Negative = EV was gone.
  "alphaAtExit": 85,             // u16: Beta α×16 at exit
  "betaAtExit": 142,             // u16: Beta β×16 at exit

  // dv8 fixes (were TODO zeros in dv7):
  "entryPPermille": 580,         // u16: p × 1000 from Kelly LUT
  "entryRx100": 1200,            // u16: R × 100 from Kelly LUT
  "entryFPermille": 289,         // u16: half-Kelly f × 1000
  "convictionTier": 0,           // u8: 0=LOW, 1=MED, 2=HIGH

  "dataVersion": 8               // bumped from 7
}
```

---

## RideConfig Addition

```rust
// In engine/config.rs, add to RideConfig struct:
/// Feature flag: use Bayesian signal for exit decisions.
/// When false (default): old composite score drives exits, Bayesian is logged only.
/// When true: Bayesian drives exits, old composite is logged only.
pub use_bayesian_signal: bool,
```

**Default value:** `false`

```rust
impl Default for RideConfig {
    fn default() -> Self {
        Self {
            // ... existing fields ...
            use_bayesian_signal: false,
        }
    }
}
```

---

## Integration Seams

### Called BY (upstream):

| Caller | What | Where |
|--------|------|-------|
| `hot_path.rs::on_trade()` | `position_manager.on_subsequent_trade(event, now, deduped)` | Engineer 3 adds `deduped` param |
| `hot_path.rs::on_trade()` | `position_manager.open_position(event, score, now, mag, size, conviction)` | Unchanged signature |

### Calls INTO (downstream):

| Function | Target | What Changes |
|----------|--------|-------------|
| `open_position()` | `RideState::new()` | NEW: +p_permille, +r_x100, +conviction_tier, +use_bayesian |
| `on_subsequent_trade()` | `RideState::on_buy_event()` | NEW: +source_u8, +skip_bayesian |
| `on_subsequent_trade()` | `RideState::on_sell_event()` | NEW: +source_u8, +is_creator_sell |
| `close_position_inner()` | reads `RideState` fields | NEW: reads alpha_x16, beta_x16, r_est_x100, computes f_hat |
| `PaperTradeLogger::log()` | writes JSONL | NEW: +bayesianFAtExit, +alphaAtExit, +betaAtExit, wires entryPPermille etc. |

### Files this engineer writes:

1. **PRIMARY:** `src/engine/positions.rs` — modify ClosedPosition, open_position(), on_subsequent_trade(), close_position_inner()
2. **SECONDARY:** `src/persistence/paper_logger.rs` — bump dv8, add Bayesian fields, wire TODO zeros

### Files this engineer does NOT touch:

- `bayesian_signal.rs` (Engineer 1)
- `ride_state.rs` (Engineer 2)
- `hot_path.rs` (Engineer 3)
- `feeds/mod.rs` (Engineer 3)

---

## Performance Budget

All changes are on cold paths (position open ~10-50/hour, position close ~10-50/hour, JSONL write ~10-50/hour). No hot-path performance impact.

| Operation | Path | Budget |
|-----------|------|--------|
| Prior initialization in `open_position()` | Cold (entry) | <50ns, called ~10-50/hour |
| Bayesian f̂ computation in `close_position_inner()` | Cold (exit) | <15ns, called ~10-50/hour |
| JSONL field serialization in `paper_logger.rs` | Cold (logging) | <500ns added (3 extra serde fields) |
| `on_subsequent_trade()` dedup param | Hot path | <1ns (bool parameter pass) |
| `on_subsequent_trade()` source extraction | Hot path | <1ns (`event.source.as_u8()`) |
| **Total hot path overhead** | | <2ns |

---

## Compile-Time Assertions

```rust
// In positions.rs:
// ClosedPosition grew by 6 bytes — document new size for regression
const _: () = assert!(
    core::mem::size_of::<ClosedPosition>() <= 600,
    "ClosedPosition grew too large — audit field additions"
);

// Verify EntryConviction fields are all accessible:
const _: () = assert!(core::mem::size_of::<EntryConviction>() <= 24);
```

---

## Test Cases

### Test 1: Conviction flows through to RideState Bayesian prior

```rust
#[test]
fn test_conviction_flows_to_bayesian_prior() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut config = test_config();
    config.ride_config.use_bayesian_signal = true;
    let mut pm = PositionManager::new(config, tx);

    let conviction = EntryConviction {
        p_permille: 600,
        r_x100: 1200,
        f_permille: 280,
        size_lamports: 100_000_000,
        conviction_tier: 1, // MED
        _pad: [0; 5],
    };

    let mint = [0xAA; 32];
    let event = make_trade_event(mint, [0xBB; 64], 50_000_000, 30_000_000_000,
                                  1_000_000_000_000_000, true);
    pm.open_position(&event, 75.0, 1000, 60.0, conviction.size_lamports, conviction);

    let pos = pm.get_position_mut(&mint).unwrap();
    match &pos.exit_mode {
        ExitMode::Ride(rs) => {
            // MED tier: total = 9 * 16 = 144
            // alpha_0 = 600 * 144 / 1000 = 86
            // beta_0 = 144 - 86 = 58
            assert!(rs.alpha_x16 >= 80 && rs.alpha_x16 <= 96,
                "α should be ~86, got {}", rs.alpha_x16);
            assert!(rs.beta_x16 >= 48 && rs.beta_x16 <= 64,
                "β should be ~58, got {}", rs.beta_x16);
            assert_eq!(rs.r_est_x100, 1200);
            assert_eq!(rs.entry_f_permille, 280);
        }
    }
}
```

### Test 2: Bayesian exit state captured in ClosedPosition

```rust
#[test]
fn test_bayesian_exit_state_captured() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut config = test_config();
    config.ride_config.use_bayesian_signal = true;
    let mut pm = PositionManager::new(config, tx);

    let conviction = EntryConviction {
        p_permille: 550,
        r_x100: 1000,
        f_permille: 250,
        size_lamports: 100_000_000,
        conviction_tier: 1,
        _pad: [0; 5],
    };

    let mint = [0xAA; 32];
    let event = make_trade_event(mint, [0xBB; 64], 50_000_000, 30_000_000_000,
                                  1_000_000_000_000_000, true);
    pm.open_position(&event, 75.0, 1000, 60.0, conviction.size_lamports, conviction);

    // Feed a confirming buy, then force close
    let buy = make_trade_event(mint, [0xCC; 64], 100_000_000, 31_000_000_000,
                                1_000_000_000_000_000, true);
    pm.on_subsequent_trade(&buy, 1100, false);
    pm.force_close(&mint, ExitReason::MaxHold, 2000);

    let closed = rx.try_recv().unwrap();
    // Should have Bayesian fields populated
    assert!(closed.alpha_at_exit > 0, "α at exit should be > 0");
    assert!(closed.beta_at_exit > 0, "β at exit should be > 0");
    // f̂ at exit should be defined (can be positive or negative)
    // Since we only had 1 buy and force-closed, f̂ should be near entry value
}
```

### Test 3: Paper logger dv8 output includes Bayesian fields

```rust
#[test]
fn test_paper_logger_dv8_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.jsonl");
    let mut logger = PaperTradeLogger::new(
        path.to_str().unwrap(), true, "test-v3".to_string(),
    ).unwrap();

    let closed = ClosedPosition {
        // ... minimal required fields ...
        bayesian_f_at_exit: -15,
        alpha_at_exit: 85,
        beta_at_exit: 142,
        entry_p_permille: 580,
        entry_r_x100: 1200,
        entry_f_permille: 289,
        conviction_tier: 1,
        // ... rest of fields ...
    };

    logger.log(&closed, "SomeMintBase58").unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let json: serde_json::Value = serde_json::from_str(content.trim()).unwrap();

    assert_eq!(json["dataVersion"], 8);
    assert_eq!(json["bayesianFAtExit"], -15);
    assert_eq!(json["alphaAtExit"], 85);
    assert_eq!(json["betaAtExit"], 142);
    assert_eq!(json["entryPPermille"], 580);
    assert_eq!(json["entryRx100"], 1200);
    assert_eq!(json["entryFPermille"], 289);
    assert_eq!(json["convictionTier"], 1);
}
```

### Test 4: on_subsequent_trade with dedup=true skips Bayesian update

```rust
#[test]
fn test_dedup_skips_bayesian() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut config = test_config();
    config.ride_config.use_bayesian_signal = true;
    let mut pm = PositionManager::new(config, tx);

    let conviction = EntryConviction {
        p_permille: 500, r_x100: 1000, f_permille: 200,
        size_lamports: 100_000_000, conviction_tier: 1, _pad: [0; 5],
    };

    let mint = [0xAA; 32];
    let event = make_trade_event(mint, [0xBB; 64], 50_000_000, 30_000_000_000,
                                  1_000_000_000_000_000, true);
    pm.open_position(&event, 75.0, 1000, 60.0, conviction.size_lamports, conviction);

    let alpha_before = get_alpha(&pm, &mint);

    // Non-deduped buy → should update α
    let buy1 = make_trade_event(mint, [0xCC; 64], 200_000_000, 31_000_000_000,
                                 1_000_000_000_000_000, true);
    pm.on_subsequent_trade(&buy1, 1100, false);
    let alpha_after_normal = get_alpha(&pm, &mint);
    assert!(alpha_after_normal > alpha_before, "Normal buy should increase α");

    // Deduped buy → should NOT update α
    let buy2 = make_trade_event(mint, [0xDD; 64], 200_000_000, 31_500_000_000,
                                 1_000_000_000_000_000, true);
    pm.on_subsequent_trade(&buy2, 1200, true);
    let alpha_after_dedup = get_alpha(&pm, &mint);
    assert_eq!(alpha_after_dedup, alpha_after_normal,
        "Deduped buy should NOT change α: {} == {}", alpha_after_dedup, alpha_after_normal);
}
```
