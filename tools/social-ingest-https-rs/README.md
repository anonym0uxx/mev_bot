# `pq-social-capture` — Rust HTTPS `[S]` capture lanes (twitterapi / tiktok / firecrawl / pump)

Rust twins of the polling Python adapters in [`../social-ingest/`](../social-ingest),
one subcommand per feasible platform, all emitting the identical normalized
NDJSON contract (`normalize.py` schema + the Twitch lane's `observed_at_ns`
capture stamp). Standalone Cargo project outside the workspace, sibling of the
dependency-free Twitch lane [`../social-ingest-rs`](../social-ingest-rs).
**One dependency: `ureq` (rustls TLS).** No tokio, no serde, no hyper — JSON is
hand-rolled in the same audited style as the Twitch lane's `emit.rs`.

## Scrutiny — every Python adapter, judged

| Adapter (Python) | Protocol | Auth (env — never hardcoded) | SDK dependence | Latency profile | Rust verdict |
|---|---|---|---|---|---|
| `twitterapi_stream.py` | REST GET polling (`api.twitterapi.io` advanced_search, cursor pagination) | `TWITTERAPI_IO_KEY` (`X-API-Key` header) | none (stdlib urllib; optional pyyaml) | **poll-cadence-dominated** (5–10 s watch vs ~ms parse) | **feasible-now → `twitterapi` subcommand** |
| `tiktok_stream.py` | REST GET polling (provider-agnostic hashtag feed) | `TIKTOK_API_KEY` + `TIKTOK_API_BASE` (`Authorization: Bearer`) | none (stdlib urllib) | poll-cadence-dominated (60 s watch) | **feasible-now → `tiktok` subcommand** |
| `firecrawl_stream.py` | REST POST (`api.firecrawl.dev/v1/scrape`) | `FIRECRAWL_API_KEY` (`Authorization: Bearer`) | none (stdlib urllib) | poll-cadence-dominated (120 s watch; scrape itself seconds-slow server-side) | **feasible-now → `firecrawl` subcommand** |
| `telegram_stream.py` | **MTProto push stream** (Telethon; edits/deletions as D6 signals) | `TELEGRAM_API_ID/HASH/SESSION` | **heavy** (Telethon; Rust equivalent = grammers, a large async SDK) | true real-time push — the one lane where transport latency IS the profile | **infeasible-without-heavy-SDK → stays Python (PRIMARY)**, measurement-gated future option |

Telegram in Rust would drag in grammers + tokio + an MTProto crypto stack —
violating this lane's minimal-dependency rule for the platform where the
*Python* implementation is already push-based and upstream-fastest. It stays
Python behind the same NDJSON contract.

## Why `ureq` + rustls, and nothing else

- The Twitch lane proved the pattern: plain TCP ⇒ zero deps. These lanes need
  HTTPS; hand-rolling TLS is a non-starter, so the minimal honest step up is
  **one** small blocking HTTP client with rustls (no OpenSSL linkage, no async
  runtime). `default-features = false, features = ["tls", "proxy-from-env"]`
  drops ureq's gzip lane and matches `urllib`'s env-proxy behavior.
- No serde: emission and parsing are the same audited hand-rolled JSON style as
  `../social-ingest-rs/src/emit.rs` (§67 — the adapter stays removable and
  auditable end-to-end).

## What Rust honestly buys here (no overclaiming)

These lanes are **poll-cadence-dominated**: end-to-end capture latency is set
by the watch interval and the vendor's HTTP latency, which no language change
fixes. What the Rust lane removes is everything *around* that: interpreter
startup, GC pauses and allocator jitter on the parse path (tail-latency
determinism for the Signal-Horizon stamp), and per-request TCP+TLS
re-handshakes — one shared `ureq` Agent keeps connections warm across polls,
where `urllib.request` reconnects every time. It also gives the capture edge
the same crash-free, bounded-memory, `-D warnings` discipline as the rest of
the Rust surface. That is the honest win: **tighter, more deterministic
capture stamps and a hardened edge — not a faster vendor**.

## Constitution discipline (binding)

- **§22 determinism boundary** — wall clock read in exactly one function
  (`main.rs::now_ns`); `--replay` is a pure function of the fixture file
  (synthetic monotone timestamps, zero network), byte-stable run-to-run.
- **§29 provenance** — platform/author/community carried verbatim; trust is
  earned downstream in the D-ledger, never at the capture edge.
- **§29.7e sacrificial identity** — credentials come ONLY from the same env
  vars the Python twins use; pay-as-you-go research keys, never an operator's
  personal account. Missing keys refuse to start (Python twins' exact stderr).
- **§67 removable adapter** — one binary, one dependency, stdout is NDJSON
  exclusively, stderr is diagnostics exclusively. Delete it and the system
  loses the Rust HTTPS lanes; the Python twins still work.
- **§83** — no sentiment, no opinion, no decision. Capture only.

## Zero functional regression vs the Python twins

Same endpoints, same query parameters (`urlencode`/`quote_plus` replicated
byte-for-byte), same `sources.yaml` keys and watchlists, same filter classes,
same cursor/since-id semantics, same cadence defaults, same env vars, same
stderr messages, same dedupe-by-id behavior (hardened: bounded 65 536-id ring
instead of an unbounded set), and — proven by byte-exact tests generated from
the *actual* Python `normalize.py` — identical NDJSON output per platform,
plus the capture stamp. Python-value coercion (`max(0, int(x))` clamping,
truthiness chains, char-not-byte text caps, `str(float)` stderr formatting) is
mirrored in `src/json.rs` and unit-tested.

Hardening added on top (all subcommands): connect/read/write timeouts, one
shared Agent (connection reuse), bounded jitter-free exponential backoff
(1→2→4…60 s, deterministic steps) for HTTP 429/5xx/transport errors with
`Retry-After` respected, 8 MiB response-size cap, malformed-JSON skip (never a
panic), stdout line-buffered NDJSON only.

## `pump` — the Pump.fun-native lane (tier-3, degradation-sentineled)

The one lane with **no Python twin and no vendor**: pump.fun has NO official
public data API, so this subcommand polls the same undocumented frontend feed
the pump.fun web UI uses —
`GET https://frontend-api-v3.pump.fun/replies/{mint}?limit=50&offset=0&reverseOrder=true`
per watched mint, plus (with `--live-list`) `GET /coins/currently-live` once
per 60 s for stderr liveness logging. That makes it a **tier-3 source** by this
project's acquisition hierarchy: reverse-engineered, unversioned-for-us,
Cloudflare-fronted, and historically churny — the host has already walked
`frontend-api` → `frontend-api-v2` → `frontend-api-v3`, and each hop broke
unofficial consumers overnight. **Anonymous-read status from a datacenter IP is
UNVERIFIED until Phase-B server probing** — residential-browser behavior does
not transfer.

```bash
$B pump --mints-file mints.txt              # one base58 mint per line, # comments ok
$B pump --mints-file mints.txt --live-list  # + currently-live logging (1 req/min reserved)
$B pump --mints-file mints.txt --once       # single probe pass (Phase-B checklist)
$B pump --replay tests/fixtures/pump_replies.json
```

Emission: the shared NDJSON schema (platform `"pump"`, author = replying
wallet **lowercased**, community = the coin's mint **verbatim base58 case** —
mints are case-sensitive, engagement zeros, `echo:false`, capture stamp) plus
ONE extra trailing field `"mint"`: the thread's mint. The thread context IS a
mint-grade coin reference — stronger than any ticker in the text — so it is
carried explicitly. Dedupe is by reply id on the shared bounded ring; both
response shapes the endpoint has shipped (bare array and `{"replies":[...]}`)
are handled; unknown fields are tolerated; malformed entries are skipped;
an **empty array is a quiet poll, never an error**.

### Degradation sentinel (first-class for an undocumented endpoint)

| Signal | Detection | Reaction |
|---|---|---|
| `SCHEMA_DRIFT` | FNV-1a hash over the sorted top-level key names of the first reply object changed | stderr log with old/new hash; tolerant parser keeps running |
| `STATUS_CLASS_DRIFT` | per-endpoint HTTP status class changed (2xx→5xx, …) | stderr log; poll skipped, cadence continues |
| `CHALLENGE_WALL` | HTML content-type / HTML body / Cloudflare challenge markers where JSON belongs (checked BEFORE the auth wall — Cloudflare serves its challenge WITH a 403) | stderr log; back off 5 minutes; keep running |
| `AUTH_WALL` | 401/403 without a challenge page = anonymous reads revoked | stderr log; **exit code 3** (distinct — the supervisor must see the capability loss loudly) |

### Request budget

Hard global budget: **≤ 20 requests/minute across ALL watched mints**,
round-robin. `--interval-secs` (seconds per full cycle) defaults to the budget
floor `ceil(n_mints * 60 / 20)` and is **clamped up** to it — the lane can run
slower than the budget, never faster. `--live-list` reserves 1 req/min for the
currently-live poll (floor becomes `ceil(n * 60 / 19)`). Ten watched mints =
one 30 s cycle = a 3 s gap between requests.

### Phase-B activation checklist

1. **Probe anonymous GET from the server IP first**: `pump --mints-file probe.txt --once`
   with 1–2 mints; healthy = NDJSON or a quiet pass, exit 0.
2. Exit 3 (`AUTH_WALL`) on the probe = anonymous reads are revoked for
   datacenter IPs → do NOT activate the lane.
3. `CHALLENGE_WALL` on the probe = Cloudflare challenges the server IP → treat
   as unavailable from this host; try again later before concluding.
4. Watch the first hour of stderr for `SCHEMA_DRIFT` (fixture shapes are from
   the reverse-engineered spec, not a contract).
5. JWT fallback (authenticating with a sacrificial pump.fun account to survive
   an auth wall) is **documented as NOT implemented** — a deliberate Phase-B+
   decision point, not an oversight. See `docs/PUMP_NATIVE_INTELLIGENCE.md`.

## Env vars

| Subcommand | Required env |
|---|---|
| `twitterapi` | `TWITTERAPI_IO_KEY` (https://twitterapi.io, pay-as-you-go) |
| `tiktok` | `TIKTOK_API_KEY` + `TIKTOK_API_BASE` (your provider endpoint) |
| `firecrawl` | `FIRECRAWL_API_KEY` (https://firecrawl.dev) |
| `pump` | none — anonymous frontend reads (tier-3; revocation = exit 3) |

## Usage

```bash
cd tools/social-ingest-https-rs
cargo build --release
B=./target/release/pq-social-capture

# X firehose, KOL amplifier watch, curated list — same classes as the Python:
export TWITTERAPI_IO_KEY=...
$B twitterapi --class firehose --sources ../social-ingest/sources.yaml --watch 5 \
    | cargo run --quiet --manifest-path ../social-ingest/probe/Cargo.toml

# TikTok hashtag feed (provider-agnostic):
export TIKTOK_API_KEY=... TIKTOK_API_BASE=https://<provider>/tiktok/hashtag
$B tiktok --sources ../social-ingest/sources.yaml --watch 60

# Firecrawl web-legibility clock:
export FIRECRAWL_API_KEY=fc-...
$B firecrawl --url https://www.dexscreener.com/solana --watch 120

# Fuse with the Twitch Rust lane and the Python Telegram lane — everyone
# speaks the same NDJSON contract:
{ $B twitterapi --sources ../social-ingest/sources.yaml --watch 5 & \
  ../social-ingest-rs/target/release/pq-twitch-capture pumpwatch & \
  python3 ../social-ingest/telegram_stream.py ; } \
    | cargo run --quiet --manifest-path ../social-ingest/probe/Cargo.toml
```

Flags mirror the Python twins exactly: `twitterapi` takes
`--class firehose|amplifier|list --sources --query --type Latest|Top --pages
--watch`; `tiktok` takes `--hashtag --sources --watch`; `firecrawl` takes
`--url --sources --watch`. The twin-less `pump` lane takes
`--mints-file --interval-secs --live-list --once`.

## Replay mode (deterministic, zero network)

```bash
$B twitterapi --replay tests/fixtures/twitterapi_pages.json
$B tiktok     --replay tests/fixtures/tiktok_feed.json
$B firecrawl  --url https://www.dexscreener.com/solana \
              --replay tests/fixtures/firecrawl_scrape.json
$B pump       --replay tests/fixtures/pump_replies.json           # bare array
$B pump       --replay tests/fixtures/pump_replies_wrapped.json   # {"replies":[...]}
$B pump       --replay tests/fixtures/pump_drift.json             # SCHEMA_DRIFT demo
```

A fixture is a saved sequence of raw API responses (one JSON value per poll —
compact or pretty-printed). Replay runs the identical parse/dedupe path with a
fixed synthetic clock (`1_000_000_000 + n·1_000_000` ns — the Twitch lane's
constants), so output is byte-identical on every run (§22). No socket is ever
opened; the integration tests assert byte-exact equality against expected
lines generated by running the actual Python `normalize.py` over the same
fixtures. Example line:

```json
{"platform":"x","author":"cryptoKOL","community":"","text":"send it $WIF EPjF...","likes":420,"reposts":69,"replies":12,"echo":false,"observed_at_ns":1000000000}
```

## Python twins remain

The Python adapters in `../social-ingest/` are NOT deleted and must keep
working: they are the executable reference for these lanes' semantics, the
fallback when a vendor changes shape faster than a Rust release cycle, and the
only implementation for Telegram (PRIMARY there). Any behavior divergence
between a Rust lane and its Python twin is a bug in the Rust lane.

## Building

Standard Cargo build against crates.io with the committed `Cargo.lock` pinning the
exact dependency tree (30 packages: ureq + rustls and their transitive closure).
The build sandbox this lane was developed in had no registry egress, so it was
originally verified against a vendored tree; the vendored copy is deliberately NOT
committed (37 MB of third-party source does not belong in the repo). `cargo build
--release` on any normal machine reproduces the pinned tree byte-for-byte.


## Verify

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

87 tests: 73 unit (parsing, Python-coercion parity, YAML subset, dedupe ring,
backoff ladder, urlencode parity, per-platform normalizers, pump shape-hash /
challenge / status-class sentinels, pump budget-pacing math) + 14 integration
(byte-exact replay per platform including both pump response shapes, replay
determinism, schema-drift survival, keyless refusal with the Python twins'
exact messages, NDJSON round-trip + schema order incl. the pump trailing
`mint` field). Tests never touch the network.
