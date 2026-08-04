# Task 2 — Junction Completion (2026-07-31)

## Summary

The junction crate (`pump-quant-junction`) is complete: provenance types,
CanonicalTx→AppEvent translation, accountSubscribe decoder, PumpPortal adapter,
bounded SPSC queue, wire-up binary, and overflow counter test. All four stages
committed; 21 tests pass; golden digest unchanged.

## Task 1 (blocker-2 structural fix) — commit `3ec21a9`

**Type signature chosen:** `DecodedRealSol(u64)` — a newtype with a **private
inner field**, constructable ONLY via `DecodedRealSol::from_curve(&PumpCurve)`,
which itself can only come from `pump_quant_protocol::decode_pump_curve()`
(discriminator-verified, real on-chain layout).

**Why a derived value cannot satisfy it:** `DecodedRealSol` has no public
constructor. The only way to produce one is to pass a `&PumpCurve` reference,
which only `decode_pump_curve` can create (it validates the 8-byte discriminator
and reads fields at their on-chain offsets). A `u64` computed as `vsol - 30*SOL`
is a bare `u64` — there is no `From<u64>` impl, no public `new()`, and the
field is private. The compiler rejects `OnchainConfirm { real_sol_lamports:
vsol - 30_000_000_000 }` at the type level, not at assertion time.

**Replaces the value-inequality assertion.** The old test `assert_ne!(real_sol,
vsol - 30 SOL)` would fail on live data because pump.fun maintains
`virtual_sol = 30 + real_sol` as an identity. The new test
`test_decode_completed_curve_still_decodes` documents this exact case: when
values match, the type (not the value) is the enforcement.

## Task 2(a) — stage 3 (PumpPortal adapter) — commit `3ec21a9`

Two compile errors fixed:
1. `parse_pumpportal_create` 3-arg mismatch → corrected call arity.
2. Double-wrapping `ProvenancedEvent` → `canonical_tx_to_market_trade` already
   returns `ProvenancedEvent`; removed redundant wrap.

## Task 2(b) — stage 4 (wire-up) — commit `f35665a` (interim)

- `parse_events` moved from `main.rs` to `pub mod parse` in the library crate.
  Byte-identical — the golden digest test (8/8) passes unchanged.
- `junction-run` binary created in the junction crate: drains the junction
  queue alongside `parse_events` text-file events, both feeding `Engine::tick`.
- **Golden digest unchanged:** `2_822_236_667_991_883_855` (re-pin #6).

## Task 2(c) — overflow counter test — commit `f35665a`

`test_deliberate_overrun_counter_increments_and_surfaces`:
- Fill queue to capacity (8).
- Push 20 more past capacity → all drop, each increments `overflow_stats().dropped`.
- Counter reads exactly 20; `last_drop_slot` reads the final dropped slot.
- Queue depth never exceeds capacity (8) despite overrun.
- Counter is surfaced via `overflow_stats()` — the wire-up binary prints it as
  `junction_overflow`.

## Commits this task

| Hash | Description |
|------|-------------|
| `3ec21a9` | Blocker-2 structural fix (DecodedRealSol) + stage 3 (PumpPortal adapter) |
| `f35665a` | Stage 4 wire-up + overflow counter test |

## Test results

- Junction crate: 21/21 pass (lib + bin).
- Golden digest: 8/8 pass (unchanged).
- Engine E2E: 15/15 pass.

## Four numbers (source: `llama_20260730-232216.err.log`, `agent.log`)

| Metric | Value | Δ |
|--------|-------|---|
| `common_chat_peg_parse` | 0 | 0 |
| `stop_processing n_tokens` | 67,422 | — |
| `compression_count` (agent.log, cumulative-from-log) | 222 | +5 |
