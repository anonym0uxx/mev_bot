# Task 2 — Data-Plane Credentials & §4.3 Ingestion Lanes — 2026-07-31

**Repository commit at start:** 98c87ce
**Session:** 20260731_140756_4fe58f60

## §4.3 Credential Ingest — Status

### 1. HELIUS_API_KEY — Enhanced-WS lane (LIVE OBSERVATION)

**The committed key (`2c32e05f-...`, docs/RPC-RATE-LIMIT-SPEC.md:113) is FREE TIER.**

Proven via live connection (NOT provider-replay):
- `pq-stream-capture helius-ws` connected to `wss://marielle-qe2lvr-fast-mainnet.helius-rpc.com`
- `slotSubscribe`: **WORKED** — 26+ slot notifications received in ~20s, slot 436435404→436435429, ~400ms cadence
- `transactionSubscribe`: **REJECTED** — `"transactionSubscribe is not available on the free plan"` (code -32600)
- HTTP RPC `getHealth`: **WORKED** — returns `{"result":"ok"}`
- Default WS base `wss://mainnet.helius-rpc.com`: **401 Unauthorized** with this key; the `marielle` endpoint URL override is required

**What this means:** The Enhanced-WS lane is BUILT, CORRECT, and FAIL-CLOSED (subscription rejection is logged loudly, not silently dropped). But `transactionSubscribe` — the subscription that feeds the pump.fun / PumpSwap event decode plane — requires **Helius Developer plan ($19/mo) or higher**. The free tier delivers only slot notifications, which are necessary for the slot-staleness watchdog but insufficient for canonical ingest.

**Required from operator:** A `HELIUS_API_KEY` on at least the **Developer plan**. The free-tier key cannot drive the transaction feed.

### 2. LaserStream gRPC — what it adds and what it costs

Per `docs/HELIUS_INTEGRATION.md` ( researched against live docs, 2026-07):

| Property | Enhanced-WS (Developer+) | LaserStream gRPC (Business+) |
|---|---|---|
| Plan | Developer ($19/mo) | Business ($499/mo) |
| Feed | `transactionSubscribe` / `accountSubscribe` / `slotSubscribe` | Yellowstone-compatible gRPC (filtered by program/account) |
| Replay | NO replay — WS disconnect = hole in the tape | ~24h `from_slot` replay window, SDK auto-reconnect resumes |
| Delivery | advisory ordering (we re-sequence in our reducer, §20) | exactly-once claims, slot notifications = safe commit cursors |
| Gap recovery | none on the WS lane; RPC polling fills gaps | SDK resume from last slot; gap > 24h → full refeed + journaled gap |
| Credits | WS streaming not credit-billed (sub-based) | 2 credits/0.1MB — scope filters tightly |

**What LaserStream adds over Enhanced-WS:** gap-free replay (the WS lane has NO replay — a disconnect is a hole that only RPC polling can partially fill), exactly-once delivery semantics, and slot-cursor-based safe-commit tracking. The canonicalizer's ≥2-feed requirement (manifest §4) is satisfied by gRPC + WS + RPC polling; without gRPC, it's WS + RPC polling (weaker gap coverage).

**Recommendation:** Prove Enhanced-WS end-to-end first (cheaper lane, Developer plan). The operator can then decide whether the gap-recovery and exactly-once semantics justify $499/mo for LaserStream — with evidence from the WS lane's observed disconnect/staleness behavior.

**LaserStream entitlement is UNVERIFIED** (open item #5). The gRPC code is built (`tools/stream-capture-rs/grpc-server-only/`, `helius-laserstream` 0.5.x SDK) but cannot be tested without a Business-plan key.

### 3. RPC_URLS — two independent providers

Not yet provisioned. The code (`tools/stream-capture-rs/src/rpc.rs`) implements EWMA-latency + consec-error health scoring with deterministic provider priority. A single Helius endpoint does NOT satisfy the "two INDEPENDENT providers" requirement — Helius rate-limits by API key across all its endpoints. A second provider (e.g., QuickNode, Triton, or `api.mainnet-beta.solana.com`) is needed for true independence.

### 4. BIRDEYE_API_KEY — token-security fields (§6.7)

The Birdeye capture lane is BUILT (`tools/social-ingest-https-rs/src/birdeye.rs`):
- `birdeye_ohlcv_1d_v1` — daily bars per mint
- `birdeye_token_overview_v1` — vendor `data` object untouched
- `birdeye_token_security_v1` — plan-tier-gated endpoint
- Fail-closed: missing `BIRDEYE_API_KEY` → exit 3
- Budget-limited via `BIRDEYE_BUDGET_PER_MIN` env var

Per §6.7 and directive §3.1: `BIRDEYE_API_KEY` is **guaranteed MISSING** — it was added after the legacy setup. Per the directive, a missing Birdeye key is **NON-BLOCKING for go-live** — the Birdeye lane fails OPEN as absence, so 1D-candle backfill and token-data enrichment simply don't populate until the key arrives.

### 5. Bitquery / CoreCast — STALE, NOT PROVISIONED

The `.env.example` references `BITQUERY_API_KEY` and `CoreCast` (lines ~70-75). Per the operator's directive: **we are NOT using Bitquery, NOT wiring CoreCast**. These are stale legacy references. The Rust workspace code does NOT reference Bitquery or CoreCast anywhere (`rust/crates/`, `tools/`). Confirmed by grep: zero matches in Rust source. The `.env.example` is stale; do not provision from it.

### 6. Social sources the code CURRENTLY expects (for later planning)

The `SocialPlatform` enum (`rust/crates/pump-quant-ingest/src/social_parse.rs:74`) defines 8 platforms:

| Platform | Source | Credential needed | Status |
|---|---|---|---|
| X (Twitter) | `twitterapi.io` stream | `TWITTERAPI_IO_KEY` | Later phase |
| TikTok | TikTok scraper | `TIKTOK_API_KEY`/`_BASE` | Later phase |
| Telegram | Telegram MTProto | (no key in code) | Later phase |
| Web (Firecrawl) | Firecrawl | `FIRECRAWL_API_KEY` | Later phase |
| Twitch | IRC lane (`tools/social-ingest-rs`) | (no key) | Later phase |
| Pump | pump.fun frontend | (no key, degraded sentinel) | Later phase |
| Aggregator (CoinGecko) | CoinGecko trending | (no key in code yet) | Later phase |
| Discord | Discord adapter | `DISCORD_USER_TOKEN` | Later phase |

**CoinGecko:** referenced in `attention.rs` as `Platform::Aggregator` — the legibility tier (§783). No CoinGecko API key is currently in the code or `.env.example`; the adapter is not yet built.

**Discord:** `DISCORD_USER_TOKEN` is listed in the directive §3.1 credential map (manifest §12). The Discord capture lane is referenced in tests (`pq-regression/src/golden_tape.rs`) but the live adapter is not yet built in the Rust workspace.

### 7. PumpPortal — CONFIRMED LIVE

PumpPortal WS lane is live (33 token-creation events in 65.8s, prior session LIVE OBSERVATION, no credential needed). Confirmed by `tools/stream-capture-rs/src/pumpportal_parse.rs` and `rust/crates/pump-quant-ingest/src/pumpportal_parse.rs`.

### 8. Operator note: live Helius key at docs/RPC-RATE-LIMIT-SPEC.md:113

**DISCRETE WORK ITEM:** The literal key `2c32e05f-ac39-4d4d-b5d9-fea06f6d7fe1` is committed at `docs/RPC-RATE-LIMIT-SPEC.md:113` and republishes on every push. When the operator provides the new Helius key, the removal of this literal from `docs/RPC-RATE-LIMIT-SPEC.md` is a discrete work item. **The agent will NOT rotate anything itself** — rotation is the operator's call (§64). The agent's role is to report the removal once the operator provides the new key and confirms the rotation.

### 9. §41 — wallet private key

**NOT REQUESTED, NOT ACCEPTED.** Per §41 and the operator's directive: signing-plane custody happens at ProbeReadinessGate, generated on THIS box, public address only. If the operator offers a wallet private key early, the agent will decline and cite §41. That is a Tier-0 custody decision and it is not the agent's to shortcut.

## Criterion 65 compliance

Every measurement in this report is classified:
- Helius WS slot notifications: **LIVE OBSERVATION** (connected to live endpoint, received real-time slot data)
- Helius `transactionSubscribe` rejection: **LIVE OBSERVATION** (live rejection from the server)
- Helius `getHealth` RPC: **LIVE OBSERVATION** (live RPC response)
- PumpPortal 33 events/65.8s: **LIVE OBSERVATION** (prior session, recorded)
- LaserStream gRPC capabilities: **PROVIDER-REPLAY** (from `docs/HELIUS_INTEGRATION.md`, researched against published docs — NOT live-tested)
- Birdeye API endpoints: **PROVIDER-REPLAY** (from code docs, not live-tested)
