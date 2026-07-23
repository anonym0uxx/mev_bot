# `pq-social-capture` — Rust HTTPS `[S]` capture lanes (twitterapi / tiktok / firecrawl / pump / coingecko / birdeye)

Rust twins of the polling Python adapters in [`../social-ingest/`](../social-ingest),
one subcommand per feasible platform, all emitting the identical normalized
NDJSON contract (`normalize.py` schema + the Twitch lane's `observed_at_ns`
capture stamp) — plus one deliberate exception, the `birdeye` lane, which
shares the transport/pacer/sentinel machinery but emits MarketIntel records
(§6.7 market data, not social evidence — see its section below). Standalone Cargo project outside the workspace, sibling of the
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

## `coingecko` — the aggregator-legibility lane (LATE tier, documented API)

**What CoinGecko is for us: a legibility clock + the aggregator's own
sentiment gauge + news-tier corroboration — NEVER earliness.** A pump.fun /
letsbonk memecoin trades on-chain for hours-to-days before any aggregator
lists it; a CoinGecko listing is a LATE legibility event per the
pre-legibility doctrine (the coin has crossed the aggregator's inclusion bar:
exchange tickers, a maintained page, retail discoverability). The lane
therefore feeds WAVE-TIMING / crowd-arrival / fade context, never entry
earliness. Full research record, endpoint rationale and activation checklist:
`docs/COINGECKO_SOURCE.md`. For genuinely EARLY pool-level coverage of
pump/bonk tokens the right CoinGecko-family surface is the **GeckoTerminal
onchain API** (`/onchain/networks/solana/tokens/{mint}/pools`, free-tier
accessible) — deliberately NOT wired here because the DEX-microstructure plane
already owns pool telemetry; this lane captures only what is social-shaped.

Three modes, all pollable in ONE process sharing a global budget pacer
(same shape as `pump`'s — the lane can run slower than the budget, never
faster):

```bash
$B coingecko --trending                          # /search/trending: retail search attention
$B coingecko --category pump-fun                 # /coins/markets?category=: roster watch
$B coingecko --contract-watch mints.txt          # /coins/solana/contract/{mint} round-robin
$B coingecko --trending --category solana-meme-coins --contract-watch mints.txt \
             --interval-secs 1800 --once         # all three, one probe pass
```

| Mode | Endpoint | Event captured |
|---|---|---|
| `--trending` | `GET /search/trending` (top-15 coins + top-6 categories by search popularity, ~10 min server cache) | a coin/category entering the retail attention board (`TRENDING …`) |
| `--category <id>` | `GET /coins/markets?vs_currency=usd&category=<id>&per_page=250` | a NEW coin appearing in a category roster (`LISTED …`); real ids: `solana-meme-coins`, `pump-fun`, `letsbonk-fun-ecosystem`, `meme-token` (verify via `/coins/categories/list`) |
| `--contract-watch <mints-file>` | `GET /coins/solana/contract/{mint}` per watched mint (file format = the pump lane's) | 404 = not listed (quiet poll); first 200 = the **AGGREGATOR-LISTED** legibility event; changed `sentiment_votes_up_percentage` re-emits as `SENTIMENT …` |

### Tier / auth / limits (researched 2026-07, docs.coingecko.com)

| Tier | Auth | Root | Rate | Monthly cap |
|---|---|---|---|---|
| Keyless public | none | `api.coingecko.com/api/v3` | IP-throttled, **dynamic** ("fair access"); budget default 10 req/min | n/a |
| Demo (free) | `CG_API_KEY` → `x-cg-demo-api-key` header | `api.coingecko.com/api/v3` | ~30 req/min (varies with traffic); budget default 25 req/min | **10 000 calls/month** |
| Paid (Analyst+) | `x-cg-pro-api-key` | `pro-api.coingecko.com` | plan-dependent | plan credits; unlocks `/coins/list/new`, `/news`, onchain megafilter/trades |

Both keyless and Demo are allowed; the startup log states which is active.
The startup log also prints the **monthly-sustainable cycle** for the chosen
mode mix (10 000 calls/month ≈ one request per 260 s) — the per-minute budget
alone would burn the Demo month in under 7 hours, so slow the lane with
`--interval-secs` (clamped UP to the budget floor, never down) or
`--budget-per-min`. 429s respect `Retry-After` on the shared backoff ladder;
401/403 exits 2 loudly (`AUTH_REJECTED`); the same FNV-1a shape-hash
`SCHEMA_DRIFT` + `STATUS_CLASS_DRIFT` sentinels as `pump` watch each endpoint
family (documented API — drift expected rare, tracked anyway).

Emission: shared schema (platform `"coingecko"`, author `"coingecko"` — the
aggregator is the actor, community = category id / `"trending"` / the watched
mint, engagement zeros, `echo:false`, capture stamp) plus optional trailing
fields ONLY when the vendor stated them (§6.4 — never fabricate):
`"mint"` (base58 VERBATIM case from `platforms.solana`),
`"aggregator_listed":true`, `"sentiment_bp"` (0–10000 =
`sentiment_votes_up_percentage` × 100, rounded),
`"sentiment_conf_bp"` (CoinGecko does not expose raw vote counts, so
confidence maps the audience-size field it does publish:
**1 `watchlist_portfolio_users` = 1 bp, saturating at 10 000**) and
`"sentiment_model":"coingecko-votes-v1"`.

## `birdeye` — the REQUIRED 1D-candle backfill + token-data lane (§6.7, MARKET data)

**Constitutional status: REQUIRED source.** Amendment A-3 (constitution §6.7,
human-directed 2026-07-23) designates Birdeye Data Services the provider of
record for **1D OHLCV candle backfill/cross-check** and **token-data
enrichment for candle analysis** (§21.6 bar/market-structure family). Build
obligation: `docs/SERVER_BUILD_MANIFEST.md` §10; the §6.6 external-tool
evaluation record is `docs/BIRDEYE_SOURCE.md`. "Required" binds the BUILD,
not the trade path: Birdeye stays auxiliary intelligence, consumed only
through **MarketIntelCache**, never authority — the §6.1 prohibition on
Birdeye trade history as raw truth stands, own canonical flow remains the
PRIMARY bar source, and the lane fails OPEN as absence (an outage, 429 or
schema drift never halts, delays or degrades any strategy lane).

**This is MARKET data, not social evidence.** Unlike every other lane in this
binary, `birdeye` does NOT emit the SocialEvent NDJSON schema and never
enters the §29 social plane. It emits three MarketIntel record kinds:

| Record | Shape | Notes |
|---|---|---|
| `birdeye_ohlcv_1d_v1` | `{"record":"birdeye_ohlcv_1d_v1","mint":…,"observed_unix_ms":…,"bars":[{"t":unix_s,"o":…,"h":…,"l":…,"c":…,"v":…,"v_usd":…}],"provider":"birdeye","interval":"1D","quote":"usd"}` | every numeric value is the vendor's EXACT raw JSON token, passed through unmodified — no float math in our code, ever; `v_usd` only when the vendor stated it (§6.4); bars missing any of `unixTime`/`o`/`h`/`l`/`c`/`v` are skipped, never fabricated |
| `birdeye_token_overview_v1` | `{"record":"birdeye_token_overview_v1","mint":…,"observed_unix_ms":…,"raw":<data object untouched>}` | §6.3 raw preservation: key order, number spellings and unknown fields survive verbatim — downstream MarketIntelCache carries the full §21.6 provenance list |
| `birdeye_token_security_v1` | same shape as overview | plan-tier-gated (see below) |

Three composable watch modes on ONE process sharing the global budget pacer
(the coingecko lane's exact shape — the lane can run slower than the budget,
never faster):

```bash
export BIRDEYE_API_KEY=...   # REQUIRED — missing key exits 3 (fail-closed)
$B birdeye --ohlcv-watch mints.txt                     # /defi/v3/ohlcv, 1D range
$B birdeye --overview-watch mints.txt                  # /defi/token_overview
$B birdeye --security-watch mints.txt                  # /defi/token_security (Starter+)
$B birdeye --ohlcv-watch mints.txt --overview-watch mints.txt \
           --time-from 1750000000 --time-to 1753000000 --once   # probe pass
$B birdeye --replay tests/fixtures/birdeye_ohlcv.json
```

| Mode | Endpoint | Notes |
|---|---|---|
| `--ohlcv-watch <mints-file>` | `GET /defi/v3/ohlcv?address=<mint>&type=1D&time_from=<t0>&time_to=<t1>&mode=range` | round-robin per mint; `t0`/`t1` from `--time-from`/`--time-to` (unix secs), default the last `BIRDEYE_DEFAULT_LOOKBACK_DAYS=30` days ending now; §6.7 mandates `1D` only (own flow owns sub-daily) |
| `--overview-watch <mints-file>` | `GET /defi/token_overview?address=<mint>` | liquidity, holders, trade counts, volume, buy/sell pressure, price frames — all plan tiers |
| `--security-watch <mints-file>` | `GET /defi/token_security?address=<mint>` | **Starter+ plan only**: a 401/403 here logs ONE loud "token_security unavailable on this plan tier — omitting (never fabricated)" and disables the mode for the session (fail-open as absence); the same key keeps serving the other endpoints |

### Auth / tier / limits (researched 2026-07, docs.birdeye.so — re-verify at activation)

Every call carries `X-API-KEY: $BIRDEYE_API_KEY` + `x-chain: solana`.
**Fail-closed:** a missing key refuses to start with exit 3 (the pump lane's
distinct capability-loss code) — a §6.7 REQUIRED source must never be polled
keylessly into silent absence; a key REJECTED on ohlcv/overview
(`AUTH_REJECTED`) exits 3 too. Plan tiers (Standard/Starter/Premium/
Business) meter both request rate and **compute units per call** — the
default budget is a conservative `BIRDEYE_BUDGET_PER_MIN=30` req/min (env
var or `--budget-per-min` to override; flag wins) precisely because CU
metering makes hot pacing expensive on small plans. `--interval-secs` is
clamped UP to the budget floor, never down; 429s ride the shared backoff
ladder with `Retry-After` respected. The same FNV-1a shape-hash
`SCHEMA_DRIFT` + `STATUS_CLASS_DRIFT` sentinels as `pump`/`coingecko` watch
each endpoint family; drift is loud on stderr and the lane keeps running on
raw passthrough. Repeated identical responses are content-deduped
(observation stamp masked), so an unchanged candle set is a quiet poll.

### §21.6 reconciliation status of backfilled bars

Backfilled daily bars are **`backfill-unreconciled`** until the server's
first reconciliation epoch journals the divergence distribution of Birdeye
1D candles vs our own canonical daily aggregation on overlapping windows
(`docs/BIRDEYE_SOURCE.md`, Phase-B activation checklist #3–4). Only after
that divergence report is journaled may backfilled bars carry
`reconciliation status: cross-checked`; until then every consumer treats
them accordingly, and the §21.6 screens (missing/stale, wrong-pair/
duplicate, quote-asset distortion, artificial volume, aggregation mismatch,
look-ahead, survivorship) gate admission either way.

## Env vars

| Subcommand | Required env |
|---|---|
| `twitterapi` | `TWITTERAPI_IO_KEY` (https://twitterapi.io, pay-as-you-go) |
| `tiktok` | `TIKTOK_API_KEY` + `TIKTOK_API_BASE` (your provider endpoint) |
| `firecrawl` | `FIRECRAWL_API_KEY` (https://firecrawl.dev) |
| `pump` | none — anonymous frontend reads (tier-3; revocation = exit 3) |
| `coingecko` | `CG_API_KEY` optional — free Demo key → `x-cg-demo-api-key` header (~30 req/min, 10 000 calls/month); absent = keyless public access (IP-throttled, lower); the log states which is active |
| `birdeye` | `BIRDEYE_API_KEY` **REQUIRED** (`X-API-KEY` header; missing = exit 3 fail-closed — §6.7 required source) + `BIRDEYE_BUDGET_PER_MIN` optional (default 30; `--budget-per-min` wins) |
| `sentiment-enrich` | none required — `LLAMA_SERVER_URL` (default `http://127.0.0.1:8080`, the local llama.cpp server) + `LLAMA_MODEL_ID` (default `local-llm-v0`, the provenance tag) |

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
`--mints-file --interval-secs --live-list --once`; the twin-less `coingecko`
lane takes `--trending --category --contract-watch --interval-secs
--budget-per-min --once`; the twin-less `birdeye` lane takes
`--ohlcv-watch --overview-watch --security-watch --time-from --time-to
--interval-secs --budget-per-min --once` (mints-file format = the pump
lane's).

## Replay mode (deterministic, zero network)

```bash
$B twitterapi --replay tests/fixtures/twitterapi_pages.json
$B tiktok     --replay tests/fixtures/tiktok_feed.json
$B firecrawl  --url https://www.dexscreener.com/solana \
              --replay tests/fixtures/firecrawl_scrape.json
$B pump       --replay tests/fixtures/pump_replies.json           # bare array
$B pump       --replay tests/fixtures/pump_replies_wrapped.json   # {"replies":[...]}
$B pump       --replay tests/fixtures/pump_drift.json             # SCHEMA_DRIFT demo
$B coingecko  --replay tests/fixtures/coingecko_trending.json     # trending board
$B coingecko  --replay tests/fixtures/coingecko_contract.json     # LISTED -> SENTIMENT
$B coingecko  --category pump-fun \
              --replay tests/fixtures/coingecko_markets.json      # category roster
$B coingecko  --replay tests/fixtures/coingecko_drift.json        # SCHEMA_DRIFT demo
$B birdeye    --replay tests/fixtures/birdeye_ohlcv.json          # 1D candle record
$B birdeye    --replay tests/fixtures/birdeye_overview.json       # raw-preserved overview
$B birdeye    --replay tests/fixtures/birdeye_security.json       # raw-preserved security
$B birdeye    --replay tests/fixtures/birdeye_drift.json          # SCHEMA_DRIFT demo
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

## The brain seam — `sentiment-enrich` (LLM annotations, OFF the hot path)

`sentiment-enrich` is deliberately NOT a capture lane. It is the one seam
where the local LLM (the operator's llama.cpp server — the same
`http://127.0.0.1:8080` endpoint the supervisor's `llama_server.yaml`
describes and its `bench_endpoint` health-checks) touches the social stream,
and the architecture keeps it in its constitutional place:

```
capture (Rust/Python lanes)  →  sentiment-enrich  →  deterministic core
        [S] observations          BRAIN seam            judgment
```

- **The LLM is never a fact source or authority** (§65 criterion 8: "LLM
  output cannot enter factual state"). Its output is an ENRICHMENT annotation
  with provenance: three fields spliced into each NDJSON line —
  `"sentiment_bp"` (0–10000, 5000 = neutral, 0 = maximal bearish /
  scam-accusation, 10000 = maximal bullish), `"sentiment_conf_bp"` (0–10000)
  and `"sentiment_model"` (which model said so). Downstream these are
  corroboration-tier integers at most — evidence the core may weigh alongside
  everything else, never truth it may cite (§6.5-spirit: a research artifact
  is never cast into canonical truth; sentiment annotates social observations
  only, it can never create, confirm or veto an on-chain fact).
- **Annotations are recorded INPUTS, so replay is byte-identical.** The
  enrichment happens at the capture side of the determinism boundary and is
  written into the recorded stream; the deterministic core replays the exact
  bytes it originally saw. The nondeterministic model is quarantined at the
  seam — it can never make a replay diverge.
- **Absent sentiment is UNKNOWN, never neutral-positive (§6.4).** Server
  unreachable, timeout (5 s hard budget), non-JSON output, out-of-range
  values (rejected, NOT clamped — clamping would manufacture certainty from
  garbage): the line passes through byte-identical, unannotated. The core
  treats missing `sentiment_bp` as UNKNOWN — it must never default it to
  5000. Enrichment absence never blocks capture; the filter never drops,
  never reorders, and adds three fields or nothing.
- **The stream contract is a splice, not a re-serialization.** The original
  line bytes are preserved verbatim up to the closing brace (byte-prefix
  identical); only the three fields are inserted before the final `}`. Input
  lines over 64 KiB are streamed through untouched (bounded memory, no model
  call).
- **The request is grammar-caged.** llama.cpp's `json_schema` field
  (server-side GBNF constrained decoding — `json_schema_support: true` in the
  supervisor's server config) makes the model physically unable to emit
  anything but the two bounded integers; temperature 0, fixed seed, small
  `n_predict`, `cache_prompt` for the shared instruction prefix, one
  keep-alive connection for the whole run. Per-line latency p50/max goes to
  stderr at exit; a degradation-counter summary every 100 lines.

```bash
# The pipeline: capture | enrich | core. Delete the middle stage and the
# stream still flows — the annotation is optional by construction (§67).
$B twitterapi --sources ../social-ingest/sources.yaml --watch 5 \
    | $B sentiment-enrich \
    | cargo run --quiet --manifest-path ../social-ingest/probe/Cargo.toml

# Supervised run: enrichment failure exits loudly instead of failing open.
$B sentiment-enrich --require < capture.ndjson > enriched.ndjson

# Deterministic offline test: fixture responses instead of the network.
$B sentiment-enrich --replay tests/fixtures/sentiment_replay.json < in.ndjson

# Pipeline stub (identity filter — no server, no annotation):
$B sentiment-enrich --passthrough < in.ndjson
```

Before / after (one line; the splice is everything after `...s":42`):

```json
{"platform":"x","author":"degen","community":"","text":"send it $WIF","likes":420,"reposts":69,"replies":12,"echo":false,"observed_at_ns":42}
{"platform":"x","author":"degen","community":"","text":"send it $WIF","likes":420,"reposts":69,"replies":12,"echo":false,"observed_at_ns":42,"sentiment_bp":9100,"sentiment_conf_bp":7000,"sentiment_model":"local-llm-v0"}
```

`--replay <responses.json>` takes a JSON array of sentiment responses
consumed in order (a `null` entry simulates a failure → line unchanged; an
optional `content_hash` field is carried for fixture bookkeeping and
ignored). Exhausted fixture = absence, not an error.

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

183 tests: 141 unit (parsing, Python-coercion parity, YAML subset, dedupe
ring, backoff ladder, urlencode parity, per-platform normalizers, pump
shape-hash / challenge / status-class sentinels, pump budget-pacing math,
coingecko budget/monthly-cap math, sentiment-bp/conf-bp mappings, trending /
markets / contract normalizers, shape dispatch, optional-field emission,
sentiment splice / strict-validation / prompt-escaping / line-cap mechanics,
birdeye budget/env resolution + time-range math, bar parsing with verbatim
raw-token passthrough incl. exotic number spellings, raw-object preservation,
shape dispatch/fingerprints, content-keyed dedupe, CLI validation)
+ 42 integration (byte-exact replay per platform including both pump response
shapes, all three coingecko surfaces and all three birdeye record kinds,
replay determinism, schema-drift survival, keyless refusal with the Python
twins' exact messages, birdeye missing-key fail-closed exit 3 +
malformed/truncated-fixture error-not-panic + MarketIntel-not-SocialEvent
record-shape proof, NDJSON round-trip + schema order incl. the pump trailing
`mint` field and the coingecko optional-tail order; sentiment-enrich
never-drop/never-reorder, fail-open absence for
null/out-of-range/unreachable-server/oversize, `--require` loud exit,
passthrough byte-identity, replay byte-determinism).
Tests never touch the network — the "unreachable server" tests point at a
closed loopback port.
