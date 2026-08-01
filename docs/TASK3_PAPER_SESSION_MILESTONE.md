# Task 3: Paper Session — Milestone Result

**Date**: 2026-07-31  
**Commit**: (this commit)  
**Session**: 300s live on free lane (PumpPortal + Helius free tier)

## Summary

The paper session ran 300 seconds live on the free lane with both WebSocket
connections active. **189 OnchainConfirm events were decoded from real Helius
account snapshots** — a 100% decode rate (189/189 account notifications matched
the pump.fun bonding-curve discriminator and decoded successfully).

## Key Results

| Metric | Value |
|--------|-------|
| PumpPortal trades received | 3 |
| PumpPortal trades enqueued | 0 |
| PumpPortal creates received | 58 |
| PumpPortal creates parsed | 58 |
| Helius slot notifications | 136 |
| Helius account notifications | 189 |
| OnchainConfirm decoded | **189** (100%) |
| PDAs derived | 58 |
| PDA venue-supplied addresses | 0 (not in PumpPortal payload) |
| PDA venue matches | 0 (N/A — no venue address to compare) |
| Account subs active | 58 |
| Account subs evicted | 0 (under cap of 64) |
| Overflow drops | 0 |
| Junction events drained | 189 |
| Engine ticks | 6 |
| Gate: promoted/admitted | 0 / 0 |
| Net lamports | 0 |
| WS errors | 0 |
| Reconnects | 0 |
| Last slot seen | 436,542,362 |

## PDA Derivation

Used `solana_program::Pubkey::find_program_address` with seeds
`[b"bonding-curve", mint_pubkey]` and program ID
`6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`.

This is the verified, mainnet-tested implementation from the `solana-program`
crate (v2.1). No hand-rolled SHA-256, no custom PDA helper. The protocol crate
(`pump-quant-protocol`) does not contain a PDA derivation helper — it deals
with account decoding, not address derivation.

**Correctness confirmed**: 189/189 account notifications had `owner =
6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` (pump.fun program) and
discriminator `[23, 183, 248, 55, 96, 216, 172, 96]` — the pump.fun
bonding-curve account discriminator. A wrong PDA would have subscribed to a
different account with a different owner/format, and the decoder would have
rejected it.

## Subscription Bound

MAX_ACCOUNT_SUBS = 64, FIFO eviction. 58 subscriptions were made (one per
create event from PumpPortal), all under the cap. Zero evictions occurred.
The subscription set is bounded; the decode map (`server_sub_to_mint`) retains
entries for evicted subscriptions so late-arriving notifications remain
decodable, but the map size is bounded by total subs attempted in the session,
not unbounded.

## Venue-Supplied Address Cross-Check

PumpPortal's create payload does NOT carry a bonding-curve address. The
`pumpportal_parse.rs` module has no "bonding" or "curve" fields. Therefore the
cross-check (assert derived PDA == venue-supplied address) cannot be performed.
PDA correctness is instead confirmed by the 189/189 discriminator match rate.

## Race Condition Fix

Helius sends the first account notification immediately upon subscription,
sometimes before the Ack that maps `server_sub_id → req_id → mint`. The
`extract_account_data` function was also looking for `result.account.data`
but the actual Helius notification structure is `result.value.data[0]` with
slot at `result.context.slot`. Both bugs were fixed.

## Gate Behavior

The gate held fail-closed: 0 promoted, 0 admitted, 0 net lamports. This is
correct behavior for a paper session with `Config::dev_portable()` — the
universe filter and gate thresholds are set conservatively. No check was
relaxed to produce a fill.

## Stubbed or Assumed

- `Config::dev_portable()` — no live config file provided. This is the only
  assumption. All WS connections, PDA derivation, decoding, and gate logic
  are real.

## Provenance

- PumpPortal trades: `ProvenanceSource::PumpPortal`, `is_live=true`
- OnchainConfirm: `ProvenanceSource::HeliusAccountSubscribe`, `is_live=true`
- Criterion 65 (provenance on every record): satisfied by construction
  in `decode.rs`
- PDA derivation: `solana_program::Pubkey::find_program_address` (verified,
  mainnet-tested)

## Four Numbers

- `common_chat_peg_parse`: 0 (Δ0)
- `stop_processing n_tokens`: post-compaction (compaction fired mid-session)
- `compression_count`: 47 (source: agent.log cumulative grep
  "compression_attempt"; Δ+2 since last report)
- `max_turns` effective: **150** (config NOT raised to 250 — see below)

## max_turns Finding

The config file shows `max_turns: 150`. The agent log confirms every session
ends with `max_iterations_reached(150/150)`. The raise to 250 did NOT take
effect. This is why the tool budget keeps expiring — the binding limit is 150,
not 250.
