# Acceptance ledger (PR #13, branch task/durable-missing-history)

Single running ledger. Status words: DONE (evidence cited), PARTIAL, OPEN. Linux evidence only; nothing here is Windows durability acceptance.

## Superseded results (hard-stop break-even bug)
Before the fix, the model-managed hard stop passed `trail_bps = 0` to `protection_level_fp`, so the trail leg equalled ENTRY and
the "-35% backstop" was a break-even stop. Contract restored (not changed): `LifecycleParams::standard().hard_sl_bps` and
`Config::lc_hard_sl_bps` = 3,500; `protection_level_fp` documents `entry * (10_000 - hard_sl_bps) / 10_000`.
Superseded (do not cite): any model-managed protection/lifecycle result produced before commit 71b6ff3f that depended on a close
at/below entry, notably the `amm_path_e2e` runs showing 18 closes for 1 opened position and `protect:amm_exit:HardStop` counts
in the pre-fix run, and the older `model_managed_positions_stand_down...` slow-bleed assertion (it passed but did not isolate
the leg). Replaced by `position::tests::{the_hard_stop_level_*, hard_stop_isolated_*, the_trail_leg_is_actually_disabled_*}`.
Legacy (unmanaged) path is unedited; `legacy_unmanaged_positions_keep_their_trail_and_hard_stop_behaviour` pins it, including its
pre-existing label quirk (a legacy trail exit at/below entry is labelled HardStop).

## Items
| # | Item | Status | Evidence / gap |
|---|------|--------|----------------|
| 1 | Hard stop pinned, isolated from other triggers | DONE | position tests; mutation (restore `0`) fails 2 isolated tests |
| 2 | AMM lifecycle independent ledger | DONE | `amm_lifecycle_independent_ledger_*`; mutation (double fill record) fails it |
| 3 | Protection-gap surfacing (named, last VERIFIED mark, alert path) | DONE | `held_amm_protection_gap.rs` (4 tests) |
| 4 | Seeded daemon baseline | OPEN | |
| 5 | Real-process recovery | OPEN | |
| 6 | universe_promotable trace / Qwen eligibility | OPEN | |
| 7 | Identity-registration negative tests, mid-life discovery | OPEN | |
| 8 | Executed AMM volume to shared consumers | OPEN | |
| 9 | BUY/SELL pre-execution quote matrix, WSOL route | OPEN | |
| 10 | Hosted full-workspace CI, hashed Windows package | OPEN | |

## Stated limits
- A held-AMM mark is the pool's PRE-trade state of the observed swap and excludes that swap's own price impact. A price-moving
  swap followed by silence is not detectable until a later swap's pre-trade state, or the 60 s pricing budget raises the gap.
- Spot trigger is not an executable sell quote. AMM routing fills stay non-assessable.
- No new liquidation rule: the gap alerts; it does not close anything.
