# Social ingestion — `[S]` live adapters + end-to-end probe

The deterministic core (portable, dossier-locked, laptop-tested) lives in the Rust
workspace and needs no keys:

- `pump-quant-ingest::social_parse` — vendor-agnostic decode of one normalized post
  → bounded `SocialEvent` (cashtag + Solana-contract extraction, echo flag, measured
  instant, `horizon_rank` provenance). No float/clock/network (§22).
- `pump-quant-ingest::social_source` — the `SocialSource` capture trait + portable
  mock + pure batch fan-out.
- `pump-quant-app::social_ingest` — maps `SocialEvent`s to corroboration-tier
  `AppEvent::SocialCall`s (per named contract) and narrative `Mention`s;
  `ledger_quality` resolves earned D1–D10 trust (PUBLIC_BURNED default);
  `coordinated_content` flags cross-source copy campaigns. Social is never
  self-authorizing; on-chain confirmation is always required (§29/§71).

This folder is the **`[S]` side**: the live capture that produces the normalized
JSON. It reads the clock and network at the capture boundary by design; it never
makes a decision. Every adapter emits the SAME one-object-per-line schema:

```json
{"platform":"x|telegram|tiktok|web","author":"...","community":"",
 "text":"gm $WIF <contract-addr>","likes":0,"reposts":0,"replies":0,"echo":false}
```

## The adapters (all `--selftest` runs with zero keys)

| Adapter | Source | Tier | Keys |
|---|---|---|---|
| `telegram_stream.py` | Telegram MTProto (Telethon) | **PRIMARY** — upstream, free, real-time; edits/deletions = D6 | `TELEGRAM_API_ID/HASH/SESSION` (my.telegram.org) |
| `twitterapi_stream.py` | twitterapi.io | X: firehose \| amplifier(KOL) \| list | `TWITTERAPI_IO_KEY` |
| `tiktok_stream.py` | Data365 / ScrapeBadger | slow-meta emergence | `TIKTOK_API_KEY` + `TIKTOK_API_BASE` |
| `firecrawl_stream.py` | Firecrawl | general-web legibility clock | `FIRECRAWL_API_KEY` |

`normalize.py` is the shared schema builder every adapter uses. `sources.yaml` holds
the seed inventory (TG channels, the X KOL watchlist + Greek-CT list, TikTok
hashtags, web pages) from constitution §29.7 — all PUBLIC_BURNED-presumed, to score,
never to trust. `run_all.py` multiplexes several adapters into one stream.

## Prove the whole pipeline with no keys

```bash
cd tools/social-ingest
python3 run_all.py --selftest | cargo run --quiet --manifest-path probe/Cargo.toml
```

Fuses every adapter's sample output through the REAL parser — cashtags, contract
addresses, engagement, echo, all extracted deterministically.

## Run live

```bash
# Telegram (primary) — dedicated research account, read-only:
pip install telethon pyyaml
export TELEGRAM_API_ID=... TELEGRAM_API_HASH=... TELEGRAM_SESSION=...
python3 telegram_stream.py | cargo run --quiet --manifest-path probe/Cargo.toml

# X firehose + KOL amplifier watch, fused:
export TWITTERAPI_IO_KEY=...
python3 run_all.py --adapters telegram,x-firehose,x-amplifier \
    | cargo run --quiet --manifest-path probe/Cargo.toml
```

X filter classes (from `sources.yaml`): `--class firehose` (breadth + mention
velocity), `--class amplifier` (`from:` the KOL list — PUBLIC_BURNED, for
wave-timing + FADE, never entry/copy), `--class list` (a curated CT list).

## Where it plugs in on the server

The `[S]` adapter feeds the engine, which calls
`social_ingest::ingest_next(&mut source, quality)` each cadence tick with
`quality = |ev| ledger_quality(&ledger, ev.author_id, &policy)` — earned trust from
the D1–D10 `SocialSourceQualityLedger`, never a per-account hardcode. Resulting
`SocialCall`s / `Mention`s enter the existing discovery lane + attention-velocity
layer; `coordinated_content` feeds COORDINATED_SPAM detection.

## Strategy (the why)

Alpha flows **upstream → downstream**: deployer/insider wallets → TG call channels →
mid-CT raids → big X-KOLs (Ansem/Orangie) → aggregators/scanners → retail. The KOL
and scanner layers are legible by construction (PUBLIC_BURNED); polling them to *buy*
is being exit liquidity. So we watch them only for wave-timing, meta rotation, and
FADE, and put the real weight on the upstream sources (Telegram, on-chain money) and
the *derivative* of distinct-originator mentions before the wave. Full write-up:
project doc **SOCIAL INGESTION STRATEGY — polling playbook**.

## Rust lanes are PRIMARY (twitch + twitterapi + tiktok + firecrawl)

The capture edge now runs in Rust wherever that is feasible without heavy
SDKs; the Python files in this folder stay as **reference + fallback** — the
executable spec each Rust twin is byte-tested against, and the quick tool for
probing a new vendor before a Rust port:

| Lane | PRIMARY | Fallback / reference |
|---|---|---|
| Twitch chat | [`../social-ingest-rs`](../social-ingest-rs) `pq-twitch-capture` (zero deps, plain TCP) | — (no Python twin) |
| X (twitterapi.io) | [`../social-ingest-https-rs`](../social-ingest-https-rs) `pq-social-capture twitterapi` | `twitterapi_stream.py` |
| TikTok | `pq-social-capture tiktok` | `tiktok_stream.py` |
| Firecrawl web | `pq-social-capture firecrawl` | `firecrawl_stream.py` |
| **Telegram** | **`telegram_stream.py` (Python stays PRIMARY)** | — |

Telegram stays Python by design: MTProto needs a heavy SDK (Telethon in
Python; grammers + tokio in Rust), which violates the Rust lanes'
minimal-dependency rule — and it is the one push-based lane, so the Rust
rewrite would buy the least. It remains a measurement-gated future option.
All lanes, either language, speak the identical NDJSON contract, so `run_all.py`
fan-in and the probe work unchanged. Same env vars, same `sources.yaml`, same
flags; the Rust lanes add `--replay <fixture>` for deterministic offline tests.

## Twitch (Rust lane)

Twitch chat is the one social lane that needs no TLS (plain-TCP IRC, anonymous
read-only access), so its capture is a dependency-free pure-std Rust binary:
[`../social-ingest-rs`](../social-ingest-rs) (`pq-twitch-capture`). It emits the
exact same one-object-per-line schema as `normalize.py` (platform `"twitch"`,
engagement zeros, `echo:false`, capture-stamped `observed_at_ns`), so it pipes
into the probe / paper runner and fuses with `run_all.py` output like any adapter
here. Includes a deterministic `--replay` mode for offline tests. See its README.
