# Birdeye — the REQUIRED 1D-candle backfill + token-data source (§6.7)

Constitutional status: **required source** (Amendment A-3, constitution §6.7,
human-directed 2026-07-23). Build obligation: `docs/SERVER_BUILD_MANIFEST.md` §10.
This document is the §6.6 **external-tool evaluation record** for the dependency.

## What Birdeye is for Hermes (and what it is NOT)

Birdeye supplies exactly two capabilities, both consumed through
**MarketIntelCache only**, both research/feature-plane, neither ever authority:

1. **1D OHLCV backfill/cross-check** for the §21.6 bar and market-structure
   family. Our own canonical trade flow remains the PRIMARY bar source — the
   only leakage-proof, wash-screenable one. Birdeye's role is history we were
   not alive to capture: daily candles extending structure lookback (prior
   range highs/lows, compression baselines, volatility regime, token-age
   conditioning) beyond our own capture window, plus a reconciliation
   cross-check on windows where both exist.
2. **Token-data enrichment for candle analysis**: overview fields (liquidity,
   holder counts, trade counts, volume, buy/sell pressure, price frames) and
   plan-tier-gated security fields — context features that condition structure
   analysis (e.g. "breakout on rising holders + executable liquidity" vs
   "breakout on air"), never signals that stand alone.

NOT: not a latency source (canonical streams see the chain first), not truth
(§6.1 explicitly prohibits Birdeye trade history as an authoritative raw
source — that prohibition stands unchanged under §6.7), not a trade
authorizer, not an availability dependency of any strategy lane.

## §6.6 evaluation record

| Field | Record |
|---|---|
| Capability provided | Historical 1D OHLCV per mint; token overview/security/market data |
| Already have it? | No — canonical capture starts at server go-live; no daily history before that |
| Hot-path relevance | Never (research/feature plane; cache enrichment only) |
| Latency / freshness | Non-critical (daily bars); observation vs data timestamps carried per §21.6 |
| Reliability / limits | Documented public API, plan-tiered CU + rate ceilings; budget-paced, CU-aware backoff |
| Failure behavior | Fail-open as absence; drift sentinel (shape-hash), loud logs; §21.6 screens reject bad candles |
| Cost / licensing | Keyed plans (Standard/Starter/Premium/Business); token_security needs Starter+ |
| Self-hostability | No (SaaS); mitigated: cache-only role, absence-tolerant consumers |
| Provenance / verifiability | Every record carries the full §21.6 MarketIntelCache carry list; overlapping windows reconciled against canonical flow |
| Dependence risk | Low by construction — absence degrades lookback depth only, never operation |
| Expected net-SOL impact | Longer-lookback structure features (hypothesis, §46-gated): better regime/level conditioning for entries/exits on tokens older than our capture history |
| Validation method | Reconciliation divergence report (Birdeye 1D vs own canonical daily aggregation on overlap) journaled before cross-check admission; feature families admitted only through §46 ablation |

## Acquisition (verified against docs.birdeye.so, 2026-07; re-verify at activation)

| Endpoint | Use | Notes |
|---|---|---|
| `GET /defi/v3/ohlcv?address=<mint>&type=1D&time_from=&time_to=` | daily candles per mint | count mode ≤5000 bars/call; `currency=usd` or `native`; all plan tiers |
| `GET /defi/token_overview?address=<mint>` | liquidity, holders, trades, volume, buy/sell pressure, price frames | all plan tiers |
| `GET /defi/token_security?address=<mint>` | security/authority flags for candle-context screening | Starter+ only; omit cleanly on Standard, never fabricate |

Base: `https://public-api.birdeye.so`. Auth: `X-API-KEY: $BIRDEYE_API_KEY`
(operator env, never committed) + `x-chain: solana`. Intervals available run
`1s`…`1M`; Hermes' §6.7 mandate is `1D` (other intervals only if a registered
experiment later asks — own flow owns sub-daily).

## Flow (Phase-B)

`pq-social-capture birdeye --ohlcv-watch <mints-file> --overview --security`
(new subcommand, SERVER_BUILD_MANIFEST §10; same ureq+rustls, Cargo.lock-pinned,
fixture-tested, budget-paced, drift-sentinel pattern as the `coingecko` lane)
→ provenance-tagged records → **MarketIntelCache** → §21.6 screens
(missing/stale, wrong-pair/duplicate, quote-asset distortion, artificial
volume, aggregation mismatch, look-ahead, survivorship) → daily bars feed
`pump-quant-features::market_structure` lookback + token-data context features.
No new cache, no new strategy path, no attention/narrative coupling (Birdeye is
market data, not social evidence — it does NOT enter the §29 social plane).

## Phase-B activation checklist

1. Operator provisions `BIRDEYE_API_KEY` (plan tier per token_security need).
2. Build the `birdeye` subcommand with recorded-response fixtures + drift fixture.
3. First live epoch: journal per-endpoint latency/CU budget + the
   reconciliation divergence report (Birdeye 1D vs own canonical daily bars).
4. Only after the divergence report is journaled may backfilled bars carry
   `reconciliation status: cross-checked`; before that they are
   `backfill-unreconciled` and consumers treat them accordingly.
