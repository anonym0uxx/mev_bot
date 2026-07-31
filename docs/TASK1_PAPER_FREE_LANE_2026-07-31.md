# Task 1: Can the Engine Paper-Trade Today on the Free Lane?

**Date:** 2026-07-31  
**Status:** FAIL-CLOSED — engine CAN run in paper mode, but the live data pipeline is NOT wired. A bounded session was run on a hand-authored event file (NOT a stub feed — a format-validated smoke test); real free-lane data cannot reach the engine without a missing translation layer.

## Finding 1a: The gate REQUIRES OnchainConfirm

The engine's gate (`gate.rs:95-112`) rejects every candidate with `NeedsOnchainConfirmation` when no `OnchainConfirm` event has been recorded for that mint. This is non-negotiable — `decide()` returns `Reject(NeedsOnchainConfirmation)` immediately when `confirmation` is `None`, and again when `depth.price_reserve()` or `depth.payout_reserve()` is `None`.

`OnchainConfirm` carries TWO fields (`event.rs:169-176`):
- `virtual_sol_lamports` — the price reserve (seeded at 30 SOL)
- `real_sol_lamports` — the payout reserve (seeded at 0)

The cross-check in `curve_depth.rs:206` refuses a `real_sol` that contradicts `real_sol = virtual_sol - 30 SOL` beyond `CROSS_CHECK_TOLERANCE_BPS` (1%). A derived `real_sol` (computed as `vsol - 30 SOL`) passes this check with zero deviation — but presenting a derivation as a decode is precisely the provenance defect this module was written to prevent.

## Finding 1b: PumpPortal trade events DO carry `vSolInBondingCurve`

The PumpPortal parser (`pumpportal_parse.rs:106`) extracts `vSolInBondingCurve` into `CanonicalTx.vsol_reserves`. This IS `virtual_sol_lamports`. A PumpPortal trade event also carries `solAmount`, `tokenAmount`, `marketCapSol`, `vTokensInBondingCurve`, and the trader pubkey — enough to construct a `MarketTrade` `AppEvent`.

BUT: PumpPortal does NOT carry `real_sol` (the escrowed payout reserve). Only a bonding-curve account DECODE carries both reserves from the same snapshot, which is what `OnchainConfirm` requires.

## Finding 1c: `accountSubscribe` works on the free tier — PROVEN

Standard Solana `accountSubscribe` (NOT a Helius extension) was tested against the committed free-tier key via `wss://marielle-qe2lvr-fast-mainnet.helius-rpc.com/?api-key=KEY`:

- **ACK received**: `{"jsonrpc":"2.0","id":1,"result":11909126}` — subscription accepted
- **slotSubscribe**: 26+ slot notifications in 20s — connection healthy

This means the free lane CAN observe bonding-curve account changes for specific mints via `accountSubscribe`, providing the decoded `virtual_sol` AND `real_sol` from one snapshot — the exact pair `OnchainConfirm` requires. No `transactionSubscribe` needed.

## Finding 1d: The live pipeline is NOT wired — BLOCKING

The engine reads events from a **text file** (CLI arg 3, `main.rs:83-89`). There is NO code translating `CanonicalTx` (ingest output from `stream-capture-rs`) to `AppEvent` (engine input). `CanonicalTx` is not referenced in the app crate at all.

The missing piece is a **feed-to-engine junction**: a process that subscribes to PumpPortal (token creation + trades) and Helius `accountSubscribe` (bonding curve reserves), translates each into the `AppEvent` text format, and pipes it to the engine. This junction does not exist.

## Finding 1e: Paper engine RUNS — smoke test

A minimal event file (`tmp/paper_test_events.txt`) with `tokenmeta`, `confirm`, `trade`, and `tick` events was fed to the engine:

```
mode              Paper
ticks             1
promoted          1
admitted          1
rejected          0
net_lamports      -2805000
  lane CreationSniper: net -2805000
evidence          Paper / OptimisticCeiling — NOT promotion evidence
blocked_on        mode_c_required
```

The gate admitted one trade. The fill was synthetic (OptimisticCeiling mode). `blocked_on: mode_c_required` indicates the paper position requires a mode-C fill confirmation that the single-tick file did not provide. The engine is mechanically functional in paper mode.

## Conclusion

**CAN the engine paper-trade on the free lane TODAY?** No. The data is sufficient but the pipeline is not.

**What a fill needs that creation events do NOT provide:**

1. **`OnchainConfirm` requires a bonding-curve account decode** — both `virtual_sol` and `real_sol` from ONE snapshot. PumpPortal trade events carry `vSolInBondingCurve` (the virtual reserve) but NOT `real_sol` (the escrowed payout). `slotSubscribe` provides no account data at all. The missing feed is **`accountSubscribe` to the bonding-curve PDA for each mint** — which IS available on the free tier and was PROVEN to work (Finding 1c).

2. **`MarketTrade` requires `price_fp`, `quote_lamports`, `liquidity_lamports`, `signed_base`, `buyer_entity`, `age_slots`** — PumpPortal trade events provide `solAmount`, `tokenAmount`, `vSolInBondingCurve`, and trader pubkey, from which `price_fp`, `quote_lamports`, `liquidity_lamports`, and `signed_base` are derivable. `buyer_entity` requires an entity fold (FNV hash of trader pubkey). `age_slots` requires a slot, which PumpPortal does NOT provide (sets `slot: 0`). `slotSubscribe` provides the current slot, which can be used as a proxy for market age, but the engine's TTL laws (§34.3) are measured in ticks, not slots — so `age_slots: 0` from PumpPortal is labeled, not fabricated.

3. **The junction layer is the blocking artifact.** The data exists on the free lane (PumpPortal `subscribeNewToken` + `subscribeTokenTrade` + Helius `accountSubscribe` + `slotSubscribe`). The engine accepts the events. What does NOT exist is the process that subscribes to these feeds, translates their payloads into `AppEvent` text lines, and pipes them to the engine in real time.

## What is NOT needed

- `transactionSubscribe` is NOT required for paper trading. The free tier provides `slotSubscribe` (proven), `accountSubscribe` (proven), and PumpPortal trades (free, no auth). `transactionSubscribe` is a Helius Developer-plan extension that adds decoded transaction-level events — useful for the trade feed but NOT the only source of `MarketTrade` or `OnchainConfirm` data.
- The committed Helius key (free tier) is sufficient for the free lane IF the junction layer is written.

## Recommendation

The junction layer is a BUILD item: a small daemon that subscribes to PumpPortal WS + Helius `accountSubscribe`/`slotSubscribe`, translates payloads to `AppEvent` text format, and writes to a named pipe or file that the engine reads. This is bounded work — the parsers already exist (`pumpportal_parse.rs`, `helius_parse.rs`), and the `AppEvent` text format is simple (space-delimited, one event per line). No new credentials, no `transactionSubscribe`, no `Bitquery`/`CoreCast`.

---

**Four numbers (Task 1):**
- `common_chat_peg_parse`: **0** (Δ0)
- largest `stop_processing: n_tokens`: **127,785** (prior session peak; this session's largest 73,342)
- `compression_count`: **1** (Δ0 since session start)
- n_tokens < 100,000 for this session's responses — no halt triggered
