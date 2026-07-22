# PUMP-NATIVE INTELLIGENCE — feasibility + architecture record

Status: **decided and built** (the `pump` subcommand of
`tools/social-ingest-https-rs`, 2026-07-22). This doc records what was
investigated, what was chosen per data category, and what remains UNVERIFIED
until Phase-B server probing. Uncertainty markers are deliberate — do not
strip them without re-verifying.

## 1. What "Pump News" actually is

- **No official pump.fun product named "Pump News" could be verified as of
  2026-07-22.** Claims of one trace to third parties.
- The real pump.fun-native social surfaces are the **frontend feed endpoints
  the web UI itself consumes** — Feed, Communities, Callouts, and per-coin
  **replies** threads — plus the **mobile app's trends feed**. These are
  frontend plumbing, not products, and not documented.
- **pumpnewz.com is NOT pump.fun** — an unaffiliated third-party site. Do not
  treat it as an official source or a data vendor.
- docs.pump.fun exists but its contents are **UNVERIFIED** from this build
  environment (egress-blocked); nothing in it is known to expose a public data
  API. Verify during Phase B before citing it.

## 2. No official public data API

pump.fun exposes **no official public data API** — no keys, no docs, no SLA,
no ToS-sanctioned programmatic read surface. Everything below the on-chain
line is reverse-engineered frontend traffic and must be engineered as
revocable at any moment.

## 3. Endpoint catalog used (reverse-engineered, churn-prone)

| Host | Used for | Notes |
|---|---|---|
| `frontend-api-v3.pump.fun` | `GET /replies/{mint}?limit=50&offset=0&reverseOrder=true`; `GET /coins/currently-live` | The lane's poll targets. Host has churned `frontend-api` → `-v2` → `-v3`; each hop broke unofficial consumers. Response shape observed BOTH as a bare JSON array and as `{"replies":[...]}` — the capture lane handles both. |
| `advanced-api-v2.pump.fun` | (catalogued, not polled) coin lists / metadata used by the advanced UI | Same tier, same churn risk; kept on file for future categories only. |

Both sit behind Cloudflare. Treat every field as optional and every shape as
temporary — that is why the capture lane fingerprints response shape (FNV-1a
over sorted top-level keys) and logs `SCHEMA_DRIFT` instead of assuming.

## 4. Acquisition hierarchy chosen per category

| Category | Chosen acquisition | Tier | Rationale |
|---|---|---|---|
| Per-coin replies (social) | `pump` subcommand polling `frontend-api-v3` with a first-class degradation sentinel | tier-3 frontend | The ONLY source of this data anywhere; nobody sells it (see §5). Sentinel makes the fragility loud instead of silent. |
| Token creation / bonding-curve state / migration | **Canonical on-chain** via existing Helius websockets + PumpPortal free create/migration subscriptions | tier-1 | Chain truth beats frontend truth; already wired; zero new risk. |
| Livestream chat | **DEFERRED** | — | Pump livestreams run on LiveKit; chat needs a LiveKit access token minted per room by pump.fun's backend — token acquisition is a fragile auth dance not worth Phase-A complexity. |
| Trending / KOTH (king-of-the-hill) | **Derived on-chain preferred** (compute from curve buy-flow we already ingest); frontend trending endpoints only as a cross-check | tier-1 derive | A trending list we compute cannot be revoked or reshaped. KOTH endpoint liveness on v3 is **UNVERIFIED**. |

## 5. Provider scan results (who sells what — 2026-07-22)

- **PumpPortal** — free websocket for token **create** and **migration**
  events; **trade** stream is metered/paid. No social data.
- **Bitquery / Shyft / Helius** — on-chain (pump.fun program decoding, curve
  events, trades) only. No social data.
- **Moralis** — had pump.fun endpoints; **sunsetting 2026-07-31**. Do not
  build on it.
- **NOBODY sells pump.fun social data** (replies/feed/livestream). If we want
  it, we capture it ourselves from the frontend surface — hence the tier-3
  lane with the sentinel.

## 6. ToS / stability risks

- **Cloudflare WAF** fronts every endpoint; datacenter IPs may be challenged
  or blocked outright. The lane classifies a challenge page (`CHALLENGE_WALL`,
  5-minute backoff) separately from an auth revocation (`AUTH_WALL`,
  exit code 3) — Cloudflare serves challenges WITH a 403, so the challenge
  check runs first.
- **Rate posture**: ~20 requests/minute is the working anonymous ceiling; the
  lane enforces a hard global budget of ≤20 req/min across all watched mints
  (round-robin; `--live-list` reserves 1 req/min).
- **Endpoint churn**: v1→v2→v3 history says the host WILL move again. The
  shape-hash sentinel plus tolerant parsing means drift degrades loudly, not
  silently.
- **ToS**: scraping an undocumented frontend API is unsanctioned by
  definition; the lane runs anonymously (no account at risk), read-only, and
  budget-capped. Revocation is an accepted outcome, surfaced as exit 3.

## 7. Emission contract

One NDJSON line per NEW reply (deduped by reply id, bounded ring): the shared
`normalize.py` schema — platform `"pump"`, author = replying wallet
lowercased, community = coin mint verbatim (base58 is case-sensitive), zero
engagement counters, `echo:false`, `observed_at_ns` capture stamp — plus one
extra trailing field **`"mint"`**: the thread's mint. Thread context is a
mint-grade coin reference, stronger than any ticker parsed out of text.
Absence of data is never an error: an empty replies array is a quiet poll.

## 8. Phase-B activation checklist

1. **Probe anonymous GET from the server IP first**:
   `pq-social-capture pump --mints-file probe.txt --once` (1–2 mints).
   Exit 0 with NDJSON or a quiet pass = anonymous reads work from this IP.
2. Exit 3 (`AUTH_WALL`) = anonymous reads revoked for datacenter IPs — do not
   activate; escalate to the JWT decision below.
3. `CHALLENGE_WALL` = Cloudflare challenges this IP — retry across hours
   before concluding; consider it unavailable from this host if persistent.
4. Verify docs.pump.fun contents (UNVERIFIED) and KOTH endpoint liveness
   (UNVERIFIED) while probing.
5. Watch first-hour stderr for `SCHEMA_DRIFT` / `STATUS_CLASS_DRIFT`.
6. **JWT fallback is NOT implemented** — authenticating with a sacrificial
   pump.fun account to survive an auth wall is documented here as a Phase-B+
   decision point only (adds an account at risk and a login flow to maintain;
   take that on only if the probe proves anonymous reads are dead AND the
   replies signal has demonstrated alpha).

## 9. UNVERIFIED items (must be resolved by Phase-B probing)

- **Anonymous-read status** of `frontend-api-v3` from a datacenter/server IP.
- **docs.pump.fun contents** (whether anything official changed).
- **KOTH endpoint liveness** on the v3 host.
