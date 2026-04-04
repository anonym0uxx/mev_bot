# Backrunner Removal Refactor Plan

## Goal
Strip the legacy bonding-curve backrunner (MEV backrun engine) from the codebase, leaving ONLY the momentum graduation engine as the sole trading path.

## Current Architecture

```
main.rs
  ├── feeds/ (PumpPortal, Helius, CoreCast, ShredStream, EventJoiner)
  │     └── FeedEvents → engine_rx channel
  ├── engine/ (BACKRUNNER — remove)
  │     ├── hot_path.rs — main event loop dispatcher, mint_map, scoring pipeline
  │     ├── gates.rs — 12+ sequential gates for backrun trigger filtering
  │     ├── scorer.rs — weighted composite score for backrun qualification
  │     ├── scoring.rs — LUT builders, sigmoid/gaussian precomputation
  │     ├── entry_engine.rs — Kelly-tiered sizing + magnitude prediction
  │     ├── positions.rs — position tracking, TP/SL/exit logic for BC trades
  │     ├── exit_v4.rs — urgency-based exit engine for BC trades
  │     ├── ride_state.rs — ride state machine for BC positions
  │     ├── watchlist.rs — two-phase entry watchlist for BC
  │     ├── entry_randomizer.rs — jitter to avoid MEV fingerprinting
  │     ├── regime.rs — regime classifier (bonding curve progress)
  │     ├── bonding_curve.rs — constant-product AMM simulation
  │     ├── risk_manager.rs — risk gating for BC entries
  │     ├── bayesian_signal.rs — Bayesian signal model for BC
  │     ├── kelly_sizing.rs — Kelly criterion (PARTIALLY NEEDED by momentum)
  │     ├── health.rs — health monitoring (NEEDED)
  │     ├── config.rs — config loading (NEEDED, but trimmed)
  │     └── integration_tests.rs — BC integration tests (remove)
  ├── momentum/ (KEEP — this is the engine)
  │     ├── mod.rs (6112 lines) — core momentum engine
  │     ├── config.rs — MomentumConfig
  │     ├── pool.rs — PumpSwap/Raydium pool resolution
  │     ├── position.rs — momentum position management
  │     ├── price_feed.rs — WS price feed
  │     ├── scorer.rs — graduation scorer
  │     ├── velocity.rs — velocity/acceleration tracking
  │     ├── rpc_sender.rs — TX submission (Jito/Nozomi)
  │     ├── logger.rs — trade logging
  │     └── tod.rs — time-of-day gating
  ├── tx/ (KEEP — used by momentum)
  ├── feeds/ (KEEP)
  ├── core/ (PARTIALLY NEEDED — mint_map used by hot_path for enrichment)
  ├── persistence/ (KEEP)
  ├── api/ (KEEP, trim BC stats)
  ├── alerts/ (KEEP)
  └── system/ (KEEP)
```

## What Momentum ACTUALLY Needs from engine/

### 1. `ScoredToken` + `GradEnrichment` (from `hot_path.rs`)
- `ScoredToken`: struct with score, magnitude, kelly_size, conviction — passed via channel
- `GradEnrichment`: grad_speed_s, volume, buys_5s, unique_buyers, sells_5s — extracted from mint_map at migration time
- **Action:** Move both structs to `momentum/` or a shared `types.rs`

### 2. `kelly_sizing.rs` (partial)
- `compute_momentum_kelly_size()` — used by momentum for live sizing
- `compute_momentum_kelly_inputs()` — parses trade history for WR/avg_win/avg_loss
- `MomentumPaperTrade` — simple struct
- `PaperBankroll` / `BankrollSource` — used by hot_path only
- **Action:** Move the 3 momentum-relevant items to `momentum/kelly.rs`; delete the rest

### 3. `health.rs` — HealthMonitor
- Used by momentum indirectly (via main.rs health_monitor)
- Also used in main.rs event loop for feed staleness detection
- **Action:** Keep, but move to top-level module (`health.rs` or `monitoring/`)

### 4. `config.rs` — EngineConfig
- Loads `canary.json`, contains `MomentumConfig` (nested), gate config, position config, etc.
- MomentumConfig is already in `momentum/config.rs`
- EngineConfig has TONS of backrunner fields (gate, position, ride, etc.)
- **Action:** Create new minimal `AppConfig` that loads canary.json and extracts only what momentum needs: paper_mode, log_file, health config, and the MomentumConfig blob

## Modules to DELETE (pure backrunner)

| Module | Lines | Purpose |
|--------|-------|---------|
| `engine/gates.rs` | 586 | MEV backrun trigger gates |
| `engine/scorer.rs` | 437 | Backrun composite scorer |
| `engine/scoring.rs` | 393 | LUT/sigmoid helpers for scorer |
| `engine/entry_engine.rs` | 878 | Kelly-tiered entry pipeline |
| `engine/positions.rs` | 1397 | BC position tracking + TP/SL |
| `engine/exit_v4.rs` | 973 | Urgency-based exit engine |
| `engine/ride_state.rs` | 866 | Ride state machine |
| `engine/watchlist.rs` | 731 | Two-phase entry watchlist |
| `engine/entry_randomizer.rs` | 168 | MEV fingerprint jitter |
| `engine/regime.rs` | 303 | Bonding curve regime classifier |
| `engine/bonding_curve.rs` | 216 | Pump.fun AMM simulation |
| `engine/risk_manager.rs` | 589 | BC risk gating |
| `engine/bayesian_signal.rs` | 737 | Bayesian signal model |
| `engine/integration_tests.rs` | 123 | BC tests |
| `engine/hot_path.rs` | 1027 | Event dispatcher + mint_map (REWRITE) |
| `core/mint_map.rs` | 365 | Trade history ring buffer |
| `core/trade_record.rs` | 42 | Trade record struct |
| **TOTAL** | **~9,831** | |

## Modules to KEEP (momentum + shared infra)

| Module | Lines | Purpose |
|--------|-------|---------|
| `momentum/*` | ~16,000 | The engine |
| `tx/*` | ~3,500 | TX building (PumpSwap, Jito, Nozomi, wallet) |
| `feeds/*` | ~5,000 | Data feeds |
| `persistence/*` | 499 | SQLite + JSONL logging |
| `api/*` | 440 | HTTP health/status API |
| `alerts/*` | ~200 | Telegram alerts |
| `system/*` | 425 | OS tuning |
| `engine/health.rs` | 563 | Health monitoring |
| `engine/kelly_sizing.rs` | 893 | Kelly (partial — move to momentum/) |
| `engine/config.rs` | 1715 | Config (rewrite — trim to momentum needs) |

## Refactor Steps (ordered)

### Phase 1: Extract types + kelly → momentum/
1. Create `momentum/types.rs` with `ScoredToken` and `GradEnrichment`
2. Create `momentum/kelly.rs` with `compute_momentum_kelly_size`, `compute_momentum_kelly_inputs`, `MomentumPaperTrade`
3. Update all `momentum/` imports to use local modules
4. **Build + test — momentum should compile with no `engine::hot_path` or `engine::kelly_sizing` imports**

### Phase 2: Rewrite main.rs
1. Create `config/app_config.rs` — minimal config loader:
   - `paper_mode: bool`
   - `log_file: String`
   - `health: HealthConfig`
   - `momentum: MomentumConfig` (from momentum/config.rs)
   - `bonding_curve_enabled: false` (hardcoded, for backward compat during transition)
2. Rewrite main.rs event loop:
   - Remove HotPath, PositionManager, EntryEngine, RiskManager construction
   - Remove `ScoredToken` channel (momentum computes its own scores via `momentum/scorer.rs`)
   - Simplify event dispatch: Trade/PreWarm → health_monitor only; Migration/PumpSwapGrad → momentum; Tick → momentum
   - Remove `drain_closed_positions` (backrunner concept)
   - Remove all `engine_config.gate.*` and `engine_config.position.*` references
3. **Build + test**

### Phase 3: Delete backrunner modules
1. Delete all files listed in "Modules to DELETE" above
2. Delete `engine/mod.rs` re-exports for deleted modules
3. Move `engine/health.rs` → `monitoring/health.rs` (or keep in engine/ with just health + config)
4. Clean up `engine/config.rs` — remove all gate/position/ride/exit fields, keep only what momentum needs
5. Remove `core/` module entirely (mint_map + trade_record only used by hot_path)
6. **Build + test**

### Phase 4: Clean up API + config
1. Remove all backrunner stats from `api/server.rs` (grad_arb_*, position stats)
2. Simplify `canary.json` — remove gate/position/ride/exit sections
3. Remove dead config fields from `engine/config.rs`
4. **Build + test + deploy**

### Phase 5: Decouple GradEnrichment from mint_map
Currently: `hot_path.on_migration()` extracts enrichment from `mint_map` (trade history).
After: Momentum engine must compute enrichment itself OR we pass raw feed data differently.

**Key question:** Does momentum actually USE the enrichment from hot_path?
- `grad_speed_s` — YES, used in scorer
- `volume_sol_x100` — YES, used in scorer  
- `buys_5s` — YES, used in scorer
- `unique_buyers` — YES, used in scorer
- `sells_5s` — YES, used in scorer

These are currently computed from the `MintHistoryMap` ring buffer (per-trade history built from Trade events). Momentum engine doesn't see Trade events directly — it only sees Migration/PumpSwapGraduationDirect.

**Options:**
A. **Move mint_map into momentum** — momentum receives Trade events too, builds its own history
B. **Compute enrichment from feed data at migration time** — Helius Enhanced WS already provides vault balances, we can derive volume/buyers from that
C. **Keep a slim event_tracker module** that accumulates trade stats per mint and provides enrichment at migration time

**Recommendation:** Option C — create a lightweight `enrichment_tracker.rs` (replaces 365-line mint_map + 1027-line hot_path with ~200 lines). It listens for Trade events, maintains per-mint counters, and returns GradEnrichment on request.

## Risk Assessment
- **LOW RISK:** Phases 1-3 are mechanical refactoring — no logic changes
- **MEDIUM RISK:** Phase 5 (enrichment decoupling) changes data flow
- **Rollback:** Git branch, easy revert if anything breaks

## Expected Outcome
- **~10K lines deleted** (backrunner code)
- **~200 lines added** (enrichment_tracker, kelly.rs, types.rs)
- **Cleaner binary** — no dead code paths, faster compile
- **Single entry path** — momentum engine is THE engine
- **Config simplification** — canary.json drops from ~200 fields to ~80

## Timeline Estimate
- Phase 1: 30 min (extract types)
- Phase 2: 1-2 hours (rewrite main.rs)
- Phase 3: 30 min (delete files)
- Phase 4: 30 min (API cleanup)
- Phase 5: 1 hour (enrichment tracker)
- **Total: ~3-4 hours**
