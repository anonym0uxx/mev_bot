# HELIUS INTEGRATION — product map, constitutional roles, and what is built

Researched against live docs (www.helius.dev/docs, 2026-07; re-verify at activation).
Governing laws: §6.1 (canonical raw sources), §6.3 (raw-bytes-first), §6.6 (auxiliary
intelligence), §18 (source portability — Helius is a provider, never a hard dependency).

## Product → role map (decided)

| Product | Role for Hermes | Status |
|---|---|---|
| **LaserStream gRPC** (Yellowstone-compatible; Business+ plan; ~24h `from_slot` replay; exactly-once claims) | **Primary canonical ingest** feeding our own decode plane | `tools/stream-capture-rs/grpc-server-only/` (pq-laserstream-grpc, `helius-laserstream` 0.5.x SDK — SERVER-BUILD-ONLY: crates.io unreachable from the build box) |
| **LaserStream/Enhanced WebSocket** `transactionSubscribe`/`accountSubscribe`/`slotSubscribe` (Developer+) | Secondary/fallback raw feed + dev-tier substitute; no replay — gRPC owns gap recovery | **BUILT + tested**: `pq-stream-capture helius-ws` (hand-rolled RFC6455 over rustls, reconnect, slot-staleness watchdog, raw NDJSON per §6.3) |
| **Webhooks (enhanced)** — whale / address-activity push, ≤100k addresses per webhook, confirmed-commitment, at-most-once-ish (3 retries then dropped) | **Whale-tracker lane**: DISCOVERY/CORROBORATION tier only (§6.6/§28 — Helius's parse is third-party interpretation, never canonical truth; canonical facts re-derive from raw streams) | **BUILT + tested**: `pq-stream-capture webhook-listener` (pure-std HTTP behind TLS-terminating proxy, authHeader verification, 1s-ACK-then-process, signature dedupe, raw + normalized whale NDJSON) |
| **Priority Fee API** `getPriorityFeeEstimate` + standard `getRecentPrioritizationFees` | Fee-governor input → §38 CalibrationStore records for `ex_tip_compute` | **BUILT + tested**: `pq-stream-capture fee-sampler` (versioned `fee_calibration_v1` NDJSON, integer percentiles) |
| **Standard RPC multi-provider failover** | All confirmed-state reads; deterministic provider priority + health scoring | **BUILT + tested**: `pq-stream-capture` `rpc.rs` (EWMA-latency + consec-error health, mock-transport-tested state machine) — manifest §4 |
| **Sender** (`sender.helius-rpc.com/fast`, SWQoS+Jito+Harmonic+Rakurai fan-out, tip ≥0.001 SOL Max / ≥0.000005 SWQoS-only, 0 credits, all plans) | **Primary execution egress at Phase-B** — submission is §6-adjacent (Tier-0 signing boundary), NOT ingestion; deliberately not built in this batch | Endpoints + tip-account list recorded here; client lands with manifest §6 under the signing boundary |
| **Pre-Confirmations / Shred Delivery** (Professional; scheduler-stage / raw shreds) | Optional future edge; unconfirmed-hint tier only, gated by canonical confirmation (§6.4) | Deferred — evaluate after LaserStream soak evidence |
| **Enhanced Transactions API** (legacy, 100 credits/call) | Rejected for new work — use `getTransactionsForAddress` (10cr/100tx) for wallet-history backfill | — |
| **DAS `getAsset`/`getTokenAccounts`** | Cold-path token-metadata/holder enrichment (MarketIntelCache) | Phase-B, via `rpc.rs` — trivial |
| **Parsed Streams (closed beta)** | Auxiliary cross-check of OUR decoder only; never canonical (§6.6) | Watch |
| **Gatekeeper (`beta.helius-rpc.com`)** | Faster RPC edge; `HELIUS_WS_URL`/`RPC_URLS` already accept it | Config-only |
| **Wallet API** | Whale identity/funding-provenance enrichment (§28 evaluation-record rules apply) | Deferred |

Plan reality: LaserStream mainnet gRPC needs **Business ($499/mo)** minimum; `transactionSubscribe`
WS needs Developer+; webhooks + Sender work on all plans. Streaming is billed 2 credits/0.1MB —
scope gRPC filters tightly (transactions by program/account include; pool accounts by
owner=pAMMBay…; data slices where possible).

## The two-lane canonical design (§18 portability)

Server go-live runs BOTH: LaserStream gRPC (primary; SDK auto-reconnect resumes `from_slot`,
~24h replay window, slot notifications = safe commit cursors) and the WS lane (independent
code path, Developer-tier fallback, also the failover if gRPC entitlement lapses). Both emit
raw-preserving NDJSON into the same decode plane; the canonicalizer's ≥2-feed requirement
(manifest §4) is satisfied by gRPC + WS + RPC polling. Delivery-guarantee caveat from Helius's
own docs: ordering is advisory — we sequence in our own reducer (slot, index), never trust
stream order (§20).

## Whale-webhook lane discipline

Enhanced-webhook payloads are Helius-parsed interpretations (type/source/nativeTransfers/
tokenTransfers/events.swap). They enter as **discovery/corroboration only**: candidates for
watchlist attention, wallet-cohort research enrichment. They never populate canonical trades,
never authorize, and every consumed fact must re-resolve to raw chain evidence before it
becomes canonical (§6.3/§6.4). Delivery is lossy by design (3×1s retries then dropped) —
acceptable for a corroboration lane, disqualifying for a canonical one; stated in module docs.

## Fail-closed / fail-open matrix (as built)

| Lane | Missing key/secret | Outage |
|---|---|---|
| helius-ws | exit 3 (fail-closed arming) | reconnect + staleness watchdog; staleness gates refuse decisions downstream |
| webhook-listener | exit 3 (WEBHOOK_AUTH_SECRET) | events lost upstream (Helius drops) — corroboration absence, never a halt |
| fee-sampler | exit 3 (no provider derivable) | stale calibration → §8 conservative default + no-arm |
| pumpportal | n/a (free) | reconnect; absence-tolerant |
| laserstream gRPC | exit (SDK) | SDK resume from last slot; gap > replay window → full refeed + journaled gap |

Env vars: `HELIUS_API_KEY`, `HELIUS_WS_URL` (optional), `LASERSTREAM_ENDPOINT`,
`WEBHOOK_AUTH_SECRET`, `RPC_URLS`, plus the existing capture-suite keys. Never committed.
