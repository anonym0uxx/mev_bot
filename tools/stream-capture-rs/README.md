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
| `discord-gateway` | **Passive** Discord Gateway v10 (`wss://gateway.discord.gg/?v=10&encoding=json`): live MESSAGE_CREATE capture from the operator's paid alpha rooms, invisible presence | `DISCORD_USER_TOKEN` or `DISCORD_BOT_TOKEN` (per `--token-kind`; required, exit 3 if the selected one is missing); `DISCORD_GATEWAY_URL` (optional override, testing) | n/a (Discord, not Helius) |
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

## Discord Gateway lane (passive alpha capture)

`discord-gateway` is a **passive, read-only** Discord Gateway v10 client that
monitors paid alpha servers the operator **legitimately subscribes to** and
captures the live message push. The whole design is "behave exactly like a
normal, well-behaved client" — that posture is what keeps a legit account in
good standing; it is **not** an evasion tool.

**Passive / invisible / read-only — and why it is the safe stance.** A legit
paid account is flagged when it behaves like a *scraper*, not when it reads
messages it is entitled to. So this lane:

* sends only three Gateway ops, ever — `IDENTIFY` (op 2), `RESUME` (op 6),
  `HEARTBEAT` (op 1). It never sends a message, typing indicator, reaction, or
  any presence update beyond the single `invisible` IDENTIFY.
* makes **zero REST calls** — no message-history fetch. History scraping is
  exactly the access pattern that trips detection; we consume only the live
  Gateway push, which is what a normal client at rest does.
* identifies with `presence.status = "invisible"` — a first-class Discord
  feature, the supported "incognito" posture: the account shows offline to the
  room while still receiving messages.
* keeps the socket warm with Gateway op-1 heartbeats only. It deliberately does
  **not** send RFC 6455 WS pings (the shared client can, but this lane does not
  drive it) — a real Discord client heartbeats at the Gateway layer, so adding
  WS pings would be an atypical fingerprint. It still auto-replies to server
  pings, which is normal.

**User vs. bot token (`--token-kind user|bot`, default `user`).** Both tokens
go **raw** into `IDENTIFY.token` for the Gateway — the `Bot ` prefix is only for
REST `Authorization` headers, which this lane does not use. In practice a **bot
usually cannot be added to a paid alpha room** (that needs the server owner's
invite/Manage-Server), so capturing a room the *operator* pays for is typically
a **user token**. `MESSAGE_CONTENT` is a privileged intent for bots but is
present-by-default for user tokens. Choosing `bot` reads `DISCORD_BOT_TOKEN`;
`user` reads `DISCORD_USER_TOKEN`; the selected-but-missing token is fail-closed
exit 3.

**Config — allowlist and callers.** Only the operator's rooms are captured;
everything else is dropped before emit.

* `--guilds id,id` / `--channels id,id` — allowlisted server / channel ids. An
  empty dimension imposes no constraint on that dimension, so configure at
  least one.
* `--callers id,id` — author ids of the high-signal alpha callers; their
  messages are tagged `"is_designated_caller":true`.
* `--allowlist-file f` — one entry per line, `guild:<id>` / `channel:<id>` /
  `caller:<id>`, `#` comments allowed; composes with the CLI flags.
* `--client-os` / `--client-browser` / `--client-device` — the IDENTIFY
  `properties` fingerprint (default `Windows` / `Discord Client` / `desktop`, a
  plausible normal-client identity, not a spoof of a specific victim).
* `--heartbeat-jitter-seed N` — optional deterministic first-heartbeat jitter
  (see "Reconnect / staleness").

**Env:** `DISCORD_USER_TOKEN` **or** `DISCORD_BOT_TOKEN` (per `--token-kind`);
`DISCORD_GATEWAY_URL` overrides the endpoint for local testing only. Tokens are
read from env, never flags, and are **never printed** (including by
`selfcheck`, which reports only set/missing).

**What this deliberately does NOT do (out of scope by design):**

* **No REST history scraping** — no `GET /channels/:id/messages`, no backfill;
  live Gateway push only.
* **No multi-account rotation** — one process, one account, one socket.
* **No proxy cycling / IP rotation / evasion** of any kind.
* **No fake activity** — no typing, no reactions, no presence churn, no
  visible online status. Invisible and silent is the entire posture.

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
{"lane":"discord","recv_unix_ms":...,"raw":<MESSAGE_CREATE `d` payload untouched>}
{"lane":"discord_alpha","platform":"discord","guild_id":...,"channel_id":...,"author_id":...,
 "author":<username>,"community":<channel or guild id>,"content":<msg text>,
 "is_designated_caller":true|false,"ts":<snowflake→unix ms>,
 "cashtags":["WIF",...],"mints":["<base58 pubkey>",...]}
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

`discord-gateway` runs the **Gateway** heartbeat/resume state machine on top of
the same reconnect ladder: it reads `heartbeat_interval` from HELLO (op 10),
beats with op 1 carrying the last sequence `s`, tracks HEARTBEAT_ACK (op 11) and
force-reconnects a **zombie** connection (a heartbeat un-ACKed for `interval *
1.5`); it resumes with op 6 on op 7 RECONNECT / op 9 resumable and re-IDENTIFYs
on op 9 non-resumable, and its staleness watchdog fires after 120 s with no
Gateway frame at all. First-heartbeat jitter: the Discord docs jitter the first
beat by `interval * random[0,1)` to de-sync fleets; this crate is
RNG-free/deterministic, so the default sends the first beat at the full interval
(the natural cadence boundary — safe for a single client) and
`--heartbeat-jitter-seed N` gives a deterministic fraction `N/1000` of the
interval instead.

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

`cargo test` — 182 tests, no network except webhook loopback: RFC 6455 codec
vectors (masked/unmasked/fragmented/control, 125/126/127 length boundaries,
adversarial truncation at every byte offset must need-more, never panic;
byte-bomb declared lengths rejected before allocation), SHA-1 RFC 3174
vectors incl. the million-'a' message, handshake accept vector, fixture-driven
classification/normalization for every lane, RPC failover state machine on a
mock transport, integer-exact percentiles, loopback webhook auth/dedupe/413
flow. The Discord Gateway lane adds 48 tests: opcode/dispatch routing, the
IDENTIFY bitmask (`33281`)/invisible-presence/raw-token shape, RESUME-vs-
re-IDENTIFY on op 7 vs op 9, snowflake→unix decode, heartbeat seq + zombie
deadline (`interval * 1.5`), allowlist filtering, designated-caller tagging,
cashtag+mint extraction, dedupe idempotency, the exact `discord_alpha` key set,
missing-token→exit-3, and frame-parser fuzz (truncated/garbage never panics).
`cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` both clean.
