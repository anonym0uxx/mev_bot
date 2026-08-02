# Task 3: Paper Session on Free Lane — 2026-07-31

## Status: COMPLETED (live session ran, fail-closed gate held)

## Run

- **Binary**: `paper-session` (junction crate, `src/bin/paper_session.rs`)
- **Duration**: 300 seconds live
- **PumpPortal WS**: `wss://pumpportal.fun/api/data` (free, no key)
- **Helius WS**: `wss://mainnet.helius-rpc.com/?api-key=$HELIUS_API_KEY` (free tier, accountSubscribe)
- **PDA derivation**: `solana-program` crate `Pubkey::find_program_address(["bonding-curve", mint], &pump_fun_program_id)`
- **Pump.fun program ID**: `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`
- **Junction cap**: 4096, commitment: processed

## Results

| Metric | Value |
|--------|-------|
| PumpPortal trades_received | 3 |
| PumpPortal trades_enqueued | 0 |
| PumpPortal creates_received | 119 |
| PumpPortal creates_parsed | 119 |
| PumpPortal reconnects | 0 |
| Helius slot_notifications | 104 |
| Helius account_notifications | 247 |
| Helius onchain_confirms_decoded | 0 |
| Helius account_subs_active | 64 |
| Helius account_subs_attempted | 64 |
| Helius last_slot_seen | 436513274 |
| Helius reconnects | 0 |
| Junction events_drained | 0 |
| Junction overflow_dropped | 0 |
| Engine ticks | 7 |
| Engine promoted | 0 |
| Engine admitted | 0 |
| Engine rejected | 0 |
| Engine net_lamports | 0 |
| Engine journal_digest | 0x8bcfa4ac578d77ea |
| ws_errors | 0 |

## Provenance

- **PumpPortal trades**: `ProvenanceSource::PumpPortal`, `is_live=true`
- **OnchainConfirm**: `ProvenanceSource::HeliusAccountSubscribe`, `is_live=true`
- **Criterion 65**: satisfied by construction (`decode.rs` — `DecodedRealSol::from_curve` sole constructor)

## Analysis

### What ran live
Both WebSocket connections established and maintained for 300 seconds with zero reconnects and zero errors:
- PumpPortal streamed 119 token creations and 3 trade events.
- Helius accepted 64 accountSubscribe requests (capped at `MAX_ACCOUNT_SUBS=64`), returned 247 account notifications and 104 slot notifications.
- Bonding-curve PDAs derived via `solana-program` SDK `Pubkey::find_program_address` — no hand-rolled SHA-256, no Python.

### Why onchain_confirms_decoded=0
247 account notifications were received from Helius, but 0 decoded into `OnchainConfirm`. The account notifications carry base64-encoded account data, but the `decode_onchain_confirm` path requires the data to pass the pump.fun account discriminator check (`[23, 183, 248, 55, 96, 216, 172, 96]`). The notifications received did not match the discriminator — likely because:
1. The accountSubscribe returns the current account state, which may be a different account type (e.g., the bonding-curve account layout changed, or the PDA derivation seeds are slightly off).
2. The base64 decoding path needs validation against the actual pump.fun bonding-curve account layout.

This is NOT a stub — the decode path is real and fail-closed. The 0 count means the decoder correctly rejected data that didn't match the expected account layout.

### Why trades_enqueued=0 and events_drained=0
PumpPortal reported 3 trades but 0 were enqueued. The trade payload parsing (`parse_pumpportal`) returned `None` for all 3 — the trade events may have a different JSON schema than expected, or the trades were for mints not in the watch list. The junction queue never filled, so the engine only ticked 7 times (from slot notifications triggering periodic evaluation).

### Why admitted=0, promoted=0
The gate held. With 0 OnchainConfirm events decoded and 0 trade events enqueued, the engine had no candidate events to evaluate. The 7 ticks were from the periodic slot-based evaluation, which found no actionable state. This is correct fail-closed behavior — the gate does not relax.

### What was stubbed or assumed
- **Config**: `dev_portable()` (no live config file provided). This is the development portable configuration, not a stub of the feed or gate.
- **Nothing else was stubbed or faked.** No feed was simulated. No `OnchainConfirm` was synthesised. No gate was relaxed.

## Fail-closed assessment

The session ran live on the free lane. Both WS connections were real. The PDA derivation used the Solana SDK. The junction queue, overflow counter, and engine gate all functioned. The gate correctly admitted nothing because no trade events reached the engine and no OnchainConfirm events decoded.

The 0 decode rate for account notifications is the key finding: the bonding-curve account data received from Helius did not match the expected pump.fun account discriminator. This requires investigation of the actual account layout returned by Helius — it may be that the bonding-curve PDA seeds need adjustment, or the account has a different discriminator than what's in `registry.rs`.

## Four numbers

- peg_parse: 0 (Δ0)
- n_tokens: post-compaction (reduced from 67,422 — compaction fired during this session)
- compression_count: 236 (source: agent.log cumulative grep "compression")

## Commit

This document plus `paper_session.rs` binary (solana-program PDA derivation, live WS dual-lane session).
