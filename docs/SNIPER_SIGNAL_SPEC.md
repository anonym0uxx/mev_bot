# SniperEngine — Entry Signal System Spec

**Version:** 1.2  
**Date:** 2026-04-04  
**Author:** Apollo  
**Reviewed by:** Opus 4.6 quant (2026-04-04)  
**Status:** Ready for implementation  
**Goal:** Bonding curve sniper. Jito atomic bundle (buy+sell). 12%+ scalp target. Max loss per attempt = Jito tip + fees (~5000 lamports).

**v1.2 Changes (Opus quant review):**
- G1 dev prebuy: expanded to first 5 trades, added same-slot SOL-linked wallet detection
- G2 same-block bundle: tightened to ≥2 wallets + volume threshold (was ≥3 wallets flat)
- G3 serial rugger: tightened from ≥20/50% to ≥5/40%; added ≥10 tokens = auto-fail regardless of rate
- G4 curve ceiling: lowered from 40 SOL to 25 SOL (bonding curve math, not arbitrary)
- G5 trade count: **DROPPED** — redundant with revised G4 and velocity signal
- G6 new: Creator wallet balance gate (throwaway wallet detection)
- G7 new: Supply concentration gate (>15% held by single wallet)
- S1 velocity: metric changed from vsol/trade to vsol/second; sub-10-trade substitute added
- S2 bot ratio: **REPLACED** with wallet diversity (unique_wallets/trade_count)
- S3 fill %: U-shaped curve, sweet spot at 2–8% fill
- S4 buy/sell ratio: **REPLACED** with sell pressure timing (first sell index)
- S5 smart money: pre-seeded from external PnL leaderboards; negative wallet signal added; max raised to 15pts
- Tier 2 weights rebalanced: dev history 55%, metadata 25%, twitter 10%, engagement 5%, telegram 5%
- Social signals disabled for tokens <120s old (data doesn't exist at mint)
- Entry thresholds tightened: ≥50 with velocity, ≥40 without

---

## Executive Summary

We enter pump.fun tokens **on the bonding curve**, before graduation, using Jito atomic bundles. TX1 buys at price P1; TX2 sells at price P2 ≥ P1 × 1.12. Both land or neither does — no open position risk.

Signal system has three tiers:
- **Tier 0: Hard Gates** — binary pass/fail. Any failure = immediate skip.
- **Tier 1: On-Chain Score** — continuous 0–100 score from ShredStream + Helius BC state.
- **Tier 2: Social Multiplier** — async 0.5×–2.0× multiplier. Irrelevant for tokens <120s old.

Entry fires when: all Tier 0 gates pass AND `on_chain_score * social_multiplier_bps / 10_000 >= entry_threshold`.

**Key insight (Opus review):** At fresh mint (<60s), most Tier 2 social signals don't exist or are fabricated. The system is biased toward Tier 0 gates and Tier 1 on-chain signals for early entries. Social layer is weighted toward dev history (only reliably available signal at mint).

**ArXiv 2602.14860 finding:** vsol/second (inflow rate) > vsol/trade as a velocity metric. Moderate-fast inflow (0.3 SOL/s) scores higher than spike inflow (>1 SOL/s) because coordinated snipers create instant spikes then exit. Wallet diversity > bot ratio detection.

**Economic breakeven (ArXiv eq. 3):** p(+12% move) > entry_price²/exit_price² — at vSol=5 entry this is extremely low. Atomic bundle structure means max loss = Jito tip only, so the bar for positive EV is lower than intuition suggests.

---

## Bonding Curve Math (Reference)

```
k = x0_tot_virt × y0_tot_virt = 30 × 1.073e9 = 3.219e10

vsol: current virtual SOL in curve
vtok: current virtual token supply = k / vsol
Price P = vsol² / k

Graduation: vsol_total = 115 (virtual). Real SOL raised = 85.

Fee: 1.25% on SOL exchanged. effective_sol_in = delta_sol × 0.9875

Buy:
  vsol_new = vsol + effective_sol_in
  tokens_out = vtok - k / vsol_new

Sell P2 target (+12%):
  P2 = P1 × 1.12
  vsol_exit = sqrt(P1 × 1.12 × k)
  sol_out = (vsol_exit - vsol_after_buy) × 0.9875

SOL inflow needed for +12% at various entry points:
  Entry vSol=5:   need +0.67 SOL net inflow
  Entry vSol=10:  need +1.4 SOL net inflow
  Entry vSol=20:  need +3.1 SOL net inflow
  Entry vSol=25:  need +4.2 SOL net inflow  ← revised G4 ceiling

This math is why G4 ceiling is 25 SOL, not 40. Above 25 SOL the required
follow-on inflow becomes unrealistic for typical pump.fun token lifetime.
```

---

## Tier 0: Hard Gates

**Evaluated in order, within 10ms of TokenCreated. Any failure = immediate skip, free all state.**
**Design principle: gates must be signal-rich and low-noise. Dropped G5 (trade count) as redundant.**

---

### G0: Not Mayhem Mode ⚠️ FIRST CHECK
**Source:** PumpPortal `TokenCreated` event field OR Helius BondingCurve `accountSubscribe` OR `create_v2` decode.  
**Logic:** `is_mayhem_mode == true` → FAIL.  
**Why:** Pump.fun AI agent gets 1B extra tokens, trades with random buy/sell walk for 24h. Destroys every Tier 1 signal:
- Velocity: agent places large buys → fake vsol/second spike
- Wallet diversity: agent = one wallet, collapses diversity ratio
- Sell timing: agent's random sells distort first-sell-index signal
- Fee routing: goes to undisclosed wallets, not tracked on fees.pump.fun

**⚠️ create_v2 gap:** Pump.fun is migrating from `create` to `create_v2` (different discriminator). Our current ShredStream parser only handles legacy `create`. Bots missing `create_v2` will silently drop all new tokens when transition completes. Fix required before sniper goes live: dual-discriminator detection in `shredstream.rs`.

**Rust field:** `not_mayhem_mode: bool`

---

### G1: No Dev Pre-Buy
**Source:** ShredStream first 5 trades + creator_map + same-slot check.  
**Logic (in order):**
1. If any of first 5 trades: `trader == creator_map[mint] && is_buy == true` → FAIL
2. If any wallet bought in the **same slot as the create instruction** AND that wallet received SOL directly from creator wallet in the last 100 slots → FAIL (1-hop linked wallet check via Helius `getSignaturesForAddress`, cached)

**Why extended:** Sophisticated devs use a secondary wallet funded right before launch. Buying in the create slot via Jito bundle is the common pattern for pre-arranged pumps.  
**Rust field:** `no_dev_prebuy: bool`  
**Default if creator_map miss:** PASS — lower social score compensates

---

### G2: No Coordinated Bundle at Create
**Source:** ShredStream slot tracking per mint.  
**Logic:** If `distinct_wallets_in_create_slot >= 2 AND total_sol_in_create_slot > 2.0` → FAIL.  
**Why revised from ≥3:** Coordinated snipers deliberately split into exactly 2 wallets to evade ≥3 detection. Volume threshold `> 2 SOL` prevents false positives from 2 small organic buys happening to land in the same slot.  
**Rust field:** `no_coordinated_bundle: bool`

---

### G3: Dev Not a Serial Rugger
**Source:** Dev wallet history cache (Helius `getAssetsByCreator`, async).  
**Logic (any condition → FAIL):**
- `dev_tokens_launched >= 10` (serial launcher, regardless of stated success rate — ArXiv confirms prolific creators graduate less)
- `dev_tokens_launched >= 5 AND rug_rate > 0.40`
- `dev_is_blacklisted == true` (manually curated list from our own losses)

**Default on cache miss:** PASS with neutral social score. Do NOT penalize unknown devs — most first-time devs are unknown.  
**Rust field:** `dev_not_serial_rugger: bool`

---

### G4: Curve Fill Below 25 SOL
**Source:** Helius BC `accountSubscribe` (vsol_reserves) OR ShredStream trade sum.  
**Logic:** `vsol >= 25.0` → FAIL.  
**Why 25 not 40:** At vSol=25, +12% requires ~4.2 SOL of follow-on net inflow. Typical pump.fun token that isn't graduating dies before accumulating that much additional inflow at this curve stage. The bonding curve math makes late entry EV-negative, not just low.  
**Rust field:** `curve_fill_ok: bool`

---

### G6: Creator Wallet Not a Throwaway
**Source:** Helius `getBalance(creator)` + `getAssetsByCreator(creator)` — async, cache.  
**Logic:** `creator_sol_balance < 0.05 SOL AND dev_tokens_launched == 0` → FAIL.  
**Why:** Script-generated rug factory wallets are funded with exactly enough SOL for the creation fee and nothing else. A fresh wallet with zero history and <0.05 SOL post-creation is almost certainly a disposable. Legitimate first-time devs either have SOL from prior activity or funded the wallet deliberately.  
**Default on cache miss:** PASS  
**Rust field:** `creator_not_throwaway: bool`

---

### G7: No Single-Wallet Supply Concentration
**Source:** ShredStream — track cumulative token purchases per wallet from trade log (no RPC needed).  
**Logic:** If any single wallet has bought `> 15%` of total tokens purchased so far (across all trades for this mint, window: first 20 trades) → FAIL.  
**Why:** A single wallet holding >15% of early supply with 20 trades into the launch is a coordinated dump setup. They have immediate leverage to crater the price.  
**Rust field:** `no_supply_concentration: bool`  
**Note:** Compute from ShredStream `token_amount` field per trader. Zero additional data needed.

---

### ~~G5: Trade Count~~ DROPPED
**Reason:** Redundant. G4 (curve fill ceiling at 25 SOL) already captures "too much activity for the vSol level." G7 (supply concentration) catches the harmful case. A standalone trade count gate added noise without signal value for mint sniping.

---

## Tier 1: On-Chain Score (0–100 pts)

**Computed from ShredStream trade stream + Helius BC account state. Re-evaluated on each ShredStream trade for this mint.**

**Entry threshold:** `on_chain_score >= 50` (with velocity data) or `>= 40` (without, i.e. trade_count < 10)

---

### S1: Inflow Rate Score (0–30 pts)
**Replaces vsol/trade. Metric: vsol/second = delta_vsol / seconds_since_create.**

**Why vsol/second beats vsol/trade:** Bots fragment orders into many small trades to look organic. vsol/trade rewards fragmentation. vsol/second measures actual capital commitment rate — harder to fake with timing manipulation.

**Why 0.3 SOL/s scores higher than 1.0 SOL/s:** Instant spike (>1 SOL/s) in first 10s is the coordinated sniper signature — one or two actors front-running, then exiting. Steady acceleration (0.3 SOL/s) over 30-60s is genuine organic momentum. You don't want to be buying the exit of someone else's spike.

```
vsol_per_second = (vsol - 30.0) / max(seconds_since_create, 1)
// Note: subtract 30.0 (virtual synthetic initialization) to get real inflow rate

Score:
  vsol/s >= 1.0  → 20 pts  (very fast — spike risk, capped lower)
  vsol/s >= 0.3  → 30 pts  (sweet spot — strong organic momentum)
  vsol/s >= 0.1  → 25 pts  (solid)
  vsol/s >= 0.03 → 15 pts  (moderate)
  vsol/s <  0.03 → 5 pts   (slow bleed)
```

**Sub-10-trade substitute (when trade_count < 10):**
```
Instead of 0 pts flat, use first non-creator buy size:
  first_buy_sol 0.1–0.5 SOL  → 8 pts  (human-sized, plausible organic)
  first_buy_sol 0.5–2.0 SOL  → 5 pts  (large, possible whale/bot)
  first_buy_sol > 2.0 SOL    → 3 pts  (very large single actor, spike risk)
  first_buy_sol < 0.01 SOL   → 0 pts  (dust/bot test)
  no trades yet               → 0 pts
Entry threshold: -10 pts (40 instead of 50) when trade_count < 10
```

**Rust fields:** `inflow_rate_score: u8`, `vsol_per_second: f64`  
**Config:** `min_trades_for_velocity: u32 = 10`

---

### S2: Wallet Diversity Score (0–25 pts)
**Replaces bot ratio detection. Metric: unique_wallets / trade_count.**

**Why diversity beats bot-detection:** Bot detection via "pump.fun UI program in tx logs" is fragile — Photon, BullX, GMGN, and other legit frontends call the program directly and get misclassified as bots. Actual bots increasingly route through pump.fun's API to look organic. Wallet diversity is harder to fake: coordinated snipers reuse wallets, genuine interest creates many unique participants.

```
unique_wallet_ratio = distinct_wallet_count / trade_count
// Window: first 20 trades observed for this mint

Score:
  ratio >= 0.80 → 25 pts  (almost all unique wallets — strong organic interest)
  ratio >= 0.60 → 18 pts
  ratio >= 0.40 → 10 pts
  ratio >= 0.20 → 4 pts
  ratio <  0.20 → 0 pts   (same wallets repeating = wash trading / bot cluster)
```

**Default if <5 trades:** 5 pts flat. Insufficient data — don't be generous.  
**Rust field:** `wallet_diversity_score: u8`

---

### S3: Curve Fill Score (0–15 pts)
**U-shaped. Sweet spot at 2–8% fill (vSol 2.3–9.2).**

**Why U-shaped:** ≤2% fill is favorable math but zero evidence (too early). >22% fill requires too much follow-on inflow for +12%. The 2–8% zone has the best combination of: some momentum evidence + favorable bonding curve math + early enough for full upside.

```
fill_pct = vsol / 115.0

Score:
  fill_pct <= 0.02  (vsol ≤ 2.3)   → 8 pts   (too early, math good, evidence zero)
  fill_pct <= 0.08  (vsol ≤ 9.2)   → 15 pts  (sweet spot)
  fill_pct <= 0.15  (vsol ≤ 17.25) → 12 pts  (solid, still good math)
  fill_pct <= 0.22  (vsol ≤ 25.3)  → 6 pts   (G4 ceiling kicks in at 25 SOL)
```

**Rust field:** `fill_score: u8`

---

### S4: Sell Pressure Timing Score (0–15 pts)
**Replaces buy/sell ratio. Metric: index of first sell trade.**

**Why timing beats ratio:** The first N trades are almost always buys by definition — there are no tokens to sell until someone has bought first. A buy ratio of 1.0 in the first 5 trades is uninformative noise. The actually predictive signal is: when does the first sell appear? Early sells = dev or coordinated sniper exiting. Delayed sells = genuine buyers holding.

```
first_sell_trade_index = index of first sell tx observed for this mint

trade_count < 8:
  → 5 pts flat (insufficient data, no buy/sell judgment)

trade_count >= 8:
  first_sell_index > 15  → 15 pts  (strong sustained buying, no early exit)
  first_sell_index 10-15 → 12 pts
  first_sell_index 5-10  → 7 pts
  first_sell_index < 5   → 0 pts   (immediate sell = dev/sniper dumping)

Bonus modifier (additive, applied after base score):
  First seller sold <20% of their position → +3 pts  (partial take = holder, not dumper)
  First seller sold >80% of their position → -5 pts  (full exit = bad signal)

Floor: 0 pts total
```

**Rust field:** `sell_timing_score: u8`, `first_sell_index: u32`, `first_sell_pct: f32`

---

### S5: Smart Money Presence (0–15 pts, can go negative)
**Pre-seeded from external PnL leaderboards. Negative signal added.**

**Cold start fix:** Pre-populate `smart_wallet_set` from top-500 pump.fun wallets by PnL (Arkham/GMGN.ai/Helius indexing — public data). Update weekly. Start with data, not empty. Also maintain `dumper_wallet_set` from known consistent losers/dumpers.

```
Logic: scan first 10 buys for this mint

Any top-100 PnL wallet in first 10 buys   → 15 pts
Any top-500 PnL wallet in first 10 buys   → 10 pts
Any own-winner wallet (from our trade log) → +5 pts additive
Known dumper wallet in first 10 buys      → -10 pts (yes, negative — subtract from total)
No recognized wallets                     → 0 pts   (neutral, not penalized)

Total S5 range: -10 to 15 pts
```

**Rust field:** `smart_money_score: i8` (signed — can be negative)  
**External data:** `data/smart_wallets.json` (top-500), `data/dumper_wallets.json` — refresh weekly via cron

---

### On-Chain Score Aggregation

```rust
pub struct OnChainScore {
    pub inflow_rate_score: u8,       // S1: 0-30
    pub wallet_diversity_score: u8,  // S2: 0-25
    pub fill_score: u8,              // S3: 0-15
    pub sell_timing_score: u8,       // S4: 0-15
    pub smart_money_score: i8,       // S5: -10 to +15
    pub total: u8,                   // clamped 0-100
    pub vsol_per_second: f64,
    pub unique_wallet_ratio: f32,
    pub first_sell_index: u32,
    pub trade_count_at_score: u32,
    pub vsol_at_score: f64,
    pub computed_at_ms: u64,
    pub velocity_data_available: bool,  // false if trade_count < 10
}

impl OnChainScore {
    pub fn total(&self) -> u8 {
        let raw = self.inflow_rate_score as i16
            + self.wallet_diversity_score as i16
            + self.fill_score as i16
            + self.sell_timing_score as i16
            + self.smart_money_score as i16;
        raw.clamp(0, 100) as u8
    }

    pub fn entry_threshold(&self) -> u8 {
        if self.velocity_data_available { 50 } else { 40 }
    }
}
```

---

## Tier 2: Social Multiplier (0.5×–2.0×)

**Async. Fired on TokenCreated, cached in DashMap. Applied as:**
`final_score = on_chain_score × social_multiplier_bps / 10_000`

**Critical constraint:** For tokens age < 120 seconds, SS2 (engagement) and SS5 (telegram) are **disabled** (set to neutral 10_000 bps). At mint, these signals don't exist or are fabricated. Only SS1 (dev history) and SS4 (metadata quality) are reliably available at t=0.

**Revised weights (Opus review — heavily toward dev history at mint):**

| Signal | Weight | Rationale |
|--------|--------|-----------|
| SS1: Dev history | 55% | Only reliable signal available at mint |
| SS4: Metadata quality | 25% | In create event, instant, no API |
| SS3: Twitter presence | 10% | Weak but free from PumpPortal event |
| SS2: Pump.fun engagement | 5% | Disabled <120s, weak after |
| SS5: Telegram size | 5% | Near-useless at mint; pre-built = coordinated |

---

### SS1: Dev Wallet History (55% weight)
**Source:** Helius `getAssetsByCreator(dev_pubkey)` — async, cache 1hr.

```
dev_is_blacklisted (from our own loss log) → social_multiplier = 0 (hard veto)

dev_tokens_launched == 0:
  → +500 bps (slight positive — first launch devs not automatically bad)

dev_tokens_launched 1-4, rug_rate < 0.20:
  → +2000 bps (track record: launched tokens, most succeeded)

dev_tokens_launched 1-4, rug_rate 0.20-0.40:
  → neutral (10_000 bps)

dev_tokens_launched 1-4, rug_rate > 0.40:
  → -2000 bps (more than 2 in 5 rugged)

dev_tokens_launched 5-9, success_rate > 0.40:
  → +1500 bps (consistent launcher with decent record)

dev_tokens_launched 5-9, success_rate < 0.20:
  → -2000 bps

dev_tokens_launched >= 10:
  → G3 gate already blocked this. Shouldn't reach scoring.
```

**Note:** "Success" = token graduated OR reached >20 SOL on curve (significant traction, even if not graduated).

---

### SS4: Metadata Quality (25% weight)
**Source:** PumpPortal `TokenCreated` — no API call, instant.

```
link_count = (twitter != null) + (telegram != null) + (website != null)

link_count == 0 AND description_len < 10:
  → -1500 bps (zero effort = throwaway launch)

link_count == 0, description_len 10-50:
  → -500 bps (minimal effort)

link_count == 1:
  → +300 bps

link_count == 2:
  → +700 bps

link_count == 3:
  → +1200 bps

has_image:
  → +400 bps (custom image takes effort)

description_len 50-150:
  → +400 bps

description_len > 150:
  → +600 bps
```

---

### SS3: Twitter Presence (10% weight)
**Source:** PumpPortal `TokenCreated` — `twitter` field. Presence only, no API verification.

```
twitter present → +800 bps
twitter absent  → -800 bps
```

**Note:** Weight reduced from 20% to 10%. Presence-only (no follower count, no account age) is weak signal. Kept because it's free and marginally predictive.

---

### SS2: Pump.fun Engagement (5% weight)
**Source:** `GET https://frontend-api-v2.pump.fun/coins/{mint}` — async.  
**DISABLED for tokens age < 120 seconds.** At mint, any replies are bots or dev alts.

```
Token age >= 120s only:
  reply_count 1-5   → +200 bps
  reply_count 6-20  → +500 bps
  reply_count > 20  → +1000 bps
  is_koth           → +1500 bps
  is_live           → +700 bps
```

---

### SS5: Telegram Community Size (5% weight)
**Source:** Telegram Bot API `getChatMemberCount`.  
**DISABLED for tokens age < 120 seconds.**

```
Token age >= 120s only:
  no link           → neutral (10_000 bps)
  0-10 members      → -400 bps  (empty = fake)
  11-100 members    → +200 bps
  101-500 members   → +600 bps
  > 500 members     → +1000 bps

Note: pre-built channels (>500 members on a <60s token) are a NEGATIVE signal
in practice — coordinated launches pre-build fake communities.
At age < 120s, a >500 member channel → -500 bps instead.
```

---

### Social Score Computation

```rust
pub struct SocialScore {
    pub has_twitter: bool,
    pub has_telegram: bool,
    pub has_website: bool,
    pub has_image: bool,
    pub description_len: u16,
    pub dev_tokens_launched: u16,
    pub dev_rug_count: u16,
    pub dev_success_rate_bps: u16,
    pub dev_is_blacklisted: bool,
    pub pumpfun_reply_count: u16,
    pub pumpfun_is_koth: bool,
    pub pumpfun_is_live: bool,
    pub telegram_members: u32,
    pub token_age_secs: u32,           // age when enrichment completed
    // Sub-scores (bps, 10_000 = neutral)
    pub dev_score_bps: u16,
    pub metadata_score_bps: u16,
    pub twitter_score_bps: u16,
    pub engagement_score_bps: u16,
    pub telegram_score_bps: u16,
    // Final
    pub social_multiplier_bps: u16,    // 5_000–20_000. 0 = hard veto.
    pub enrichment_complete: bool,
    pub enrichment_ts_ms: u64,
}

// Weights (must sum to 100)
const W_DEV: i32 = 55;
const W_MET: i32 = 25;
const W_TWI: i32 = 10;
const W_ENG: i32 = 5;
const W_TG:  i32 = 5;

fn compute_multiplier(s: &SocialScore) -> u16 {
    if s.dev_is_blacklisted { return 0; }

    // Disable time-sensitive signals for fresh tokens
    let eng = if s.token_age_secs < 120 { 10_000i32 } else { s.engagement_score_bps as i32 };
    let tg  = if s.token_age_secs < 120 {
        // Pre-built community is negative for fresh mints
        if s.telegram_members > 500 { 9_500i32 } else { 10_000i32 }
    } else {
        s.telegram_score_bps as i32
    };

    let dev = s.dev_score_bps as i32 - 10_000;
    let met = s.metadata_score_bps as i32 - 10_000;
    let twi = s.twitter_score_bps as i32 - 10_000;
    let eng_d = eng - 10_000;
    let tg_d  = tg  - 10_000;

    let delta = (dev*W_DEV + met*W_MET + twi*W_TWI + eng_d*W_ENG + tg_d*W_TG) / 100;
    ((10_000 + delta).clamp(5_000, 20_000)) as u16
}
```

---

## Complete Rust Structs

### BondingCurveState

```rust
pub struct BondingCurveState {
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],
    pub creator: [u8; 32],
    pub is_mayhem_mode: bool,

    // Curve state (from Helius accountSubscribe, confirmed)
    pub vsol: f64,               // virtual SOL in curve
    pub vtok: f64,               // virtual token supply
    pub real_sol: f64,           // vsol - 30.0

    // Trade tracking (from ShredStream)
    pub trade_count: u32,
    pub buy_count: u32,
    pub sell_count: u32,
    pub unique_wallets: u32,     // distinct trader pubkeys seen
    pub wallet_set: HashSet<[u8; 32]>,  // for unique count

    // Per-wallet token holdings (for concentration check G7)
    pub wallet_token_holdings: HashMap<[u8; 32], u64>,

    // Velocity
    pub vsol_per_second: f64,    // (vsol - 30.0) / age_secs

    // Sell pressure
    pub first_sell_index: u32,   // trade index of first sell (u32::MAX if none)
    pub first_sell_pct: f32,     // fraction of that wallet's holdings sold

    // Timing
    pub created_at_ms: u64,
    pub last_trade_ms: u64,
    pub age_secs: f64,           // (last_trade_ms - created_at_ms) / 1000.0

    // Status
    pub creator_sold: bool,
    pub graduated: bool,
    pub evaluation_count: u16,
    pub first_buy_sol: f64,      // first non-creator buy size (for sub-10-trade signal)
}

impl BondingCurveState {
    pub const K: f64 = 3.219e10_f64;
    pub const GRAD_VSOL: f64 = 115.0;
    pub const FEE_RATE: f64 = 0.9875;
    pub const VIRTUAL_INIT: f64 = 30.0;  // synthetic SOL initialization

    pub fn real_inflow(&self) -> f64 { self.vsol - Self::VIRTUAL_INIT }
    pub fn fill_pct(&self) -> f64 { self.vsol / Self::GRAD_VSOL }
    pub fn price(&self) -> f64 { self.vsol * self.vsol / Self::K }
    pub fn unique_wallet_ratio(&self) -> f32 {
        if self.trade_count == 0 { return 0.0; }        self.unique_wallets as f32 / self.trade_count as f32
    }
    pub fn max_wallet_concentration(&self) -> f32 {
        let total: u64 = self.wallet_token_holdings.values().sum();
        if total == 0 { return 0.0; }
        let max_held = self.wallet_token_holdings.values().copied().max().unwrap_or(0);
        max_held as f32 / total as f32
    }
    pub fn sol_needed_for_12pct(&self) -> f64 {
        let p1 = self.price();
        let vsol_exit = (p1 * 1.12 * Self::K).sqrt();
        (vsol_exit - self.vsol) / Self::FEE_RATE
    }
}
```

### SniperEntrySignal

```rust
pub struct SniperEntrySignal {
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],
    pub assoc_bonding_curve: [u8; 32],
    pub on_chain_score: OnChainScore,
    pub social_score: Option<SocialScore>,
    pub final_score: u8,
    pub kelly_p: f32,
    pub position_size_sol: f64,
    pub entry_vsol: f64,
    pub entry_price: f64,
    pub target_vsol: f64,
    pub target_price: f64,
    pub buy_sol_lamports: u64,
    pub min_tokens_out: u64,
    pub sell_tokens: u64,
    pub min_sol_out: u64,
    pub jito_tip_lamports: u64,
    pub decision_ms: u64,
    pub paper_mode: bool,
}
```

### SniperTradeLog

```rust
#[derive(Serialize)]
pub struct SniperTradeLog {
    pub mint: String,
    pub token_name: String,
    pub token_symbol: String,
    pub decision_ms: u64,
    pub paper_mode: bool,
    pub token_age_secs: f64,
    // Curve state
    pub vsol_at_entry: f64,
    pub fill_pct_at_entry: f32,
    pub trade_count_at_entry: u32,
    pub vsol_per_second: f64,
    pub unique_wallet_ratio: f32,
    pub first_sell_index: u32,
    pub max_wallet_concentration: f32,
    // Scores
    pub on_chain_score: u8,
    pub inflow_rate_score: u8,
    pub wallet_diversity_score: u8,
    pub fill_score: u8,
    pub sell_timing_score: u8,
    pub smart_money_score: i8,
    pub social_multiplier_bps: u16,
    pub final_score: u8,
    pub kelly_p: f32,
    // Execution
    pub position_size_sol: f64,
    pub entry_price: f64,
    pub target_price: f64,
    pub jito_tip_lamports: u64,
    pub bundle_sig: Option<String>,
    // Outcome
    pub outcome: SniperOutcome,
    pub pnl_sol: f64,
    pub landed_at_ms: Option<u64>,
    // Social metadata (training data)
    pub dev_pubkey: String,
    pub dev_tokens_launched: u16,
    pub dev_rug_count: u16,
    pub has_twitter: bool,
    pub has_telegram: bool,
    pub has_website: bool,
    pub has_image: bool,
    pub description_len: u16,
    pub pumpfun_reply_count: u16,
    pub social_enrichment_complete: bool,
}

#[derive(Serialize)]
pub enum SniperOutcome {
    BundleLanded,
    BundleRejected,
    BundleExpired,
    Paper,
}
```

---

## Decision Flowchart

```
TokenCreated → register BondingCurveState → fire async social enrichment

ShredStream trades update BondingCurveState in real-time

Every trade event for this mint triggers evaluation:

G0: is_mayhem_mode?        YES → DROP
G1: dev prebuy?            YES → DROP
G2: coordinated bundle?    YES → DROP  (≥2 wallets + >2 SOL in create slot)
G3: serial rugger?         YES → DROP  (≥5 tokens + >40% rug, or ≥10 tokens)
G4: vsol >= 25?            YES → DROP
G6: throwaway wallet?      YES → DROP
G7: concentration >15%?    YES → DROP
                           ALL PASS ↓

S1: inflow rate score     (0–30)
S2: wallet diversity      (0–25)
S3: curve fill            (0–15)
S4: sell pressure timing  (0–15)
S5: smart money           (-10–+15)
= on_chain_score (0–100)

threshold = 50 if velocity_available else 40
on_chain_score < threshold? → keep monitoring, re-evaluate next trade
                             ↓ threshold met

social_score_cache[mint] hit?
  YES: apply social_multiplier_bps
  NO:  use 10_000 (neutral) — enrichment arriving async
  social_multiplier_bps == 0? → DROP (blacklisted dev)

final_score = on_chain_score × social_multiplier_bps / 10_000
final_score < 40? → keep monitoring
                  ↓ final_score >= 40

Kelly sizing:
  p = score_to_probability(final_score)
       // 40→p=0.30, 55→p=0.45, 70→p=0.60, 85→p=0.70, 100→p=0.80
  b = 0.12
  kelly_f = (p*b - (1-p)) / b
  position_sol = wallet_sol × kelly_f × 0.5  // half-Kelly
  clamp(0.01, 0.10 SOL)

Bundle construction:
  TX1: buy position_sol on bonding curve
  TX2: sell all received tokens at target_vsol = sqrt(P1 × 1.12 × k)
  Submit to Jito

Log SniperTradeLog → data/sniper_trades.jsonl
Token age > 300s with no entry → drop tracking
```

---

## Bundle Construction

### TX1: Bonding Curve Buy
```
Program: 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
Discriminator: [102, 6, 61, 18, 1, 218, 235, 234]

Instruction data:
  [0..8]   discriminator
  [8..16]  amount: u64 (tokens to buy)
  [16..24] max_sol_cost: u64 (position_size_lamports × 1.15 for 15% slippage)

Accounts:
  [0]  global (PDA)
  [1]  fee_recipient
  [2]  mint
  [3]  bonding_curve (PDA: ["bonding-curve", mint])
  [4]  assoc_bonding_curve (ATA of bonding_curve for mint)
  [5]  associated_user (ATA of wallet for mint)
  [6]  user (wallet, signer)
  [7]  system_program
  [8]  token_program
  [9]  rent
  [10] event_authority (PDA)
  [11] program
```

### TX2: Bonding Curve Sell
```
Discriminator: [51, 230, 133, 164, 1, 127, 131, 173]

Instruction data:
  [0..8]   discriminator
  [8..16]  amount: u64 (tokens received from TX1)
  [16..24] min_sol_output: u64 (sol_out_net × 0.85 × 1e9)

Accounts:
  [0]  global (PDA)
  [1]  fee_recipient
  [2]  mint
  [3]  bonding_curve (PDA)
  [4]  assoc_bonding_curve
  [5]  associated_user
  [6]  user (wallet, signer)
  [7]  system_program
  [8]  associated_token_program
  [9]  token_program
  [10] event_authority (PDA)
  [11] program

TX2 parameters:
  target_vsol = sqrt(entry_price × 1.12 × K)
  sol_out_gross = target_vsol - vsol_after_tx1
  min_sol_output = (sol_out_gross × FEE_RATE × 0.85 × 1e9) as u64
```

### Jito Bundle
```
Bundle = [TX1_buy, TX2_sell, tip_tx]
Default tip: 100_000 lamports. Ladder up if congestion detected.
```

---

## Implementation Phases

### Phase 0: Data Logging (DO FIRST — 2-4 hours)
**Files:** `src/sniper/mod.rs`, `src/sniper/logger.rs`
- Log every `on_token_created()` to `data/sniper_create_log.jsonl`
- Wire ShredStream trades to BondingCurveState (vsol, trade_count, wallet_set, first_sell_index)
- Retroactively fill outcome fields (graduated, vsol_peak) after 300s or on migration event
- **This is the training dataset. Start it now.**

### Phase 1: BondingCurveState Tracker (1-2 days)
**Files:** `src/sniper/bc_tracker.rs`
- Full BondingCurveState struct + DashMap<[u8;32], BondingCurveState>
- ShredStream TradeEvent handler: update vsol_per_second, unique wallet set, first_sell_index, wallet_token_holdings
- Helius `accountSubscribe(bonding_curve)` for confirmed reserve state
- Wallet diversity computation, concentration check

### Phase 2: Tier 0 Gates (1 day)
**Files:** `src/sniper/gates.rs`
- G0–G4, G6, G7 implementation
- Unit tests: each gate with crafted BondingCurveState

### Phase 3: Tier 1 Scorer (1-2 days)
**Files:** `src/sniper/scorer.rs`
- S1–S5 with revised metrics
- `score_to_probability()` interpolation
- Paper mode evaluation loop

### Phase 4: Smart Wallet Seeds (0.5 days)
**Files:** `data/smart_wallets.json`, `data/dumper_wallets.json`, `scripts/refresh_smart_wallets.js`
- One-time: pull top-500 pump.fun PnL wallets from GMGN.ai or Helius indexing
- Weekly cron to refresh

### Phase 5: Social Enrichment (2-3 days)
**Files:** `src/sniper/social_enrichment.rs`
- SS1 (dev history) + SS4 (metadata) — available immediately at create
- SS3 (twitter presence) — from PumpPortal event
- SS2/SS5 time-gated (disabled <120s)
- Reweighted compute_multiplier with revised weights

### Phase 6: Bundle Constructor (2-3 days)
**Files:** `src/sniper/bundle_builder.rs`
- TX1/TX2 builders with discriminators and account layouts above
- Kelly sizing
- Jito submit via ExecutionContext
- SniperTradeLog JSONL on every attempt
- **Paper mode first.** Live only after Phase 0 data validates signal quality.

---

## Phase 0 Logging Schema

`data/sniper_create_log.jsonl`:
```json
{
  "ts_ms": 1743800000000,
  "mint": "base58...",
  "dev_pubkey": "base58...",
  "token_name": "DOGE2",
  "token_symbol": "DOGE2",
  "has_twitter": true,
  "has_telegram": false,
  "has_website": true,
  "has_image": true,
  "description_len": 42,
  "outcome_vsol_peak": null,
  "outcome_trade_count": null,
  "outcome_graduated": null,
  "outcome_graduation_ms": null,
  "outcome_graduation_steps": null
}
```

---

## Config Block (canary.json)

```json
"sniper": {
  "enabled": false,
  "paper_mode": true,
  "entry_threshold_with_velocity": 50,
  "entry_threshold_no_velocity": 40,
  "final_score_threshold": 40,
  "min_trades_for_velocity": 10,
  "skip_mayhem_mode": true,
  "max_vsol_entry": 25.0,
  "min_position_sol": 0.01,
  "max_position_sol": 0.10,
  "kelly_fraction": 0.5,
  "target_profit_mult": 1.12,
  "buy_slippage_pct": 15,
  "sell_slippage_pct": 15,
  "max_token_age_secs": 300,
  "jito_tip_lamports": 100000,
  "social_disable_before_age_secs": 120,
  "log_path": "data/sniper_trades.jsonl",
  "create_log_path": "data/sniper_create_log.jsonl",
  "smart_wallets_path": "data/smart_wallets.json",
  "dumper_wallets_path": "data/dumper_wallets.json",
  "social_cache_ttl_secs": 300,
  "dev_cache_ttl_secs": 3600,
  "pumpfun_api_rps": 5,
  "telegram_bot_token": null
}
```

---

## Signal Summary Table

| Signal | Tier | Source | Weight | Notes |
|--------|------|--------|--------|-------|
| Not Mayhem Mode | T0-G0 | Helius BC / PumpPortal | GATE | AI agent corrupts all T1 signals |
| No dev prebuy | T0-G1 | ShredStream + creator_map | GATE | Extended to first 5 trades + linked wallet check |
| No coordinated bundle | T0-G2 | ShredStream | GATE | ≥2 wallets + >2 SOL in create slot |
| Dev not serial rugger | T0-G3 | Helius dev cache | GATE | ≥5/40% OR ≥10 tokens = fail |
| Curve fill < 25 SOL | T0-G4 | Helius BC account | GATE | Math-based: >25 SOL needs >4.2 SOL follow-on |
| Creator not throwaway | T0-G6 | Helius balance | GATE | <0.05 SOL + no history = disposable wallet |
| No supply concentration | T0-G7 | ShredStream holdings | GATE | Single wallet >15% of buys |
| Inflow rate (vsol/s) | T1-S1 | ShredStream + Helius | 0–30 pts | 0.3 SOL/s sweet spot; spike=lower score |
| Wallet diversity | T1-S2 | ShredStream | 0–25 pts | Replaces bot detection; diversity = organic |
| Curve fill % | T1-S3 | Helius BC account | 0–15 pts | U-curve; sweet spot 2–8% fill |
| Sell pressure timing | T1-S4 | ShredStream | 0–15 pts | First sell index; replaces buy/sell ratio |
| Smart money | T1-S5 | Pre-seeded wallet set | -10–+15 pts | Negative signal for known dumpers |
| Dev wallet history | T2-SS1 | Helius DAS | 55% | Only reliable signal at mint |
| Metadata quality | T2-SS4 | PumpPortal event | 25% | Instant, no API call |
| Twitter presence | T2-SS3 | PumpPortal event | 10% | Presence only, weak but free |
| Pump.fun engagement | T2-SS2 | Pump.fun API | 5% | Disabled <120s |
| Telegram community | T2-SS5 | Telegram Bot API | 5% | Disabled <120s; pre-built = negative |

---

*Spec v1.2 | 2026-04-04 | Apollo*
*Sources: ArXiv 2602.14860, social-signal-layer-spec.md, BONDING_CURVE_SNIPER_IDEATION.md*
*Opus 4.6 quant review: 2026-04-04*
