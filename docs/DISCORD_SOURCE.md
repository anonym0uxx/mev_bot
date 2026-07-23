# Discord — the paid real-time ALPHA-CALL source (§29/§6.6, Amendment A-5)

Constitutional status: named real-time alpha-call social source (Amendment A-5, human-directed
2026-07-23). Capture code laptop-built + fixture-tested; live connection is [S] server (Phase-B).
This is the §6.6 external-tool evaluation record for the dependency.

## What Discord is for Hermes

The operator subscribes to PAID alpha rooms whose calls/alerts/chat are highly relevant to
entries and exits. Hermes ingests them as **actionable alpha, at corroboration tier**:
1. **AlphaCall discovery lane** — a Discord call surfaces a mint onto the watchlist early; its
   realized net-SOL attributes to a distinct §71.2 lane so reflection measures per-room ROI
   (is this paid room worth the money?) and can up/down-weight or retire it.
2. **Designated-caller weight** — known paid-room callers (and curated Twitter follows) carry
   elevated attention weight, breadth-gated like the §29.6 broadcaster law (one caller = half
   a formation floor; genuine distinct corroboration completes it), never a blank multiplier.
3. **Exit signal** — a designated-caller bearish/sell/exit call on a HELD position raises exit
   pressure REDUCE-ONLY (§29.5): it can only accelerate an exit, never add or authorize risk.
4. **Sentiment** — every Discord message flows through the same local-LLM sentiment brain seam
   (GBNF-constrained, fail-open as absence) as the other social lanes.

**Inviolable (§29.8/§6.6):** Discord alpha ALONE — no on-chain confirmation, no numeric
microstructure — can NEVER admit an entry. It makes Hermes faster and better-targeted; the
on-chain + MinimumEconomicTradeGate still fires on every entry. Pinned: `tests/alpha_laws.rs`
(D2–D5) + the adm=0 invariant.

## Capture lane (Rust, `tools/stream-capture-rs discord-gateway`)

A PASSIVE, READ-ONLY Discord Gateway v10 client over the crate's hand-rolled RFC6455/rustls WS:
- Connect `wss://gateway.discord.gg/?v=10&encoding=json`; HELLO→heartbeat, IDENTIFY, READY,
  MESSAGE_CREATE→normalize, RECONNECT/INVALID_SESSION→resume-or-reidentify, zombie-connection
  detection via heartbeat-ACK, staleness watchdog, dedup ring on message snowflake.
- IDENTIFY intents `GUILDS | GUILD_MESSAGES | MESSAGE_CONTENT` (33281); **presence: invisible**
  (appears offline to the room while receiving every message — a first-class Discord feature);
  realistic desktop-client fingerprint; token placed raw in `d.token`.
- Guild+channel ALLOWLIST (only the operator's paid rooms) and a designated-caller author-id
  list. Emits raw NDJSON (`lane:"discord"`, §6.3) + normalized (`lane:"discord_alpha"`,
  `platform:"discord"`, `is_designated_caller`, cashtags, mints) → the SAME parse_social_event
  → ingest_social path as every lane → `SocialPlatform::Discord` (code 8, horizon 0).

### Operational posture — the safe stance for a legitimately-subscribed account
Bot tokens usually cannot be added to provider-run paid alpha rooms, so a USER token on a
dedicated account is the practical path. The lane is built to be low-profile the ONLY way that
actually protects a real account: **passive**. It is read-only (never sends, types, reacts,
joins/leaves), makes **zero REST calls** (no message-history fetch — that is what trips
scraping detection; it consumes only the live Gateway push), holds a single stable connection
with conservative reconnect backoff, and presents an invisible presence. A read-only listener
is essentially indistinguishable from a user with Discord open in a background tab. It
deliberately does **NOT** implement multi-account rotation, proxy/IP cycling, or fake-activity
generation — those increase risk and buy nothing for one legitimate subscription. On rate-limit
or disconnect it fails open and (Phase-B) alerts the operator. Use a dedicated throwaway account
you can afford to lose, never your main; keep the subscription legitimate. User-token automation
violates Discord ToS and carries a ban risk the operator has accepted.

Env/flags: `DISCORD_USER_TOKEN` or `DISCORD_BOT_TOKEN` (+ `--token-kind user|bot`, default user;
missing → exit 3), `--guilds`, `--channels`, `--callers`, `--allowlist-file`, client-fingerprint
flags. Twitter parity: `pq-social-capture twitterapi --follow <file>` tags curated follows with
the same `is_designated_caller` field so the designated-caller weight applies to both.

## Phase-B activation

Set `DISCORD_USER_TOKEN` (the dedicated alpha account), configure the guild/channel allowlist
for the rooms the operator names + the designated-caller author-ids, run under the supervisor
with the other capture lanes. Fail-open as absence: a Discord outage never halts trading. The
per-room ROI ledger (`Report.per_alpha_source_net`) tells the operator which paid rooms earn
their keep.
