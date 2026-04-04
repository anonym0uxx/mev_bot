# Bonding Curve Sniper — Ideation & Context

**Date:** 2026-04-03  
**Status:** Parked — ideation phase, not yet building  
**Purpose:** Feed this file to the quant architect when ready to build

---

## Origin of the Idea

We discovered that 94% of our current trades have `grad_speed_s=120` — a hardcoded fallback because we're entering post-graduation with no real signal about how old the token is. Most of our bad entries are tokens that took 2+ hours to graduate (slow bleeds, concentrated holders, already peaked when we enter).

This triggered a broader question: instead of entering AFTER graduation (on the DEX), could we enter EARLIER — at mint, on the bonding curve itself?

---

## What "Graduation" Means In Our Context

pump.fun tokens start on an internal bonding curve. When they raise ~85 SOL, they "graduate" — the LP migrates to Raydium or PumpSwap and becomes a real DEX pool.

Our current engine only triggers AFTER graduation (pool creation event via ShredStream). We don't touch the bonding curve at all.

Lifecycle from our perspective:
```
Token created on pump.fun
        ↓
Trades on bonding curve (we are currently blind to this entire phase)
        ↓
Hits 85 SOL → migrates to Raydium/PumpSwap  ← we currently enter here
        ↓
We buy post-graduation DEX trade
```

The sniper idea: enter at or near mint, on the bonding curve, and exit before or at graduation.

---

## Strategy: Bonding Curve Snipe for Quick Scalps

**Goal:** 12%+ scalps with guaranteed TX landing (both buy and sell)

**Why bonding curve is better for this goal vs post-graduation:**
- Price is deterministic math on the bonding curve — no AMM slippage surprises
- Early bonding curve moves fast — genuine momentum tokens can do 12% in seconds
- Jito atomic bundles: bundle buy + sell in same bundle → both land or neither does → no open position risk
- Less crowded than post-graduation (fewer bots watching migrations vs mint events)

**Bundle structure:**
```
TX 1: Buy X tokens at bonding curve price P1
TX 2: Sell X tokens at bonding curve price P2 (where P2 >= P1 * 1.12)
```
Both land atomically or neither does.

---

## Feed Stack for This Strategy

**Needed:**
- **PumpPortal** — `TokenCreated` events, real-time mint detection. Already have it.
- **ShredStream** — Raw TX stream for fast detection + Jito bundle submission. Already have it.
- **Helius** — RPC for any needed lookups. Already have it.

**NOT needed (can drop):**
- **Bitquery/CoreCast** — $200/month, currently used for creator sell detection, LP removal (rug), and AMM migration. All irrelevant on bonding curve — you're in and out in seconds before any of that matters. **Dropping this saves $200/month.**

---

## Key Filter Problem

PumpPortal emits hundreds of `TokenCreated` events per hour. 95%+ are rugs, bundles, or dev pre-loaded mints. The edge isn't speed — it's filter quality.

**Common filters:**
- No dev buy in first N blocks
- No bundled wallets (multiple wallets buying same block as mint)
- Metadata quality (real image, socials, non-copypasta name)
- First buyer count / concentration
- Creator wallet history (serial rugger vs legit dev)

---

## Relationship to the Deleted Backrunner

The old backrunner was a different beast — it was designed to backrun OTHER people's large buys on the bonding curve (MEV-style: see their tx in mempool, front/backrun it). That's a pure MEV play, latency-critical, no filter needed.

This sniper is NOT that. Key differences:
- **Backrunner:** React to other people's txs. MEV. Needed co-location and mempool access.
- **Sniper:** Detect new mints via PumpPortal feed, apply quality filter, enter independently. Signal-based, not MEV-based.

The bonding curve program interaction (buy/sell instructions) would be similar/reusable code. But the strategy logic, trigger, and filter are completely different. This is a cleaner, more sustainable edge than pure MEV backrunning.

The Jito bundle atomic buy+sell is also different from backrunning — it's self-contained, not dependent on anyone else's transaction.

---

## Open Questions (to resolve when building)

1. What's the right filter stack? Dev buy detection, bundle detection, metadata quality?
2. What's the target hold time? Seconds? Sub-minute?
3. Do we run this alongside the graduation engine or replace it?
4. What's the minimum liquidity/volume threshold to bother entering?
5. How do we size positions? Fixed SOL per snipe or Kelly-based?
6. What's our stop if the bundle fails to land (retry or abandon)?

---

## Alon's Notes / Decisions

- Primary goal: 12% quick scalps, guaranteed TX landing
- Likes the idea of dropping Bitquery entirely with this route
- Could run bonding curve sniper + graduation engine in parallel (different lifecycle phases, no interference)
- Not building yet — waiting for current graduation engine to prove/disprove its edge first

---

*Capturing ongoing conversation below as it happens...*

---

## Live Chat Log (2026-04-03)

**Alon:** Instead of graduation would we in theory be able to detect a new pump token in general? And buy/snipe at mint?

**Apollo:** Yes, and that's actually a more powerful strategy. PumpPortal already emits `TokenCreated` events — we receive them right now, we're just using them to track timestamps. Sniping at mint means entering on the bonding curve itself, not waiting for graduation. Completely different execution path: bonding curve buy instead of Raydium/PumpSwap swap. Different program, different instruction, different math. [full response above]

**Alon:** Which is more profitable for quick scalps where we can guarantee landing of tx and sales and take quick profits of minimum 12%

**Apollo:** For quick scalps with guaranteed landing — bonding curve snipe is more profitable, but only if your filter is good. On the bonding curve, price is deterministic math. You know exactly what you'll pay and exactly what price you need to sell at for 12%. A typical Jito bundle structure: TX1 buy + TX2 sell, atomic, both land or neither does. The catch: everyone else is doing this too. Filter quality is the edge. [full response above]

**Alon:** Could we drop bitquery if we go this route and stick with pumpswap, shredstream, and helius?

**Apollo:** Yes, completely. On the bonding curve you're in and out before creator sells, LP removal, or AMM migration matter at all. Three feeds instead of four, $200/month saved, cleaner architecture. [full response above]

**Alon:** Let's store these last 6 messages... Would this be diff than the backrunner code we deleted as well? Or how would that come into play?

**Apollo:** [This file is the answer to the first part. On the backrunner: it was MEV-based — reacting to other people's large buys, front/backrunning them, latency-critical. This sniper is signal-based — detect new mints via PumpPortal, filter for quality, enter independently. Different trigger, different logic, different edge. The bonding curve program code (buy/sell instructions) would be reusable. The strategy is not.]

**Alon:** Would we be able to use the social media query plan we thought of? How could that come into play?

**Apollo:** Yes — strongest filter we could build. Social media detection fits as a pre-filter before entering. The social media query plan was about detecting real organic interest before/around mint time — Twitter/X mentions, Telegram channel activity, Discord server creation.

For the sniper, it slots in as a pre-filter:
```
TokenCreated fires → check social signals → score (real community vs ghost launch) → enter or skip
```

What to look for:
- Twitter/X: mentions of token name/ticker in last 5-15 min before mint
- Telegram: does a TG channel exist? Member count? Age?
- Website/socials in metadata — real or placeholder?
- Volume of chatter relative to time of day

Timing challenge: need the data fast (10-30s window). Solution: **pre-detection** — monitor social feeds BEFORE the mint so by the time TokenCreated fires, we already have a score cached for that ticker/project.

Key edge: most sniper bots are purely on-chain reactive. A bot with pre-existing off-chain social signals is a fundamentally better filter than anything purely on-chain.

**Alon:** Could we get actual endpoints for how we could do that?

**Apollo:** Researched. Here's what's available:

**X (Twitter) Filtered Stream**
- Endpoint: `GET https://api.x.com/2/tweets/search/stream`
- Persistent HTTP connection, ~6-7s P99 latency
- Define rules on keywords/cashtags (e.g. `$TOKENNAME`), matching tweets stream real-time
- Cost: Pro tier ~$100/mo minimum for filtered stream access. Free/Basic = search only, no stream.

**Telegram — Telethon (MTProto)**
- NOT the Bot API (only works in chats bot is added to)
- Telethon uses your actual Telegram account via MTProto to monitor public channels
- `@client.on(events.NewMessage(chats=['@channel']))` — real-time event-driven
- Cost: FREE. Just need API ID/hash from my.telegram.org
- Use case: watch known pump.fun community channels for token name/ticker mentions

**pump.fun Token Metadata (already in PumpPortal — FREE)**
- `TokenCreated` events from PumpPortal already include: name, ticker, Twitter URL, Telegram link, website
- We receive this data NOW and throw it away
- A token with real socials at mint = dev planned the launch
- A token with no socials = likely rug
- This is Tier 1 filter, zero cost, immediate to implement

**Recommended layered approach:**
1. **Tier 1 (free, no new APIs):** PumpPortal metadata filter — has Twitter? Has Telegram? Has website? Score it.
2. **Tier 2 (free, Telethon):** If Telegram link exists, check channel age + member count. Old channel = planned launch.
3. **Tier 3 ($100/mo, X API Pro):** Stream tweets matching ticker pre-mint for buzz detection.

Start with Tier 1 — it's zero cost, already in our data stream, and probably cuts 60%+ of rugs immediately.

**Alon:** How would we integrate Kelly criterion algorithm here?

**Apollo:** Kelly is very natural on the bonding curve because price is deterministic — you know P1 (entry) and can set P2 (target) exactly.

Formula: `f* = (p * b - q) / b`
- p = probability of winning (token reaches P2 before dump)
- q = 1 - p
- b = net profit ratio (0.12 for 12% target)

The filter stack feeds Kelly directly — each signal adjusts `p`:
- Has Twitter + Telegram + website → p += 0.15 base
- Telegram channel old + 500+ members → p += 0.10
- Graduated in <60s → p += 0.15
- No dev buy in first block → p += 0.10
- etc.

Kelly then outputs position size as fraction of bankroll. High-conviction (p=0.65) → bet more. Ghost launch (p=0.25) → skip or bet tiny.

**Key insight with atomic bundles:** Because buy+sell bundle atomically, your actual loss on failure is just Jito tip + TX fee (a few thousand lamports), NOT a full position loss. This dramatically lowers the effective `q` in Kelly terms → Kelly recommends larger sizes than intuition suggests.

**Fractional Kelly:** Use half-Kelly (50% of recommended size) to start. Full Kelly maximizes long-run growth but high variance. Half-Kelly = more conservative with lower drawdown while we're proving the strategy.

**Architecture:** Filter scores → probability estimate p → Kelly formula → position size in SOL → bundle construction.

**Alon:** How would we replace the momentum engine with an engine like this?

**Apollo:** Honest answer: don't replace yet — run in parallel and let the data decide.

**Fundamental difference:**
- Momentum engine = REACTIVE. Waits for token to prove itself (graduation, velocity, score). Lower WR but real signal behind each entry.
- Bonding curve sniper = PREDICTIVE. Enters based on pre-mint signals before market has voted. Higher upside potential, betting on prediction not confirmation.

These aren't competing — they're two different bets on different moments in the lifecycle.

**If replacing fully, the momentum engine becomes 4 new components:**
1. **Signal collector** — replaces `on_graduation()`. Listens to `TokenCreated`, scores metadata filter stack, builds conviction score.
2. **Bundle constructor** — replaces Raydium/PumpSwap swap builder entirely. Atomic Jito bundles against bonding curve program. Completely different TX structure.
3. **Kelly sizer** — replaces `compute_size_lamports()`. Conviction score → position size. Simple formula replaces all complex scale-in logic.
4. **Position manager** — massively simplified. No trailing stops, no time SL, no scale-in tiers, no price feed subscriptions. Win/loss known at bundle landing time.

**What disappears:** trailing stop logic, scale-in system, observation window, velocity gate, price feed subscriptions, entire `on_tick()` loop, CoreCast/Bitquery. ~60-70% of current codebase gone.

**Recommended path:**
- Let momentum engine run 500+ trades with new grad-speed filter — see if WR improves
- Build sniper as parallel module sharing feed infra (PumpPortal, ShredStream, Jito)
- After 2-3 weeks of parallel data → kill the loser, double down on the winner
- Don't replace preemptively — let the data decide

**Alon:** So we'd build this separately and have it be a feature flag essentially in the env or config?

**Apollo:** Exactly. Pattern already exists in the codebase:

Current momentum gate in canary.json:
```json
{ "momentum": { "enabled": true, ... } }
```

Sniper would follow same pattern:
```json
{
  "momentum": { "enabled": true, ... },
  "sniper": { "enabled": false, "paper_mode": true, ... }
}
```

In main.rs, TokenCreated events fan out to both engines:
```rust
// Momentum gets: Migration / PumpSwapGraduationDirect events
// Sniper gets: TokenCreated events
if engine_config.sniper.enabled {
    sniper.on_token_created(...).await;
}
```

Both share the same feed channel. Zero interference. Paper-mode sniper runs alongside live momentum engine independently. Flip to live when data justifies it.

**Alon:** On the flip side of entry, how would we determine scale and buy more, sell some, or take profits and exit completely?

**Apollo:** Four models, each progressively more complex:

**Model 1 — Pure Atomic Bundle (start here)**
Buy+sell in one Jito bundle, predefined exit. No position management.
`Enter P1 → atomic sell at P1 * 1.12`. Zero open position risk. Caps upside but proves the filter.

**Model 2 — Partial Exit + Ride**
Bundle locks guaranteed profit on 80%, 20% stays open on a simple trailing stop.
`Buy 100% → atomic sell 80% at +12% → ride 20% with trail`
House money slice: if it moons you capture more, if it dumps you already locked profit.

**Model 3 — Mathematical Scale-in**
Bonding curve price is deterministic — you know exact price at any market cap.
`Tranche 1 at $10k MC → if +5% buy tranche 2 → sell tranche 1 at +12% → ride tranche 2`
Unlike AMM scale-in (messy, unpredictable), bonding curve scale-in is exact math. Perfect cost basis tracking.

**Model 4 — Kelly-Tiered Exits**
Kelly sizes not just entry but each exit tier based on conviction score.
`p=0.65: exit 30% at 8%, 40% at 15%, ride 30%`
`p=0.45: exit 70% at 8%, 30% at 12%, done`
`p=0.30: pure atomic only`

**Recommended progression:**
1. Start Model 1 (atomic) — prove filter WR > 50%
2. Add Model 2 (partial ride) — capture tails on strong tokens
3. Model 4 (Kelly-tiered exits) — endgame, needs historical data to calibrate

Complexity ladder: atomic → partial ride → Kelly-tiered. Earn each step with data.

**Alon:** This would be easier to gauge WR on trades than momentum I think bc momentum's logging etc is a mess. How clean is momentum's entry/exit wiring crossed to our jito and rpc buy/sell landing code?

**Apollo:** Honest assessment:

**Entry wiring:** Buy is `tokio::spawn` fire-and-forget. Position opens OPTIMISTICALLY before buy lands on-chain. `BuyState::Pending → Confirmed/Failed` state machine reconciles after the fact. Works but means engine thinks you're in a trade before you actually are.

**Exit wiring:** `close_position()` is ~250 lines of defensive edge-case handling built from real failures — check BuyState, poll for buy confirmation, handle tokens_held=0, last-chance pool resolution if accounts missing. Works but complex.

**Core problem:** Buy and sell are TWO SEPARATE TRANSACTIONS fired independently. Sell fires after buy confirms via polling. Non-atomic. This is why there's so much defensive code — you can buy and fail to sell, leaving tokens stranded.

**Sniper contrast:** Buy+sell in one Jito bundle = one atomic unit. Either both land or neither. Entire `BuyState` state machine, sell polling loop, tokens_held tracking, last-chance resolution — NONE of that exists. You never have an open position to manage.

**WR logging on sniper:** Bundle landed = win. Bundle rejected = loss. One event, one outcome, one log line. Dramatically cleaner than momentum's multi-async causal chain.
