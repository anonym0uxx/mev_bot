# HELIUS BUDGET — 30 days of continuous operation against the Business plan (2026-07-29)

**Plan under test:** 100M credits/month · 200 req/s · 50 `sendTransaction`/s · 5 `sendBundle`/s ·
staked connections · LaserStream WSS + gRPC · Shreds ($1,000/mo/IP add-on) · priority chat.

**Verdict in one line:** three of the four ceilings are irrelevant — we use **0.07%** of the request
budget and **0%** of the send budget — and the fourth, credits, is a **coin flip that nothing in the
code measures or bounds.** Central case lands at **~80% of 100M with no margin**; a busy month is
**2.7× over**. The single change that fixes it is free: our target band is entirely pre-graduation,
so most of what we currently subscribe to is data we cannot trade on.

---

## 1. The three ceilings that do not bind, disposed of first

| Ceiling | Our load | Headroom | Evidence |
|---|---|---|---|
| **200 req/s** | **0.133 req/s** | 1,500× | The bot has exactly one polling loop: `tools/stream-capture-rs/src/fees.rs:346-357` fires `getPriorityFeeEstimate` + `getRecentPrioritizationFees` every 15 s (`FEE_SAMPLE_INTERVAL_SECS`, `fees.rs:36`). Everything else is push. Verified by grep: the generic RPC pool `rpc.rs:214-249` has **no other production call site** — every remaining `pool.call(` is inside `#[cfg(test)]`. |
| **50 `sendTransaction`/s** | **0 today**, ≈0.94/s worst case once wired | 53× | Nothing in the repo submits a transaction. `rust/crates/pump-quant-app/src/main.rs:49-54` refuses `live` outright. Once Phase B builds the sender, the policy leaves cap it: 3 concurrent positions (`config.rs:1002`) × a 5-rung sell ladder over 16 s of timeouts (`ex_sell_ladder_state.rs:60-100`), decaying to one attempt per 25 s after exhaustion (`EXHAUSTED_COOLDOWN_MS`, `:106`). |
| **5 `sendBundle`/s** | ≈0.19/s worst case | 26× | Jito is opt-in per route (`ex_route_policy.rs:128-134`, "no blind Jito fallback", `:12`), bundles cap at 5 tx (`ex_bundle_assemble.rs:25`), and the same 3-position ceiling applies. |

**Staked connections and LaserStream entitlement** are qualitative, not quantitative: Business grants
roughly 10 concurrent gRPC connections (`docs/HERMES_ONE_SHOT_PROMPT.md` §18.4) and we open **one**
gRPC plus **one** WSS. The only way to breach that is connection leakage on reconnect — see §4(3).

**Shreds is a $1,000/month/IP add-on we do not use and should not buy.** There is no shred client in
the repo; the only shred artifact was `legacy/shredstream-proxy`, an orphan submodule gitlink with no
`.gitmodules` entry, deleted at commit `129696a`. Adding it would raise the bill 3× over the Business
base for a latency edge the current architecture cannot yet spend.

Everything below is therefore about credits, and credits are about **bytes**.

---

## 2. The conversion that decides everything

LaserStream is metered **~20 credits per MB** (`docs/HERMES_ONE_SHOT_PROMPT.md` §18.4, verified
2026-07; `docs/HELIUS_INTEGRATION.md:24-27` states the same rate as "2 credits/0.1MB"). Therefore:

```
100,000,000 credits ÷ 20 cr/MB = 5,000,000 MB = 5,000 GB ≈ 5 TB / month
                                              = 166.7 GB / day
                                              = 1.93 MB/s sustained, every second, for 30 days
```

**1.93 MB/s is the number to hold in your head.** Everything else is an estimate of whether the tape
we subscribe to exceeds it.

---

## 3. What we actually subscribe to, and what it costs

Source: `tools/stream-capture-rs/grpc-server-only/src/main.rs:134-160`. Three filters, commitment
`Processed` (`:158`):

| # | Filter | Scope as written | Bounded by anything? |
|---|---|---|---|
| B1 | `SubscribeRequestFilterTransactions` | `account_include = [PumpSwap, pump.fun]`, `vote:false, failed:false`, full detail | **No.** No data slice, no narrowing. |
| B2 | `SubscribeRequestFilterAccounts` | `owner = [PumpSwap]`, `..Default::default()` | **No.** No `memcmp`, no `datasize`, no data slice — a full-program account firehose. |
| B3 | `SubscribeRequestFilterSlots` | all slots | n/a, negligible |

Note that **subscription breadth is by PROGRAM, not by mint.** The watchlist caps (64 candidates,
`config.rs:940`; 256 confirmed, `:949`; 1,024–4,096 tracked mints) bound in-memory state and do
**nothing** to reduce Helius bytes. We receive the entire pump.fun + PumpSwap tape whether the
watchlist holds 64 mints or one.

### The model

Bytes-per-transaction is the dominant uncertainty. A Yellowstone `full` transaction update carries
the message, signatures, and the whole `meta` block — pre/post balances for every account,
pre/post token balances, inner instructions, and log messages. For a 17-account pump.fun swap, logs
alone are typically 40–60% of the payload. Central assumption **4 KB/tx**, bracketed 2.5–7 KB.
Combined pump.fun + PumpSwap non-vote rate bracketed **150 / 350 / 700 tx/s**.

| Scenario | B1 transactions | B2 accounts | B3 slots | A fees | **Total** | **% of 100M** |
|---|---|---|---|---|---|---|
| **Quiet** — 150 tx/s @ 2.5 KB | 19.4M cr (972 GB) | 3.1M cr | ~0 | 0.35M cr | **22.9M** | **23%** |
| **Central** — 350 tx/s @ 4 KB | 72.6M cr (3,629 GB) | 7.3M cr | ~0 | 0.35M cr | **80.2M** | **80%** |
| **Manic** — 700 tx/s @ 7 KB | 254.0M cr (12,701 GB) | 17.4M cr | ~0 | 0.35M cr | **271.8M** | **272%** |

The fee sampler — the only thing anyone would think to count, because it is the only thing that
looks like an API call — is **0.35% of the bill**. Even with four-provider failover amplification
(`rpc.rs:225-247` walks every eligible provider on failure, uncapped) it stays near 1.4%. It is
noise. The bill is one gRPC subscription's byte volume and nothing else.

### So: will we surpass 100M in a month?

**In a quiet month, no — comfortably, at about a quarter of the plan. In a central month we finish
at roughly 80% with no margin for a single manic week. In a genuinely busy month we blow through it
around day 11.** The distribution straddles the limit, and the spread between the arms is a factor
of twelve, which is the honest characterisation: **this is not currently predictable, because
nothing measures it.**

---

## 4. Four defects that turn "probably fine" into "unbounded"

**(1) There is no credit meter, no byte counter, and no cost monitor — and the constitution
requires all three.** §18.4 (`HERMES_ONE_SHOT_PROMPT.md:488`) demands *"Continuously calculate and
monitor: LaserStream data usage, credits consumed, estimated monthly cost, data-volume projections…
Cost monitoring is production health."* §31 (`:1241`) enumerates the required metrics by name.
Nothing in `tools/stream-capture-rs` counts a single byte.

**(2) The §72 arm-gate that was written for exactly this scenario is dead code.**
`rust/crates/pump-quant-ingest/src/source_registry.rs:119-128` defines
`may_arm(filter_breadth, cost_monitor_active)`, which refuses a subscription of breadth ≥ 64
(`BROAD_FILTER_BREADTH_THRESHOLD`, `:92`) unless a cost monitor is live. **`may_arm` has no
production caller** — every call site is inside its own `#[cfg(test)]` block. The capture binaries do
not even depend on `pump-quant-ingest`; they are separate crates outside the workspace. A
program-wide firehose is armed today with no cost monitor, which is precisely the state §72 exists
to make impossible.

**(3) The WebSocket reconnect loop is unbounded and its backoff can be defeated.**
`helius_ws.rs:316` is `loop {` with no exit and no attempt ceiling. `attempt` resets to 0 on **any**
notification (`:397`), so a connection that accepts the socket and delivers slot notifications but
rejects `transactionSubscribe` — a plan-gate or auth failure — will reconnect at the 1-second floor
forever, re-issuing the full subscription batch (`:332-338`) every time the 15 s staleness watchdog
fires (`HELIUS_WS_STALE_SECS`, `:41`). `Inbound::RpcError` is logged (`:426-431`) and otherwise
ignored. This is also the connection-leak path against the ~10-connection entitlement.

**(4) HTTP 429 is classified but never acted on.** `http.rs:26-29` defines `is_transient_status`
(429 + 5xx) and **nothing calls it**; `post_json_once` (`:56-66`) returns a plain error string.
`RpcPool` (`rpc.rs:238-246`) treats a rate-limit response identically to a connection refusal —
count an error, try the next provider. `Retry-After` is parsed nowhere: `backoff::retry_delay_secs`
takes a `retry_after_secs` parameter (`backoff.rs:37`) that has **no production caller in this
lane**. A throttled key gets hammered at full cadence indefinitely.

---

## 5. The reduction that costs nothing, and why it is available

**The operator's target band is entirely pre-graduation.** `mcap_band_lo_lamports = 118_420_000_000`
and `mcap_band_hi_lamports = 263_160_000_000` (`config.rs:984-985`) — 118.4 to 263.2 SOL of market
cap. Running that through the curve identity `mcap = vsol² / 32_190_000_000`:

```
mcap 118.42 SOL -> vsol = 61.74 SOL
mcap 263.16 SOL -> vsol = 92.04 SOL
graduation      -> vsol = 115.005 SOL      (85.005 SOL raised)
```

The band tops out at **80% of the way to graduation** and never reaches it. **PumpSwap is a
post-graduation venue.** We therefore cannot enter a single position on it — the entire
PumpSwap surface is needed only to (a) observe a graduation and (b) exit a position that graduated
mid-hold, which the 3-position ceiling and the sell ladder bound to a handful of pools we already
know by mint.

That makes both of these safe:

* **Delete B2 entirely.** The unfiltered `owner = PumpSwap` account firehose exists to see pool
  reserves. We hold at most 3 positions and enter none on PumpSwap. Replace it with a targeted
  `accountSubscribe`/account filter on the specific pool accounts of held positions — bounded by
  `max_concurrent_positions = 3`. Saves 3–17M credits.
* **Drop PumpSwap from `account_include` on B1**, or keep it and add a data slice. pump.fun is
  roughly half the combined tape.

| Scenario | Today | pump.fun-only + no account lane | Saving |
|---|---|---|---|
| Quiet | 22.9M (23%) | **10.7M (11%)** | 53% |
| Central | 80.2M (80%) | **39.9M (40%)** | 50% |
| Manic | 271.8M (272%) | **139.7M (140%)** | 49% |

That converts the central case from "no margin" to "2.5× margin", and converts the manic case from
"over on day 11" to "over on day 21" — still a breach, which is why the second lever matters:

* **Stop requesting full transaction detail if the decoder does not need it.** Log messages are the
  largest single component of a pump.fun transaction update. `docs/HELIUS_INTEGRATION.md:24-27`
  already prescribes *"scope gRPC filters tightly … data slices where possible"* and the
  implementation at `grpc-server-only/src/main.rs:151-157` uses `..Default::default()`. Closing that
  gap between the doc and the code is plausibly another 40% off B1, which brings even the manic case
  inside the plan.

---

## 6. What to do before day one of paper trading

**Paper mode does not help.** The capture binaries are mode-blind separate processes — no occurrence
of `RunMode`, `paper`, or `BankrollOrigin` anywhere under `tools/stream-capture-rs/`. Shadow mode is
explicitly *"paper on live feeds"* (`HERMES_PHASE_B_ACTIVATION_ONESHOT.md:519-521`). Budget 30 days
of paper at **100% of live read cost**; the only thing paper saves is a send path that costs 0 today
and ~2% of its ceiling once built.

Ordered by ratio of benefit to effort:

1. **Instrument before optimising.** Add a byte counter and a credit projector to the gRPC and WS
   lanes, emit to the journal, and alarm at 60% of plan on a 7-day trailing projection. This is
   §18.4 / §31 compliance, it is an afternoon, and it converts every estimate in this document into
   a measurement. **Do this first — the 12× spread in §3 is the cost of not having it.**
2. **Wire `may_arm` into both capture binaries** so a broad filter cannot arm without an active cost
   monitor. The function already exists and is already tested; it just has no caller.
3. **Drop the B2 account firehose**; subscribe per held pool instead.
4. **Narrow B1 to pump.fun**, or add data slices, or both.
5. **Fix the 429 path**: call `is_transient_status`, parse `Retry-After`, feed it to
   `backoff::retry_delay_secs` — all three pieces exist and are unconnected.
6. **Bound the WS reconnect loop**: cap attempts, treat a persisting `RpcError` as fatal rather than
   informational, and stop resetting `attempt` on a slot notification when the transaction
   subscription has produced nothing.
7. **Do not buy Shreds** ($1,000/mo/IP) until the execution path exists and a measurement shows
   latency, not information, is the binding constraint.

**One caveat on the whole document, stated in the spirit of §18.4's own warning
(*"Do not hardcode plan name, price, rate limits, credit model, data allowance … as permanent
truth"*):** the 20 cr/MB rate and the plan ceilings above are the operator's figures as of
2026-07-29. Verify them from the authenticated dashboard at M0 before trusting a projection built on
them, and record the verified values in the §18.9 infrastructure manifest.
