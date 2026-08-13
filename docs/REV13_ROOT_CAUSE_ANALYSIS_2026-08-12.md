# Rev-13 Root Cause Analysis — Trail/ThesisInvalidation Interaction & Rug Detection

**Date:** 2026-08-12
**Analyst:** Principal Quant (Hermes)
**Data:** 235 trade exits, Rev-13 paper trading (rev13-entry-quality-v1)
**Tape PnL:** −1.389 SOL | WR: 21.3% (sim predicted +4.855 SOL / 43.2%)

---

## Executive Summary

Two distinct problems bleed the Rev-13 PnL, both rooted in the **SIM-LIVE TICK MISMATCH**:

| Problem | Trades | PnL | % of Losses |
|---|---|---|---|
| ThesisInvalidation dominates (err=3) | 186 | −0.379 SOL | 27% |
| RugPrecursor exits (err=1) | 22 | −0.946 SOL | 68% |
| HardStop (err=2) | 2 | −0.068 SOL | 5% |
| **Total** | **235** | **−1.389 SOL** | 100% |

The trail (200 bps) is **dead code in live** — 0 TrailingStop exits across 235 trades. ThesisInvalidation (CVD rollover OR stall timer) fires first on 78.7% of trades, before the trail ever gets tested. The sim optimized trail=200 assuming the trail would be the primary exit; in live it never fires.

---

## Problem 1: ThesisInvalidation Dominates — The Trail Is Dead Code

### The exit hierarchy (position.rs:830-905)

The per-swap exit evaluation runs in this priority order:

1. **P0 RugPrecursor** — single-swap drop ≥ 1000 bps (10%) → immediate close
2. **P1 HardStop + P4 TrailingStop** — price ≤ protection_level(peak, entry, trail, hard_sl)
3. **§24(d) IntoStrength** — buy-side burst climax while in profit
4. **P2 ThesisInvalidation** — `cvd_dead || stalled`
5. **P3 TakeProfitLadder** — partial tranches at TP multiples

The ThesisInvalidation check (P2) sits BELOW the trail check (P1) in code order. But in live, it fires FIRST because:

### Sub-mechanism A: Stall timer (stalled)

```rust
let stalled = mult > 10_000 && tick.saturating_sub(pos.last_high_tick) >= stall_window;
```

- `stall_ticks = 100` in config
- `paper_tick_period_ms = 250` → **100 ticks = 25 seconds wall-clock** in live
- In sim: 100 ticks = 100 swaps (swap-count dependent)
- For sparse memecoins (1 swap / 2-5 sec): sim gave 200-500 seconds of breathing room; live gives 25 seconds
- **The stall fires 8-20× too fast in live for sparse tokens**

A memecoin that goes up 1% then consolidates for 30 seconds (normal price action) → stall fires → ThesisInvalidation → exit at a loss. The token never gets a chance to make a new high.

### Sub-mechanism B: CVD rollover (cvd_dead)

```rust
let cvd_dead = pos.cvd_peak > 0
    && pos.cvd < pos.cvd_peak.saturating_mul(i128::from(p.cvd_hold_frac_bps)) / 10_000;
```

- `cvd_hold_frac_bps = 8000` (80%) → CVD must drop 20% from peak to trigger
- In sparse flow: a token with 5 small buys then 1 large sell → CVD collapses >20% on a single swap
- **73 of 154 ThesisInvalidation losses had MFE=0** (never went up) → these are PURE CVD rollover exits (stall requires `mult > 10_000`, which never happened)
- The 80% threshold was calibrated for dense flow where CVD is well-formed. In sparse flow, CVD is noisy and a single swap can trigger rollover.

### The interaction

```rust
if cvd_dead || stalled {  // OR — either one fires
    return Some(self.close(mint, mult, ExitReason::ThesisInvalidation));
}
```

Both are OR'd. Either one kills the position. For a sparse memecoin:
- CVD rollover fires on the first large sell (noise)
- Stall timer fires after 25 seconds without a new high (too short)
- The trail (200 bps from peak) never gets tested because ThesisInvalidation always fires first

### Evidence

| Metric | Value |
|---|---|
| TrailingStop exits (err=5) | **0** |
| ThesisInvalidation exits (err=3) | **186** (78.7%) |
| TI losses with MFE=0 (pure CVD rollover) | 73 |
| TI losses with MFE>0 (went up then killed) | 81 (avg MFE=101 bps) |
| TI wins | 32 (avg MFE=1193 bps) |
| Even winners exit on err=3, NOT err=5 | Yes — trail never fires |

The 32 winning ThesisInvalidation trades are tokens that ran FAST enough to outpace the 25-second stall timer. Their avg MFE is 1193 bps (12%) — they moved so quickly the stall never had time to fire. But they're still cut short: the trail at 200 bps should have let them ride further, but ThesisInvalidation closes them before the trail can capture the full move.

### Deepest root cause

**The sim's 1-swap=1-tick model means every swap-count-based timer is a wall-clock timer in live.** The sim calibrated `stall_ticks=100` assuming 100 swaps of breathing room. In live, it's 25 seconds regardless of swap rate. For the sparse memecoins our entry quality filter SELECTS FOR (low trade count, organic flow), 25 seconds is catastrophically short.

Simultaneously, `cvd_hold_frac_bps=8000` (80% threshold) was calibrated for dense flow. In sparse flow, CVD is inherently noisy — a single sell can collapse it >20%. The threshold is too tight for the flow regime we're trading in.

---

## Problem 2: Rug Detection — The Precursor Is Reactive, Not Predictive

### The 22 RugPrecursor exits

| Category | Count | PnL | Description |
|---|---|---|---|
| Full rugs (exit=0) | 7 | −0.675 SOL | Token went to zero; precursor sold at 0 |
| Partial rugs (MFE=0) | 13 | −0.269 SOL | Precursor caught the drop; token never went up |
| Precursor on winner (MFE>0) | 2 | −0.068 SOL | Precursor fired on a token that had profit |

### Why the precursor can't stop full rugs

The RugPrecursor mechanism (position.rs:831-837):

```rust
if prev_price_fp > 0 && price_fp < prev_price_fp {
    let drop = ((prev - price) * 10000) / prev;
    if drop >= p.precursor_drop_bps {  // 1000 bps = 10%
        return Some(self.close(mint, mult, ExitReason::RugPrecursor));
    }
}
```

The precursor fires **AFTER** the drop. It sells at the post-drop price (`mult`). For a token that gaps to zero in a single swap:
- The drop is ≥ 100% (≥ 10000 bps) → precursor fires
- But `price_fp = 0` → `mult = 0` → sell at zero → total loss

**The precursor is inherently reactive.** It cannot protect against instant rugs. The sell executes at the post-rug price, which is zero.

### Why the entry filter doesn't catch them

6 of 7 full rugs had **MFE=0** — they NEVER went up after entry. The token was already dying when the bot bought. The entry quality filter checks:

- ✅ `buy_ratio ≥ 55%` — the token had majority buys pre-entry
- ✅ `max_trade_lamports ≤ 750M` — no whale dominated the flow
- ✅ `age ≤ 300s` — the token was young enough
- ❌ **Price trend** — NOT checked. A token with 55% buy ratio but declining price is a dying token where buys are market sells being absorbed, not organic demand.

The filter checks flow composition but not price direction. A token in active decay can pass all three current checks.

### The 2 "precursor on winner" cases

Two trades had MFE > 0 when the precursor fired:
- DQCLxY: MFE=731 bps, then rug to zero (−0.065 SOL) — the precursor fired too late
- 6usocT: MFE=1584 bps, precursor at −190 bps (−0.002 SOL) — precursor fired on a normal pullback, killing a winner

The precursor at 1000 bps (10% single-swap drop) is too sensitive for tokens that are volatile but legitimate. A 10% intra-swap pullback is normal on memecoins that eventually rally.

---

## Fix Suggestion Set

### Problem 1 Fixes: Making the trail live again

#### Fix 1A: Convert stall_ticks to swap-count-aware (structural)

**Change:** Replace `tick - last_high_tick >= stall_window` with a swap-count-based stall that counts actual trades since last high, not wall-clock ticks.

```rust
// Instead of: tick.saturating_sub(pos.last_high_tick) >= stall_window
// Use: pos.trades_seen.saturating_sub(pos.last_high_trades_seen) >= stall_swaps
```

Add `last_high_trades_seen: u16` to Position, updated whenever `peak_price_fp` advances. Config changes from `stall_ticks` to `stall_swaps`. This makes the stall timer behave identically in sim and live — it counts actual market activity, not wall-clock time.

**Risk:** Low. This is the structurally correct fix. The sim already models 1-swap=1-tick, so `stall_swaps=100` in sim = 100 swaps in live. The calibration transfers directly.

#### Fix 1B: Loosen CVD threshold for sparse flow (config-only)

**Change:** `cvd_hold_frac_bps: 8000 → 5000` (80% → 50%)

A 50% threshold means CVD must drop by half from peak before triggering. This requires multiple sells, not just one large sell in sparse flow. The threshold should scale with trades_observed — tighter for dense flow (where CVD is well-formed), looser for sparse flow.

**Risk:** Medium. May let losing positions run longer. But 73 trades exited on pure CVD noise (MFE=0) — these were not real thesis changes, just noise.

#### Fix 1C: AND instead of OR for ThesisInvalidation (structural)

**Change:** `if cvd_dead && stalled` instead of `if cvd_dead || stalled`

Require BOTH CVD rollover AND stall to fire before invalidating the thesis. A position that has strong CVD but is stalling (no new high) is consolidating, not dying. A position with weak CVD but making new highs is still running. Only when BOTH signals agree does the thesis truly fail.

**Risk:** Medium-high. Loses the ability to exit quickly on a clear CVD collapse without a stall. But the current OR logic is too aggressive — it fires on noise. Could add a carve-out: `cvd_dead && cvd_drop_bps > 3000` (CVD collapsed by 70%+ = emergency override, OR still applies).

#### Fix 1D: Minimum profit floor before ThesisInvalidation (config-only)

**Change:** Add `ti_min_profit_bps: u32` — ThesisInvalidation cannot fire unless `mult < 10_000 + ti_min_profit_bps`. If the position is still profitable by more than `ti_min_profit_bps`, let the trail handle it instead.

```rust
let can_invalidate = mult < 10_000 + p.ti_min_profit_bps;
if can_invalidate && (cvd_dead || stalled) {
    // ThesisInvalidation fires
}
```

With `ti_min_profit_bps = 200`, a position up 2%+ would NOT be invalidated by CVD/stall — only the trail (200 bps from peak) can close it. This gives the trail room to actually work.

**Risk:** Low. This is a clean override that lets the trail function. Positions in profit get trail protection; only underwater positions are subject to ThesisInvalidation.

### Problem 2 Fixes: Rug detection

#### Fix 2A: Pre-entry price trend check (structural)

**Change:** Add a `price_trend_bps` field to the entry quality filter. Compute the price change over the last N swaps in the pre-entry ring. If price is declining by more than `max_decline_bps`, reject the entry.

```rust
// In gate.rs entry quality filter:
if cfg.entry_quality_filter_enable {
    // Existing checks: buy_ratio, max_trade, age
    // NEW: price trend check
    let trend = entry_price_trend_from_ring(&ring);
    if trend <= -(cfg.entry_max_decline_bps as i32) {
        return GateReject::EntryQualityFilter;
    }
}
```

Config: `entry_max_decline_bps = 500` (reject if price declined >5% in the observation window). A token with 55% buy ratio but −5% price trend is in decay — buys are being absorbed by sells, not driving price up.

**Risk:** Low. This catches the 6 full rugs that had MFE=0 (dead on arrival). May reject some tokens that dip then recover, but the entry filter already requires 8 trades observed — a dip in 8 trades is a meaningful signal.

#### Fix 2B: Adaptive precursor threshold (structural)

**Change:** Scale `precursor_drop_bps` based on the position's realized volatility. A high-volatility token should have a wider precursor threshold (15%+) to avoid firing on normal pullbacks. A low-volatility token should keep the tight 10%.

```rust
let adaptive_precursor = p.precursor_drop_bps.max(
    (p.precursor_drop_bps as u64 * pos.vol_bps / 10_000).min(2000) as u32
);
if drop >= adaptive_precursor { ... }
```

**Risk:** Medium. This adds complexity but prevents the 2 "precursor on winner" cases where a 10% pullback killed a legitimate position.

#### Fix 2C: Graduated rug exit (structural)

**Change:** Instead of a single 100% sell on precursor, execute a partial exit on first precursor signal (sell 50%), then full exit if the drop continues. This salvages value on tokens that dip violently then recover.

**Risk:** High. Adds complexity to the exit path. The precursor is meant to be an emergency exit — making it partial undermines its purpose. **Not recommended for Rev-14.**

---

## Recommended Rev-14 Changes (Priority Order)

| Priority | Fix | Type | Expected Impact |
|---|---|---|---|
| **P0** | 1A: Swap-count stall timer | Structural | Makes trail live again; fixes the core tick mismatch |
| **P0** | 1D: Profit floor before TI | Config | Ensures trail handles profitable positions; instant impact |
| **P1** | 1B: CVD threshold 80%→50% | Config | Reduces 73 noise-exits on MFE=0 positions |
| **P1** | 2A: Pre-entry price trend | Structural | Catches dead-on-arrival tokens; prevents full rugs |
| **P2** | 1C: AND for TI (with override) | Structural | Reduces false ThesisInvalidation; needs testing |
| **P2** | 2B: Adaptive precursor | Structural | Prevents premature precursor on volatile winners |

### Anti-overfitting guard

All fixes must be re-validated through the full gauntlet:
1. Re-run the corrected sim with the new exit logic
2. 4Q walk-forward on the HF dataset (33.58M trades)
3. CoV < 18%, bootstrap p < 0.001
4. Permutation test
5. Tape cross-validation against live paper trades

The swap-count stall (1A) is the highest priority because it's the structural root cause. Without it, no trail width calibration transfers from sim to live. The profit floor (1D) is the quickest win — it's a config-only change that immediately gives the trail room to function.

---

## Conclusion

The Rev-13 entry quality filter is working (1,999 rejects, 13.7%), and the winners are real (top 10 avg MFE 1241 bps, 58-62% capture rate). The bot CAN find real moves. But the exit architecture is broken in live due to the tick mismatch:

- **The trail is dead code** — ThesisInvalidation fires first on 78.7% of trades
- **The stall timer is 8-20× too fast** for sparse memecoins (25 sec vs sim's 100 swaps)
- **CVD rollover is too tight** for sparse flow (80% threshold fires on noise)
- **The rug precursor is reactive** — it can't prevent instant rugs (7 full rugs = −0.675 SOL)
- **The entry filter lacks a price-trend check** — dead-on-arrival tokens pass all current checks

The fixes are structural (swap-count stall, price-trend entry check) and config-only (CVD threshold, profit floor). The swap-count stall is the root — it makes the sim-to-live calibration transfer correctly, which is the prerequisite for any trail-width optimization to work in production.
