# CoinGecko — the aggregator-legibility + crypto-news source

## What CoinGecko is for Hermes (and what it is NOT)

CoinGecko is a **LATE-tier** source by construction (§783 one-step-ahead
doctrine): by the time a memecoin is listed, trending, or categorized there,
the whole market can see it — earliness is gone. Hermes therefore consumes it
as three things, none of which is ever earliness or authority:

1. **The legibility clock** — the `aggregator_listed` input of
   `nv_pre_legibility` (previously hardcoded `false`, now live): a watched
   pump/bonk coin appearing on CoinGecko CUTS its pre-legibility earliness
   bonus in the attention model. Reduce-only, one-way.
2. **Aggregator sentiment corroboration** — `sentiment_votes_up_percentage`
   and community data mapped into the normalized `sentiment_bp` /
   `sentiment_conf_bp` fields (`sentiment_model: "coingecko-votes-v1"`),
   consumed under the same §6.4 unknown-stays-unknown / reduce-only laws as
   the LLM brain seam.
3. **News/trending-tier narrative corroboration** — trending and category
   observations feed the narrative plane at the Web/Aggregator horizon tier
   (rank 3): corroboration that can raise rank at the gate, never authorize.

## Acquisition (verified against docs.coingecko.com, 2026-07)

| Endpoint | Tier | Our use |
|---|---|---|
| `GET /search/trending` | free/Demo | retail search-attention board (15 coins + categories) |
| `GET /coins/solana/contract/{mint}` | free/Demo | **listing detection by mint** for watched coins + sentiment votes + community data |
| `GET /coins/markets?category=<id>` | free/Demo | category roster (e.g. Solana meme coins); NEW roster entries = listing events |
| `/onchain/...` (GeckoTerminal) | free tier available | earlier pool-level coverage — OVERLAPS canonical on-chain ownership; not consumed (LaserStream/Helius own those facts) |

Auth: `CG_API_KEY` env (Demo key, `x-cg-demo-api-key`, root `api.coingecko.com`);
keyless public access allowed with lower limits. Budget-paced to respect the
~30 calls/min Demo ceiling across all modes. This is a DOCUMENTED public API —
the stability outlook is far better than the pump.fun frontend tier — but the
shape-hash drift sentinel still runs (all external schemas drift eventually).

## Flow

`pq-social-capture coingecko --trending --contract-watch <mints> --category <id>`
→ normalized NDJSON (`platform: "coingecko"`, optional `mint`,
`aggregator_listed: true`, `sentiment_bp`/`sentiment_conf_bp`) → the SAME
`parse_social_event` → `ingest_social` path as every other lane → dedup ring →
attention/narrative planes. `SocialPlatform::Aggregator` (horizon rank 3).
No new cache, resolver, ranking, or strategy path.

## Phase-B activation

Set `CG_API_KEY` (free Demo key from the CoinGecko dashboard), point
`--contract-watch` at the engine's confirmed/held mints file, run under the
supervisor with the other capture lanes. Failure behavior: fail-open as
absence (absence of aggregator data is never negative market evidence);
sentinel logs drift/challenge/auth states loudly.
