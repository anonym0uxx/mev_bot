# HELIUS BUDGET + SOURCE ALLOCATION — 30 days of continuous operation (2026-07-29)

**Plan under test:** 100M credits/month · 200 req/s · 50 `sendTransaction`/s · 5 `sendBundle`/s ·
staked connections · LaserStream WSS + gRPC · Shreds ($1,000/mo/IP add-on) · priority chat.

**Verdict.** Three of the four ceilings are irrelevant — **0.07%** of the request budget, **0%** of
the send budget. The fourth, credits, is decided entirely by how wide the LaserStream subscription
is. As written today it is a program-wide firehose and the central case eats **80% of the plan with
no margin**. Under the source allocation in §5 — free PumpPortal carrying the wide net, paid
LaserStream carrying the mints we can actually trade — the same month costs **7%**. That is an
**11× reduction**, and it comes from removing data we were paying for and could not legally act on
anyway.

**This document does not ask you to narrow the watchlist.** §5.3 is a price list, not a cap. The
marginal cost is roughly **100k credits per tracked mint per month**, so the plan affords about
**965 mints** in the include list — 15× today's `watchlist_capacity` of 64. Cost is not what binds
breadth. What matters is being *cognizant*: knowing the price of a knob before turning it, and
knowing that the regime (quiet vs manic) moves the bill more than the breadth does.

---

## 1. Two sources, and the rule for choosing between them

| | **PumpPortal** `wss://pumpportal.fun/api/data` | **Helius LaserStream** gRPC / WSS |
|---|---|---|
| Cost | **free, no auth** | ~20 credits/MB |
| Tier | **DISCOVERY** — parsed third-party (§6.6 / §28) | **CANONICAL** — raw on-chain |
| Filter granularity | **per mint** (`subscribeTokenTrade` takes a `keys` array) | per program *or* per account key |
| Latency | behind chain by the vendor's own ingest + parse | as close to the validator as we can buy |
| May a decision hang off it? | **No.** Must be corroborated on-chain before anything canonical hangs off it. | Yes — this is what corroboration *means*. |
| In-repo lane | `tools/stream-capture-rs/src/pumpportal_ws.rs` | `grpc-server-only/src/main.rs`, `src/helius_ws.rs` |

*(Naming note: the free lane is **PumpPortal**, a third-party data vendor. **PumpSwap** is the
post-graduation AMM program `pAMMBay6…`. They are unrelated; §5 turns on the difference.)*

**The rule.** Latency-tolerant and wide → PumpPortal, because it is free and its granularity is
per-mint. Latency-critical or decision-bearing → LaserStream, because §29 refuses entry without
on-chain confirmation and §97 makes per-swap event-driven position state law. **Never both for the
same mint at the same time** — that is the redundancy to eliminate, and the way to eliminate it is
not to dedupe after the fact but to keep the two subscription sets **disjoint by construction**.

The constitution already forces most of this. PumpPortal is DISCOVERY tier, so it *cannot* be the
canonical input to a gate decision no matter how cheap it is; and LaserStream is the only thing that
*can*, so paying for it to deliver mints we will never trade is pure waste. The allocation below is
what those two facts imply once you follow them.

---

## 2. The three ceilings that do not bind

| Ceiling | Our load | Headroom | Evidence |
|---|---|---|---|
| **200 req/s** | **0.133 req/s** | 1,500× | Exactly one polling loop: `fees.rs:346-357` fires `getPriorityFeeEstimate` + `getRecentPrioritizationFees` every 15 s (`FEE_SAMPLE_INTERVAL_SECS`, `fees.rs:36`). Everything else is push. The generic RPC pool `rpc.rs:214-249` has **no other production call site** — every remaining `pool.call(` is inside `#[cfg(test)]`. |
| **50 `sendTransaction`/s** | **0 today**, ≈0.94/s worst case once wired | 53× | Nothing in the repo submits a transaction; `main.rs:49-54` refuses `live` outright. Once built, the policy leaves cap it: 3 concurrent positions (`config.rs:1002`) × a 5-rung sell ladder over 16 s (`ex_sell_ladder_state.rs:60-100`), decaying to one attempt per 25 s (`EXHAUSTED_COOLDOWN_MS`, `:106`). |
| **5 `sendBundle`/s** | ≈0.19/s worst case | 26× | Jito is opt-in per route (`ex_route_policy.rs:128-134`; "no blind Jito fallback", `:12`), bundles cap at 5 tx (`ex_bundle_assemble.rs:25`), same 3-position ceiling. |

**Staked connections / LaserStream entitlement:** Business grants roughly 10 concurrent gRPC
connections (§18.4). We open one gRPC + one WSS + one PumpPortal socket (PumpPortal asks for exactly
one per client — `pumpportal_ws.rs:19-20`). The only way to breach is connection leakage on
reconnect; see §6(3).

**Shreds ($1,000/mo/IP) is not used and should not be bought.** No shred client exists; the only
shred artifact was `legacy/shredstream-proxy`, an orphan submodule gitlink with no `.gitmodules`
entry, deleted at `129696a`. It would triple the bill for a latency edge the current architecture
cannot spend, because there is no sender yet.

Everything below is credits, and credits are **bytes**.

---

## 3. The conversion that decides everything

LaserStream is metered **~20 credits per MB** (§18.4, verified 2026-07; `docs/HELIUS_INTEGRATION.md:24-27`
states the same rate as "2 credits/0.1MB"). Therefore:

```
100,000,000 credits ÷ 20 cr/MB = 5,000,000 MB = 5,000 GB ≈ 5 TB / month
                                              = 166.7 GB / day
                                              = 1.93 MB/s sustained, every second, for 30 days
```

**1.93 MB/s is the number to hold in your head.** PumpPortal contributes zero to it.

---

## 4. What the subscription costs as written today

`grpc-server-only/src/main.rs:134-160`, commitment `Processed` (`:158`):

| # | Filter | Scope as written | Bounded? |
|---|---|---|---|
| B1 | transactions | `account_include = [PumpSwap, pump.fun]`, `vote:false, failed:false`, full detail | **No** — no data slice, no narrowing |
| B2 | accounts | `owner = [PumpSwap]`, `..Default::default()` | **No** — no `memcmp`, no `datasize`, no slice: a full-program account firehose |
| B3 | slots | all | negligible |

Subscription breadth is **by program, not by mint**, so the watchlist caps (64 candidates,
`config.rs:940`; 256 confirmed, `:949`) bound in-memory state and reduce Helius bytes by nothing.
We receive the entire pump.fun + PumpSwap tape whether the watchlist holds 64 mints or one.

Central assumption 4 KB per full transaction update (logs alone are typically 40–60% of a pump.fun
swap payload), bracketed 2.5–7 KB; combined non-vote rate bracketed 150 / 350 / 700 tx/s.

| Scenario | **Total credits** | **% of 100M** |
|---|---|---|
| Quiet — 150 tx/s @ 2.5 KB | 22.9M | **23%** |
| Central — 350 tx/s @ 4 KB | 80.2M | **80%** |
| Manic — 700 tx/s @ 7 KB | 268.9M | **269%** |

The fee sampler — the only thing that *looks* like an API call — is **0.35%** of the bill.

---

## 5. The source allocation

### 5.1 What PumpPortal takes over, and why it is safe

The lane already exists and already has the right primitives (`pumpportal_ws.rs:39-75`):

| Subscription | What it carries | Why PumpPortal is the correct owner |
|---|---|---|
| `subscribeNewToken` | every pump.fun creation | Discovery. The gate refuses entry on a corroboration lane alone (`gate.rs`, `NeedsOnchainConfirmation`), so a creation event can only ever *nominate* a candidate. Nominating it 500 ms late costs nothing. |
| `subscribeMigration` | graduations | A graduation is a slow, one-shot venue change. We need to know it happened, not to race it. |
| `subscribeTokenTrade` (`--watch-file`) | trade flow **per mint** | This is the one PumpPortal does better than LaserStream *as a matter of shape*: it filters by mint, so it can cover the wide screening universe at zero cost. The flow it carries is screening evidence, not a decision input. |

**The band is what makes this safe.** `mcap_band_lo_lamports = 118_420_000_000`,
`mcap_band_hi_lamports = 263_160_000_000` (`config.rs:984-985`). Through `mcap = vsol²/32_190_000_000`:

```
mcap 118.42 SOL -> vsol =  61.74 SOL     (curve progress ~37%)
mcap 263.16 SOL -> vsol =  92.04 SOL     (curve progress ~72%)
graduation      -> vsol = 115.005 SOL    (85.005 SOL raised)
```

We are not a creation sniper. Every entry happens **mid-curve**, minutes to hours after launch, at
37–72% progress. Discovery latency of a second is irrelevant at that horizon — which is precisely
why the free feed can own discovery and the paid feed does not have to.

### 5.2 What LaserStream keeps, narrowed

**Replace the program-wide `account_include` with the watchlist**, driven from the engine and updated
as the watchlist churns. Keep `account_include = [pump.fun program]` only as a fallback if dynamic
filter updates turn out not to be available (see §5.4).

| Tier | Subscription | Set size | Why it must be canonical |
|---|---|---|---|
| **T2 — corroboration** | transactions, `account_include` = watchlist mints | ≤ 64 (`watchlist_capacity`, `config.rs:940`) | §29: the gate refuses without on-chain confirmation. This IS the confirmation. |
| **T3 — held positions** | accounts, keyed to the bonding-curve account of each open position, **with a data slice** | ≤ 3 (`max_concurrent_positions`, `config.rs:1002`) | §97 makes per-swap event-driven position state law and forbids an RPC price poller on the decision path. Exit latency is the most expensive latency in the system. Spend the budget here. |
| — | slots | 1 | clock, negligible |

**Delete B2 outright.** The unfiltered `owner = PumpSwap` account firehose exists to read pool
reserves. We enter zero positions on PumpSwap — the band tops out at 80% of the way to graduation —
so the only PumpSwap pools we ever care about are those of positions that graduated mid-hold, which
T3 covers by key.

### 5.3 The budget under the allocation — a price list, not a cap

**Nothing here narrows the watchlist.** Breadth is a strategy decision and must stay one; the point
of this section is to make its price legible so the decision can be taken on strategy grounds with
the cost known, rather than discovered as an overage invoice. **The include-list size and
`watchlist_capacity` are separate knobs** — they happen to be equal in the default design, but if a
wider watchlist is worth having, the correct move is to widen it and price the subscription, not to
shrink the strategy to fit a subscription that was never tuned.

**The marginal cost of one tracked mint, per month:**

| regime | tx/s/mint | KB/tx | GB/mo | credits/mo | **mints affordable on 100M** |
|---|---|---|---|---|---|
| quiet | 0.2 | 2.5 | 1.30 | 25,920 | **3,858** |
| central | 0.5 | 4.0 | 5.18 | 103,680 | **965** |
| busy | 1.0 | 5.0 | 12.96 | 259,200 | **386** |
| manic | 2.0 | 7.0 | 36.29 | 725,760 | **138** |

**Roughly 100k credits per tracked mint per month at central rates — so the plan affords about 965
mints in the include list.** That is 15× the current `watchlist_capacity` of 64 and nearly 4× the
confirmed-set cap of 256. **Cost is not the binding constraint on watchlist breadth and this
document should not be read as saying it is.** What binds breadth is the strategy question of how
many candidates are worth tracking, and the compute/latency question of what the hot path can rank
per tick — neither of which is priced here.

**What a given breadth costs (% of 100M):**

| mints | quiet | central | busy | manic |
|---|---|---|---|---|
| 64 (today's watchlist) | 2% | **7%** | 17% | 46% |
| 128 | 3% | 13% | 33% | 93% |
| 256 (confirmed-set cap) | 7% | 27% | 66% | 186% |
| 512 | 13% | 53% | 133% | 372% |
| 1024 | 27% | 106% | 265% | 743% |
| *program firehose, today* | *23%* | *80%* | — | *269%* |

Read across a row, not down a column: **the regime moves the bill more than the breadth does.** 64
mints in a manic month (46%) costs more than 256 in a central one (27%). That is the real finding,
and it is an argument for a **measured, adaptive breadth** rather than any fixed number — spend to a
credit budget with headroom, widening when the tape is quiet and narrowing only when a live meter
says the month is running hot. Which is impossible today, because there is no meter (§6(1)).

**Optimizations available, ranked by leverage, all independent of breadth:**

1. **Drop the unfiltered PumpSwap account firehose** — the band is pre-graduation, so it buys
   nothing we can trade on. Free.
2. **Stop requesting full transaction detail if the decoder does not need it.** Log messages are
   typically 40–60% of a pump.fun transaction update. `docs/HELIUS_INTEGRATION.md:24-27` already
   prescribes data slices; `grpc-server-only/src/main.rs:151-157` uses `..Default::default()`.
   Closing that gap is plausibly ~40% off the dominant lane — **worth more than halving the
   watchlist, and it costs no strategy.**
3. **Let PumpPortal carry the screening tier** so LaserStream only ever sees candidates that
   survived a screen. This is what makes a *wide* screening universe affordable: PumpPortal's
   per-mint filtering is free at any breadth.
4. **Adaptive breadth against a live credit meter**, once (1) in §7 exists.

At today's 64 with the allocation in place, a central month finishes at **7% — 14× headroom.** That
headroom is the point: it is what makes widening the band, raising `watchlist_capacity`, or adding a
second region affordable decisions rather than budget negotiations.

### 5.4 The one thing to verify before relying on this

Narrowing to the watchlist requires **updating the gRPC filter on a live stream** as the watchlist
churns. Yellowstone's subscribe endpoint is bidirectional and accepts a replacement
`SubscribeRequest`, and LaserStream is Yellowstone-compatible — but `helius-laserstream = "0.5"`
(`grpc-server-only/Cargo.toml:15`) owns reconnect internally (`main.rs:163-165`) and its API surface
for mid-stream filter replacement has not been checked in this repo. **Verify it against the SDK
before building on it.** If it is not available, the fallbacks in descending preference are:
(a) drop PumpSwap from `account_include` and add data slices, roughly halving the bill;
(b) tear down and re-subscribe on watchlist churn, accepting the `from_slot` replay cost — measure
it, because a high-churn watchlist could make replay more expensive than the firehose;
(c) keep the pump.fun program filter and accept ~40% of plan in the central case.

### 5.5 Failure modes, stated deliberately

**If PumpPortal dies** we lose *discovery of new candidates* and keep full canonical data on
everything already on the watchlist and every open position. We cannot open, we can always exit.
That is the correct failure direction and it should be asserted, not assumed — add a law pinning
that a PumpPortal outage cannot block an exit.

**If LaserStream dies** we must refuse to trade entirely, because PumpPortal is DISCOVERY tier and
§29 will not confirm on it. Fail closed; do not let a cost saving become a silent tier promotion.

**§18.4's warning applies to PumpPortal too:** *"Helius is not a sole point of failure."* Neither is
PumpPortal. Two free-tier characteristics to watch and journal: an undocumented cap on the
`subscribeTokenTrade` `keys` array (the builder at `pumpportal_ws.rs:53-66` imposes none), and
silent drops under load — which the 60 s staleness watchdog (`PUMPPORTAL_STALE_SECS`, `:35`)
catches only for *total* silence, not for partial loss.

---

## 6. Four unbounded paths, all in the capture lanes and none in the Rust workspace

**(1) No credit meter, no byte counter, no cost monitor — and the constitution requires all three.**
§18.4 demands *"Continuously calculate and monitor: LaserStream data usage, credits consumed,
estimated monthly cost, data-volume projections… Cost monitoring is production health."* §31
enumerates the metrics by name. Nothing in `tools/stream-capture-rs` counts a byte.

**(2) The §72 arm-gate written for exactly this is dead code.** `source_registry.rs:119-128` defines
`may_arm(filter_breadth, cost_monitor_active)`, refusing breadth ≥ 64 (`:92`) without a live cost
monitor. **No production caller** — every call site is in its own `#[cfg(test)]` block, and the
capture binaries do not depend on `pump-quant-ingest` at all. A program-wide firehose is armed today
in precisely the state §72 exists to prevent.

**(3) The WS reconnect loop is unbounded and its backoff is defeatable.** `helius_ws.rs:316` is
`loop {` with no ceiling; `attempt` resets to 0 on **any** notification (`:397`), so a connection
that delivers slot notifications but rejects `transactionSubscribe` — a plan-gate or auth failure —
reconnects at the 1-second floor forever, re-issuing the full subscription batch (`:332-338`) each
time the 15 s watchdog fires. `Inbound::RpcError` is logged (`:426-431`) and otherwise ignored. This
is also the connection-leak path against the ~10-connection entitlement.

**(4) HTTP 429 is classified and never acted on.** `http.rs:26-29` defines `is_transient_status`
and **nothing calls it**; `RpcPool` (`rpc.rs:238-246`) treats a rate-limit response identically to a
connection refusal. `Retry-After` is parsed nowhere — `backoff::retry_delay_secs` takes a
`retry_after_secs` parameter (`backoff.rs:37`) with no production caller in this lane.

---

## 7. Order of work before day one of paper trading

**Paper mode saves nothing on read quota.** The capture binaries are mode-blind separate processes —
no occurrence of `RunMode`, `paper`, or `BankrollOrigin` anywhere under `tools/stream-capture-rs/`.
Shadow mode is explicitly *"paper on live feeds"* (`HERMES_PHASE_B_ACTIVATION_ONESHOT.md:519-521`).
Budget 30 days of paper at **100% of live read cost**.

1. **Instrument before optimising.** Byte counter + credit projector on both Helius lanes, emitted
   to the journal, alarming at 60% of plan on a 7-day trailing projection. This is §18.4 / §31
   compliance, it is an afternoon, and it converts every estimate here into a measurement. **First,
   because the 12× spread in §4 is the cost of not having it.**
2. **Wire `may_arm` into both capture binaries.** The function exists and is tested; it has no caller.
3. **Delete the B2 PumpSwap account firehose.** Subscribe per held pool instead.
4. **Verify mid-stream filter replacement in the LaserStream SDK** (§5.4), then narrow
   `account_include` to the watchlist and drive it from the engine.
5. **Bring the PumpPortal lane up first and let it run alone for a day.** It is free, so its volume
   and drop characteristics can be measured at zero cost before a single Helius credit is spent —
   and that measurement is what tells you whether the 0.5 tx/s/mint central assumption in §5.3 is
   right for your actual watchlist.
6. **Assert the two failure directions from §5.5** as laws: PumpPortal down must not block an exit;
   LaserStream down must refuse entry rather than silently promote PumpPortal to canonical.
7. **Fix the 429 path and bound the WS reconnect loop** (§6(3), §6(4)). All the pieces exist and are
   unconnected.
8. **Do not buy Shreds** until the sender exists and a measurement shows latency, not information, is
   the binding constraint.

**Caveat, in the spirit of §18.4's own warning** (*"Do not hardcode plan name, price, rate limits,
credit model, data allowance … as permanent truth"*): the 20 cr/MB rate and the ceilings above are
the operator's figures as of 2026-07-29. Verify them from the authenticated dashboard at M0 and
record the verified values in the §18.9 infrastructure manifest.
