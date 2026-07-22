# `pq-twitch-capture` — dependency-free Rust Twitch chat capture (`[S]` lane)

Anonymous, read-only Twitch chat → the normalized social NDJSON contract, in pure
std Rust. One binary, **zero dependencies** (`[dependencies]` is empty and stays
that way), standalone Cargo project outside the workspace (like `/bench`).

## Why Rust *here* (and only here)

Every other social lane (X via twitterapi.io, Telegram MTProto, TikTok, Firecrawl)
needs HTTPS/TLS, which in dependency-free Rust means hand-rolling a TLS stack —
a non-starter. Those lanes stay Python (`../social-ingest/`) behind the same
NDJSON contract, with a Rust port remaining a measurement-gated future option.

Twitch IRC is the exception: it speaks **plain TCP** (`irc.chat.twitch.tv:6667`)
and grants **anonymous read-only access** (`NICK justinfan<digits>`, no OAuth).
That makes it the ONE social lane where a proudly dependency-free Rust capture is
feasible today — a removable adapter (§67): delete this binary and the system
loses one lane, nothing else changes.

## Access model (constitution §29.7e)

No credentials exist or are needed — we read *public* chat as an anonymous
`justinfan` identity. Per §29.7e the reading identity is presumed sacrificial:
if Twitch drops the connection or blocks the nick, we reconnect (bounded
exponential backoff, 1 s → 60 s) and re-JOIN; nothing of value is lost. The
capture never writes to chat.

## What it emits

One compact JSON object per chat line on **stdout** (stderr carries diagnostics
only). Field names match `../social-ingest/normalize.py` exactly:

```json
{"platform":"twitch","author":"degenwif","community":"pumpwatch","text":"$WIF to a billion","likes":0,"reposts":0,"replies":0,"echo":false,"observed_at_ns":1753142400000000000}
```

- `author` — chatter nick (lowercase); `community` — channel without `#`
  (lowercase). §29 provenance: origin identity carried verbatim, trust is earned
  downstream (D-ledger), never assumed here.
- `likes`/`reposts`/`replies` — always `0`: chat has no engagement counters; the
  engagement floor is applied downstream.
- `echo` — always `false`: a chat line is an origination; copy-echo detection is
  downstream via content hash.
- `observed_at_ns` — capture-instant stamp (the `normalize.py` convention:
  production adapters may stamp at capture for exact Signal-Horizon latency).
  The capture edge is the ONE place wall clock is allowed (§22); the
  deterministic core never reads it. Consumers that stamp at parse time (the
  probe) simply ignore the field.

Wiring note: the core's `SocialPlatform::from_tag` does not yet accept
`"twitch"` — landing this lane end-to-end needs that one-word addition in
`pump-quant-ingest::social_parse` (a separate workspace change).

## Usage

```bash
cd tools/social-ingest-rs
cargo build --release

# Live capture, channels with or without '#':
./target/release/pq-twitch-capture pumpwatch '#solanastreams'

# Channels from a file (one per line, '#' optional):
./target/release/pq-twitch-capture --channels-file channels.txt

# Pipe into the end-to-end probe / paper runner exactly like a Python adapter:
./target/release/pq-twitch-capture pumpwatch \
    | cargo run --quiet --manifest-path ../social-ingest/probe/Cargo.toml

# Fuse with the Python lanes: run_all.py multiplexes NDJSON streams, and this
# binary is just another NDJSON producer on the same contract:
{ python3 ../social-ingest/run_all.py --adapters telegram,x-firehose & \
  ./target/release/pq-twitch-capture pumpwatch ; } \
    | cargo run --quiet --manifest-path ../social-ingest/probe/Cargo.toml
```

## Replay mode (deterministic, zero network)

```bash
./target/release/pq-twitch-capture --replay tests/fixtures/sample.irc
```

Reads raw IRC protocol lines from a file and emits the same NDJSON with a fixed
monotonically-increasing synthetic clock (`1_000_000_000 + n·1_000_000` ns) —
byte-identical on every run (§22 determinism boundary), which is what the
integration tests assert. No socket is ever opened in this mode.

## Robustness contract

- `PING :x` answered with `PONG :x`; all other IRC verbs ignored silently.
- IRCv3 `@tags` prefixes tolerated (never requested) and dropped.
- `\x01ACTION ...\x01` (`/me`) framing unwrapped to the inner text.
- Malformed lines skipped, never a panic; input lines truncated at 4096 bytes
  (UTF-8-boundary safe); non-UTF-8 bytes lossily decoded.
- Reconnect on EOF/error with bounded exponential backoff (1–60 s), full
  re-JOIN, and ~600 ms pacing between JOINs (Twitch join rate limits).

## Verify

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```
