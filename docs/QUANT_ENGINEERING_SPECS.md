# Quant Engineering Specs — Momentum Engine Improvements

## Data Summary (24h, 4,440 trades on Raydium AMM V4)

- Net PnL: +69.49 SOL (but top 50 trades = +82.5 SOL, tail = -13 SOL)
- 85% of trades exit via `time_sl` (flat tokens, -0.31 mSOL avg — cheap)
- `hard_sl` is #1 PnL destroyer: 198 trades, -9.88 SOL (-49.9 mSOL avg)
- `trailing_stop` is the bread-and-butter winner: 230 trades, +18.40 SOL, 68% WR
- `tp3` moonshots: 53 trades, +67.78 SOL, 100% WR
- 89% of trades get ZERO price samples from Helius WS
- Price feed only works for enriched graduations (505/4440 trades)
- Re-entries on same mint are GOOD here (+65.6 SOL from re-entries)
- `hard_sl` trades dump fast: 60 within <1s, avg -55.85 mSOL

## Codebase Structure

```
rust/pump-quant-core/src/
├── momentum/
│   ├── mod.rs          (2052 lines) — main engine: on_graduation(), on_tick(), position management
│   ├── config.rs       (391 lines)  — MomentumConfig with serde defaults
│   ├── position.rs     (1094 lines) — MomentumPosition, exit logic, price sample tracking
│   ├── price_feed.rs   (612 lines)  — Hybrid WS+RPC price feed for Raydium vaults
│   ├── scorer.rs       (260 lines)  — Integer-only graduation scorer (speed/volume/velocity/recovery)
│   ├── pool.rs         (640 lines)  — Pool resolution (Raydium AMM V4)
│   └── logger.rs       (242 lines)  — JSONL paper trade logger
├── engine/             — OLD bonding curve engine (disabled, bonding_curve_enabled=false)
├── feeds/              — PumpPortal, Helius, CoreCast, ShredStream feed clients
└── tx/                 — Raydium swap builder, Jito gRPC, Nozomi, tip engine
```

Config: `rust/canary.json` → `mev.momentum` section

---

## TASK 1: Price Feed Fix (price_feed.rs, mod.rs)

### Problem
89% of trades (3,957/4,440) get `price_sample_count=0` and `ws_notif_count_at_close=0`. 
The Helius WS `accountSubscribe` for Raydium vault accounts is not delivering notifications for unenriched graduations.

Only the 505 enriched trades (those with `grad_volume_sol > 0`) receive price data. The unenriched 3,957 trades — which include ALL 53 tp3 moonshots (+67.78 SOL) — operate completely blind.

### Root Cause Investigation
In `momentum/mod.rs`, the `on_graduation()` path likely only resolves pool vaults and subscribes to the price feed when graduation enrichment data is present. When `grad_speed_s=0` and `grad_volume_sol=0` (cold/unenriched graduations), pool resolution may skip or fail silently, meaning `price_feed.subscribe()` never fires.

### Required Fix
1. In `momentum/mod.rs`: trace the `on_graduation()` path for cold graduations (where `GradEnrichment` is zeroed/default). Ensure `resolve_pool_from_transaction()` runs and `price_feed.subscribe(VaultSubscription{...})` is called regardless of enrichment quality.
2. In `price_feed.rs`: add tracing for subscription success/failure — log when `accountSubscribe` confirmations arrive (or don't) so we can distinguish "didn't subscribe" from "subscribed but no data".
3. Verify: after fix, new trades should show `ws_notif_count_at_close > 0` for >50% of trades (vs current 11%).

### Files to modify
- `rust/pump-quant-core/src/momentum/mod.rs` — `on_graduation()` and pool resolution path
- `rust/pump-quant-core/src/momentum/price_feed.rs` — subscription tracing

---

## TASK 2: Hard Stop-Loss Reduction (position.rs, mod.rs, config.rs)

### Problem
198 `hard_sl` trades lost -9.88 SOL total (-49.9 mSOL avg). 60 of these dumped within <1s of entry. These are tokens where the graduation opens and immediately sells off — classic pump-and-dump graduation pattern.

### Algorithm: Probe-then-scale entry
Instead of entering at full `size_sol` immediately, use a staged approach:

**Phase 1 — Probe** (first 2 seconds):
- Enter at `probe_size_sol` (already exists in config, currently 0.05 SOL)
- Monitor for hard dump: if price drops >5% within 2s, exit probe at minimal loss (~2.5 mSOL vs 49.9 mSOL)

**Phase 2 — Scale-in** (after 2s, if no dump):
- If price is flat or rising (gain ≥ 0 bps after 2s), scale up to full `size_sol`
- If price dropped >3% but <5%, stay at probe size with tight 3% SL
- If 0 price samples after 2s, stay at probe (don't scale blind)

### Expected Impact
- 60 trades that dump <1s: loss reduced from -55.85 mSOL to ~-2.5 mSOL each = save 3.2 SOL
- 48 trades dumping 1-5s: loss reduced from -52.47 mSOL to ~-5 mSOL each = save 2.3 SOL
- Total estimated recovery: ~5.5 SOL of the 9.88 SOL lost to hard_sl

### Config additions to `momentum` section in canary.json
```json
{
  "probe_entry_enabled": true,
  "probe_hold_ms": 2000,
  "probe_dump_threshold_bps": -500,
  "probe_scale_min_bps": -300,
  "probe_scale_require_price": true
}
```

### Files to modify
- `rust/pump-quant-core/src/momentum/position.rs` — add `ProbePhase` to MomentumState enum, track probe→scaled transition
- `rust/pump-quant-core/src/momentum/mod.rs` — in `on_tick()`, add probe evaluation before scale-in decision
- `rust/pump-quant-core/src/momentum/config.rs` — add new config fields

---

## TASK 3: Graduation Scorer Overhaul (scorer.rs, mod.rs)

### Problem
Current scorer produces `grad_score=25` (default) for 89% of trades because enrichment data is zeroed. Even when enrichment IS present (495 trades), the score isn't predictive — all grad_score buckets have similar WR (~7-24%) with no monotonic relationship.

### Algorithm: New scoring model based on actual trade outcomes

The current scorer weights:
- Speed (25pts): `(300 - min(speed, 300)) / 12` — but avg grad_speed_s=65s for enriched trades, so most get ~16/25
- Volume (25pts): `centisol / 2000` — avg is 539 SOL so most max at 25/25
- Velocity (25pts): `min(buys_5s, 25)` — avg is 3, so most get 3/25
- Recovery (25pts): `recovery_bps / 40` — checked at entry time

Problems:
1. Volume is nearly always maxed (useless discriminator)
2. Velocity is nearly always low (3 avg) — not enough variance
3. Recovery can't be computed without price data (which 89% lack)

### New Scorer Design
Replace the 4x25 model with a **5-component weighted model** (still integer-only):

```
score = speed_score(15) + volume_tier(10) + velocity_score(20) + buy_sell_ratio(25) + entry_discount(30)
```

1. **Speed** (0-15): Keep, but recalibrate — 60s=15, 120s=10, 180s=5, 300s+=0
2. **Volume tier** (0-10): Replace linear with tiers: <100→0, 100-300→4, 300-600→7, 600+→10
3. **Velocity** (0-20): Buys in 5s, but normalize per SOL of volume: `buys_5s * 100 / max(volume_sol, 1)`. High velocity relative to volume = organic demand, not just a whale.
4. **Buy/sell ratio** (0-25): NEW — `pre_grad_buys_5s / max(sells_5s, 1)`. High ratio = unidirectional buy pressure. Low ratio = distribution already happening. This data comes from GradEnrichment.
5. **Entry discount** (0-30): Replace recovery. `(bc_terminal_price - entry_price) / bc_terminal_price * 10000`. Buying below BC terminal = structural edge. This CAN be computed without WS data.

### Gate: `min_grad_score` should be raised from 40 to 50 once new scorer is live.

### Files to modify
- `rust/pump-quant-core/src/momentum/scorer.rs` — rewrite `score_graduation()` with new 5-component model
- `rust/pump-quant-core/src/engine/hot_path.rs` — ensure `GradEnrichment` includes `sells_5s` (already exists)
- `rust/pump-quant-core/src/momentum/mod.rs` — pass entry_price to scorer for discount component

---

## TASK 4: Time-of-Day Gating (mod.rs, config.rs)

### Problem
Alpha is concentrated in specific UTC hours. The engine trades unprofitably during dead hours:

| Block (UTC) | Trades | Net | WR |
|---|---|---|---|
| 08:00 | 143 | +34.88 | 36% |
| 14:00 | 603 | +22.50 | 12% |
| 16:00 | 881 | +9.82 | 5% |
| 18:00 | 908 | **-0.29** | 3% |
| 20:00 | 576 | +0.22 | 5% |
| 22:00-06:00 | ~1,034 | +0.43 | 8-27% |

18:00-06:00 UTC is barely breakeven with 4x the trade volume of profitable hours.

### Algorithm
Add time-of-day multiplier to entry sizing:

```
if hour_utc in blocked_hours → skip entry entirely
if hour_utc in reduced_hours → size *= 0.5
if hour_utc in boosted_hours → size *= 1.0 (default)
```

### Config (reuse existing `tod_config` structure)
```json
"momentum_tod": {
  "enabled": true,
  "blocked_hours_utc": [],
  "reduced_hours_utc": [18, 19, 20, 21, 22, 23, 0, 1, 2, 3, 4, 5],
  "boosted_hours_utc": [8, 9, 10, 11, 12, 13, 14, 15, 16, 17],
  "reduced_size_multiplier": 0.5
}
```

### Expected Impact
Halving size during 18:00-06:00 UTC reduces exposure during dead hours. ~2,500 trades at half size → ~50% fee reduction on those trades → save ~0.35 SOL on fees + reduced hard_sl exposure.

### Files to modify
- `rust/pump-quant-core/src/momentum/mod.rs` — add ToD check in `on_graduation()` before position open
- `rust/pump-quant-core/src/momentum/config.rs` — add `MomentumTodConfig` struct

---

## TASK 5: Max-Hold Exit Optimization (position.rs, mod.rs)

### Problem
184 `max_hold` trades: 59% WR but -5.63 SOL net. Winners avg +0.01 SOL, losers avg -0.09 SOL — 9x asymmetry. These are tokens held for the full 300s (5 min) max_hold window. The losers are positions that slowly bleed value but never trigger `hard_sl` (because they stay within the 10% SL band).

### Algorithm: Time-decay trailing stop
Instead of a binary max_hold wall at 300s, implement progressive tightening:

```
After 30s: activate trailing stop at max(current_trail, 8%) — default
After 60s: tighten trailing stop to max(current_trail, 5%)
After 120s: tighten trailing stop to max(current_trail, 3%)
After 180s: tighten trailing stop to max(current_trail, 2%)
After 240s: tighten trailing stop to max(current_trail, 1%)
```

Also add: **stagnation exit** — if position has 0 price movement (all samples = 0 bps) after 60s, exit immediately. Don't hold dead positions for 5 minutes.

### Expected Impact
- Losers that bleed slowly get stopped earlier (saves 4+ SOL)
- Winners that are still moving are protected by normal trailing stop
- Dead positions (0 samples for 60s) exit at probe size + minimal fees

### Config additions
```json
{
  "time_decay_trailing": {
    "enabled": true,
    "stages_ms": [30000, 60000, 120000, 180000, 240000],
    "trail_pcts": [8.0, 5.0, 3.0, 2.0, 1.0]
  },
  "stagnation_exit_ms": 60000,
  "stagnation_min_samples": 0
}
```

### Files to modify
- `rust/pump-quant-core/src/momentum/position.rs` — add time-decay trailing logic, stagnation detection
- `rust/pump-quant-core/src/momentum/mod.rs` — integrate into `on_tick()` exit evaluation
- `rust/pump-quant-core/src/momentum/config.rs` — add config structs
