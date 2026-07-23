# `pq-stream-capture` — live-stream `[S]` ingestion spine (WS / webhook / RPC lanes)

Push-transport sibling of [`../social-ingest-rs`](../social-ingest-rs)
(pure-std Twitch IRC) and [`../social-ingest-https-rs`](../social-ingest-https-rs)
(ureq HTTPS polling): one binary, one subcommand per lane, raw-preserving
NDJSON on stdout, diagnostics on stderr. The WebSocket client is a
**hand-rolled RFC 6455 implementation over rustls** (pure frame codec +
fragmentation reassembly, tested against the RFC vectors; SHA-1 for the
§4.2.2 handshake check implemented in ~60 lines of std) — no tokio, no serde,
no tungstenite.

## Lanes

| Subcommand | Lane | Env keys | Helius plan tier required |
|---|---|---|---|
| `helius-ws` | Enhanced WebSocket: `transactionSubscribe` (+ `accountSubscribe`, `slotSubscribe` heartbeat) | `HELIUS_API_KEY` (required, exit 3 if missing); `HELIUS_WS_URL` (optional base override, e.g. `wss://beta.helius-rpc.com`) | **Developer+** for `transactionSubscribe`; account/slot subs work on all plans |
| `pumpportal` | PumpPortal `wss://pumpportal.fun/api/data`: `subscribeNewToken` + `subscribeMigration` (+ `subscribeTokenTrade` via `--watch-file`) | none (`PUMPPORTAL_WS_URL` override for testing only) | n/a (free; one socket per process, per PumpPortal's rules) |
| `webhook-listener` | Helius **enhanced webhooks** (whale / address-activity) | `WEBHOOK_AUTH_SECRET` (required, exit 3 if missing) | **all plans** (webhooks are plan-independent) |
| `fee-sampler` | `getPriorityFeeEstimate` + `getRecentPrioritizationFees` → `fee_calibration_v1` records | `RPC_URLS` (comma-separated priority list) or `HELIUS_API_KEY` fallback; neither → exit 3 | fee API costs **1 credit/call** (any paid plan; the standard method works on any provider) |
| `selfcheck` | codec self-tests + env status (set/missing only — values never printed) | — | — |
| [`grpc-server-only/`](grpc-server-only) | LaserStream **gRPC** (Yellowstone) — SERVER-BUILD-ONLY separate crate | `HELIUS_API_KEY`, `LASERSTREAM_ENDPOINT` | **Business/Professional** (LaserStream is not on Developer) |

## Webhook reverse-proxy note

`webhook-listener` binds **127.0.0.1** and speaks plain HTTP by design:
Helius requires an `https://` webhook URL, so a TLS-terminating reverse proxy
(caddy / nginx) must forward to the loopback port. Set the same random string
as the Helius webhook `authHeader` and in `WEBHOOK_AUTH_SECRET`; wrong/missing
headers are counted 401s. The listener ACKs `200 ok` immediately after reading
the body — Helius retries only 3× before dropping a delivery — and only then
parses/emits. Deliveries are deduped by transaction signature (bounded ring,
8192).

## Server-only gRPC build

`grpc-server-only/pq-laserstream-grpc` is a **separate crate, not a workspace
member**, whose first README line says exactly what it is: it depends on the
official `helius-laserstream` SDK (tokio + tonic tree) and therefore
**requires network to crates.io — it does not compile on the laptop profile**.
Build it on the server with `cargo build --release` inside that directory. It
exists because the SDK does reconnect **with `from_slot` replay** internally;
the WS lane here can only log the gap it cannot replay
(`RESUME_NO_REPLAY` + `SLOT_GAP` sentinels).

## Raw preservation and tier discipline (binding)

* **§6.3 raw-bytes-first.** Every lane emits the vendor payload UNTOUCHED
  before any derived view: `pumpportal` embeds the frame text verbatim;
  `helius-ws` and `webhook-listener` embed the payload through the crate's
  lossless JSON round trip (raw number text and member order preserved
  byte-for-byte — `u64::MAX` rent epochs and 15-digit floats survive intact,
  fixture-proven). Derived lines (`whale`, `fee_calibration_v1`) are ADDITIVE.
* **§6.6/§28 auxiliary tier.** Helius's enhanced parse and PumpPortal's feed
  are third-party interpretation: **DISCOVERY/CORROBORATION tier only, never
  canonical truth.** Canonical facts come from raw transactions (gRPC/WS
  lanes). A `whale` line is a pointer telling the engine where to look.
* **§22 / §99 / §102.** Wall clock read once in `main.rs` and injected; every
  parser/codec/state machine is pure and fixture-tested without sockets (the
  only test sockets are webhook loopback on `127.0.0.1:0`); every buffer has
  a named, cited cap; every tunable is a named constant.
* **§18.8 loud degradation.** Missing credentials fail closed at arming
  (exit 3, never a silent retry loop); schema drift, slot gaps, staleness,
  oversize drops and auth rejects are loud stderr sentinels.

## Emission contracts

```text
{"lane":"helius_ws","recv_unix_ms":...,"sub":"transaction|account|slot","raw":<params.result untouched>}
{"lane":"pumpportal","recv_unix_ms":...,"raw":<frame payload verbatim>}
{"lane":"helius_webhook","recv_unix_ms":...,"raw":<enhanced tx object untouched>}
{"lane":"whale","recv_unix_ms":...,"sig":...,"slot":...,"ts":...,"kind":"SWAP|TRANSFER|...",
 "wallets":[...],"mints":[...],"native_moved_lamports":N,
 "largest_token_move":{"mint":...,"amount":<raw number text>}|null}
{"record":"fee_calibration_v1","unix_ms":...,"provider":<redacted host>,
 "levels":{"min":..,"low":..,"medium":..,"high":..,"veryHigh":..,"unsafeMax":..}|null,
 "recent_fees_p50":N|null,"recent_fees_p90":N|null}
```

Fee records feed `pump-quant-execution::ex_tip_compute`'s CalibrationStore at
Phase-B (SERVER_BUILD_MANIFEST §8); consumers MUST treat records older than
60 s as stale. Integer micro-lamports throughout; percentiles are nearest-rank,
integer-exact.

## Reconnect / staleness

All WS lanes: deterministic jitter-free doubling backoff (1→60 s, the suite's
standard ladder), full resubscribe on every reconnect, client ping keepalive
every 30 s (`WS_PING_INTERVAL_SECS` — Helius idle-drops at 10 min).
Staleness watchdogs force reconnect + loud log: `helius-ws` after 15 s with
no slot notification (mainnet slots tick ~400 ms), `pumpportal` after 60 s of
total silence. WS has no replay: gap width is logged on resume; replay is the
gRPC lane's job.

## Vendored dependencies (build offline)

crates.io is not reachable from the authoring environment. To build/test,
create an **uncommitted** `.cargo/config.toml`:

```toml
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "/tmp/vw/vendor"
```

Dependency versions are pinned to the same resolutions as
`../social-ingest-https-rs/Cargo.lock` (ureq 2.12.1 / rustls 0.23.39 /
webpki-roots 0.26.11 / base64 0.22.1 / getrandom 0.4.3). `Cargo.toml` +
`Cargo.lock` + this README are committed; the `.cargo/` dir and any vendor
tree are NEVER committed.

## Tests

`cargo test` — 122 tests, no network except webhook loopback: RFC 6455 codec
vectors (masked/unmasked/fragmented/control, 125/126/127 length boundaries,
adversarial truncation at every byte offset must need-more, never panic;
byte-bomb declared lengths rejected before allocation), SHA-1 RFC 3174
vectors incl. the million-'a' message, handshake accept vector, fixture-driven
classification/normalization for every lane, RPC failover state machine on a
mock transport, integer-exact percentiles, loopback webhook auth/dedupe/413
flow. `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`
both clean.
