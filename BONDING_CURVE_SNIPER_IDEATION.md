# Bonding Curve Sniper — Ideation & Context

**Date:** 2026-04-03 (revised 2026-04-04)
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
Hits 85 SOL → migrates to Raydium/PumpSwap  ← momentum engine enters here
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

## Feed Stack — Zero CoreCast

**Design constraint: CoreCast is dropped entirely. $200/month saved.**

CoreCast currently provides three signals for the momentum engine:
1. **Creator sell detection** (Stream 1) — signer vs `creator_map`
2. **AMM migration** (Stream 2) — redundant with ShredStream
3. **LP removal / rug detection** (Stream 3) — token supply drop via Bitquery

All three are derivable from our existing feeds. Details below.

### Feed Priority Order (non-negotiable)

```
ShredStream → Helius → PumpPortal (public RPC last resort)
```

- **ShredStream** is always first. Raw shred decode, ~0ms from block production. No websocket overhead.
- **Helius** is second. Enhanced WS subscriptions, rich decoded data, 50-200ms behind shreds.
- **PumpPortal** is third. Good for `TokenCreated` enrichment and social metadata. Not latency-critical.
- **Public RPC** is last resort only — never on the hot path.

### What Each Feed Provides for the Sniper

#### ShredStream (primary — lowest latency)
ShredStream already decodes every pump.fun instruction from raw shreds. Current `parse_pump_transaction()` extracts:
- `mint`, `trader`, `bonding_curve`, `assoc_bonding_curve`
- `sol_amount`, `token_amount`, `is_buy` (discriminator-based)
- `sig`, `slot`, `timestamp_ms`

This means ShredStream **already sees every buy and sell on every bonding curve in real time**.

For the sniper, ShredStream provides:
- **Mint detection** — `TokenCreated` events (if we decode the pump.fun `create` instruction — currently we pass this to PumpPortal, but ShredStream can decode it directly)
- **Bonding curve activity** — every buy/sell on the curve, real-time
- **Creator sell detection** — `parse_pump_transaction()` gives us `trader` pubkey. Cross-reference against `creator_map` (populated by PumpPortal `TokenCreated` events). If `trader == creator_map[mint]` and `is_buy == false` → `CreatorSell`. **This replaces CoreCast Stream 1.**
- **Graduation detection** — `parse_pump_migration()` already fires on migrate instruction. Already in production.

**Creator sell replacement implementation (existing infra, just wire it):**
```rust
// In shredstream.rs parse_pump_transaction():
// Already have: trader_key, is_buy, mint_key
// Add: if !is_buy && creator_map.get(mint) == Some(trader_key) → emit FeedEvent::CreatorSell
// creator_map is already populated by PumpPortal TokenCreated handler
```

#### Helius (secondary — enrichment + LP drain detection)
Helius Enhanced WS provides rich decoded account state. For the sniper:

- **LP removal / rug detection** — replace CoreCast Stream 3 via `accountSubscribe` on the **LP token mint account** for each open position. When LP token supply drops to 0 (or >80%), the pool is being drained → emit `FeedEvent::LpRemoval`. This is actually **lower latency** than CoreCast (Helius sees account state change immediately vs Bitquery polling Solana data). Already noted in memory as the replacement plan.
  ```
  Per open position: subscribe to LP mint account via Helius accountSubscribe
  → supply → 0: emit LpRemoval(mint)
  ```
  We already use `accountSubscribe` in `price_feed.rs` — infrastructure exists.

- **Pool vault enrichment** — `PumpSwapGraduationDirect` events already use Helius Enhanced WS to pre-extract `coin_vault` and `pc_vault`. Same mechanism used for sniper exit if token graduates mid-hold.

- **Bonding curve account data** — `accountSubscribe` on the bonding curve account gives us `virtual_sol_reserves` and `virtual_token_reserves` (the fields currently `0` in `TradeEvent` from ShredStream). This is how we get real-time price and market cap on the curve without RPC calls.

- **`getAsset(mint)`** — Helius DAS for social metadata (description, twitter, telegram, website) if PumpPortal didn't inline them. Used by social enrichment worker (async, off critical path).

- **`getAssetsByCreator(dev_pubkey)`** — Dev wallet history lookup for social scoring. Async, off critical path.

#### PumpPortal (tertiary — mint enrichment, social metadata)
PumpPortal `TokenCreated` events are the primary source of:
- Dev wallet pubkey (`traderPublicKey`) → populates `creator_map` for creator sell detection
- Social metadata: `twitter`, `telegram`, `website`, `description`, `imageUri` → feeds social scoring pipeline
- `name`, `symbol`, `uri` → metadata quality score

PumpPortal fires **after** ShredStream sees the create instruction (ShredStream is first to the block). But for social metadata it's the canonical source — ShredStream only sees raw bytes.

**PumpPortal is not used for execution decisions** — only for enrichment. Every execution-critical decision is ShredStream-first.

---

## On-Chain Signals Without CoreCast

### Creator Sell Detection → ShredStream + creator_map
**Current:** CoreCast Stream 1 sends a `CreatorSell` event via Bitquery Solana subscription.
**Replacement:** ShredStream already sees every sell TX. `creator_map` is already populated from PumpPortal `TokenCreated`. Wire the comparison in `parse_pump_transaction()`.
- **Latency improvement:** ShredStream creator sell detection is faster than CoreCast (shred vs websocket).
- **Cost:** Zero. Uses existing infra.
- **Implementation:** ~10 lines in `shredstream.rs`.

### LP Removal / Rug Detection → Helius accountSubscribe
**Current:** CoreCast Stream 3 sends `LpRemoval` via Bitquery `TokenSupplyUpdates`.
**Replacement:** On each position open, subscribe to LP mint account via `helius_ws.account_subscribe(lp_mint)`. On supply drop >80% → emit `FeedEvent::LpRemoval`.
- **Latency improvement:** Account state notification is faster than Bitquery polling.
- **Cost:** Zero (Helius subscription, existing infrastructure).
- **Implementation:** ~30 lines, mirrors existing `price_feed.rs` `accountSubscribe` pattern.
- **Cleanup:** Unsubscribe on position close.

### AMM Migration → ShredStream (already live)
**Current:** CoreCast Stream 2 sends migration events.
**Status:** Already fully redundant. ShredStream `parse_pump_migration()` and `parse_pumpswap_migration()` are in production and are the faster path. CoreCast migration events are already ignored.

### Bonding Curve Price → ShredStream + Helius accountSubscribe
**For the sniper, we need real-time bonding curve price.** Two approaches:
1. **ShredStream trade events** — each `TradeEvent` tells us `sol_amount` and `token_amount` traded. We can compute price as `sol_amount / token_amount`. Reserves not available from instruction data (currently `vsol_reserves = 0`).
2. **Helius accountSubscribe on bonding curve account** — gives us `virtual_sol_reserves` and `virtual_token_reserves` directly. Deterministic price = `vsol_reserves / vtoken_reserves`. This is the cleaner approach.

**Recommended:** Use ShredStream for trade event detection (fastest), subscribe Helius to bonding curve account for reserve state. Reserve updates lag by ~50-150ms but are accurate. For bundle construction, use latest known reserves from Helius account state.

---

## Relationship to the Deleted Backrunner

The old backrunner was a different beast — it was designed to backrun OTHER people's large buys on the bonding curve (MEV-style: see their tx in mempool, front/backrun it). That's a pure MEV play, latency-critical, no filter needed.

This sniper is NOT that. Key differences:
- **Backrunner:** React to other people's txs. MEV. Needed co-location and mempool access.
- **Sniper:** Detect new mints via ShredStream/PumpPortal, apply quality filter, enter independently. Signal-based, not MEV-based.

The bonding curve program interaction (buy/sell instructions) is similar/reusable code. The existing `parse_pump_transaction()` discriminators and account layouts are reference material for building the sniper buy TX.

The Jito bundle atomic buy+sell is also different from backrunning — it's self-contained, not dependent on anyone else's transaction.

---

## Entry/Exit Models

### Model 1 — Pure Atomic Bundle (start here)
Buy+sell in one Jito bundle, predefined exit. No position management.
`Enter P1 → atomic sell at P1 * 1.12`. Zero open position risk. Caps upside but proves the filter.

### Model 2 — Partial Exit + Ride
Bundle locks guaranteed profit on 80%, 20% stays open on a simple trailing stop.
`Buy 100% → atomic sell 80% at +12% → ride 20% with trail`
House money slice: if it moons you capture more, if it dumps you already locked profit.

### Model 3 — Mathematical Scale-in
Bonding curve price is deterministic — you know exact price at any market cap.
`Tranche 1 at $10k MC → if +5% buy tranche 2 → sell tranche 1 at +12% → ride tranche 2`
Unlike AMM scale-in (messy, unpredictable), bonding curve scale-in is exact math.

### Model 4 — Kelly-Tiered Exits
Kelly sizes not just entry but each exit tier based on conviction score.
`p=0.65: exit 30% at 8%, 40% at 15%, ride 30%`
`p=0.45: exit 70% at 8%, 30% at 12%, done`
`p=0.30: pure atomic only`

**Recommended progression:** Model 1 → prove WR > 50% → Model 2 → Model 4. Earn each step with data.

---

## Kelly Criterion Integration

Formula: `f* = (p * b - q) / b`
- `p` = probability of winning (token reaches P2 before dump) — derived from filter score
- `q` = 1 - p
- `b` = net profit ratio (0.12 for 12% target)

Filter stack feeds Kelly directly — each signal adjusts `p`:
- Has Twitter + Telegram + website → p += 0.15
- Telegram channel old + 500+ members → p += 0.10
- Graduated quickly → p += 0.15
- No dev buy in first block → p += 0.10

**Key insight with atomic bundles:** Loss on failure = Jito tip + TX fee (~5000 lamports), NOT a full position loss. This dramatically lowers effective `q` → Kelly recommends larger sizes than intuition suggests.

**Start with half-Kelly** (50% of recommended size). Full Kelly maximizes long-run growth but high variance. Half-Kelly = conservative with lower drawdown while proving the strategy.

---

## WR Logging vs Momentum Engine

**Momentum engine logging is messy:**
- Buy is fire-and-forget, position opens optimistically before buy lands
- `BuyState::Pending → Confirmed/Failed` state machine reconciles after the fact
- Exit is ~250 lines of defensive edge-case handling
- Buy and sell are two separate TXes — non-atomic — which is why there's so much defensive code

**Sniper logging is clean:**
- Bundle landed = win. Bundle rejected = loss. One event, one outcome, one log line.
- No `BuyState` state machine, no sell polling loop, no tokens_held tracking, no last-chance resolution
- WR calculation: `wins / (wins + losses)` — trivially derived from bundle outcomes

---

## Key Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| CoreCast | Dropped entirely | $200/mo saved; all signals replaceable with existing feeds |
| Creator sell detection | ShredStream + creator_map | Faster than CoreCast, uses existing infra, ~10 lines |
| LP removal detection | Helius accountSubscribe on LP mint | Faster than Bitquery polling, existing accountSubscribe infra |
| AMM migration | ShredStream (already live) | CoreCast Stream 2 already redundant, ignored |
| Bonding curve price | ShredStream events + Helius reserve state | Shred = fastest detection; Helius = accurate reserves for bundle construction |
| Feed priority | ShredStream → Helius → PumpPortal → public RPC | Non-negotiable latency ordering |
| Execution model | Jito atomic bundle (buy+sell) | No open position risk, clean WR logging, guaranteed landing or no-op |
| Entry model | Model 1 first (pure atomic) | Prove filter WR before adding complexity |
| Position sizing | Half-Kelly from filter conviction score | Conservative while proving strategy |
| Run alongside momentum | Yes, feature-flagged via canary.json | Let data decide which engine is better |

---

## Open Questions (to resolve when building)

1. What's the right filter stack priority order? Dev wallet history > metadata quality > social links > engagement?
2. At what market cap / bonding curve fill % do we enter? (0% fill = very early, high risk/reward; 50% fill = more signal, less upside)
3. Hold time window — what's the max hold before we consider the entry failed?
4. Do we need to handle the case where our target sell price is above the graduation threshold (~85 SOL)?
5. How do we construct the bonding curve buy TX? (discriminator, account layout — reference existing `parse_pump_transaction()` for account indices)
6. What Jito tip do we put on the bundle for priority without overpaying?

---

## Alon's Decisions (confirmed)

- Primary goal: 12% quick scalps, guaranteed TX landing
- CoreCast: drop entirely, derive all signals from ShredStream + Helius
- Feed priority: ShredStream always first, then Helius, then PumpPortal
- Social signals: use `social-signal-layer-spec.md` as the canonical spec for the filter stack
- Run sniper in parallel with momentum engine (feature flag, paper mode first)
- Not building yet — waiting for momentum engine to accumulate more data; sniper is next major build

---

*Spec version: 1.1 | Updated: 2026-04-04 | Author: Apollo*
