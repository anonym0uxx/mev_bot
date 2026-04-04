# SniperEngine — Entry Signal System Spec

**Version:** 1.5  
**Date:** 2026-04-04  
**Author:** Apollo  
**Reviewed by:** Opus 4.6 quant (2026-04-04)  
**Status:** Ready for implementation  
**Goal:** Bonding curve sniper. Jito atomic bundle (buy+sell). 12%+ scalp target. Max loss per attempt = Jito tip + fees (~5000 lamports).

**v1.5 Changes (Opus quant review — position sizing):**
- Kelly sizing **dropped entirely** — proven mathematically inappropriate for near-zero-downside asymmetric payoff (Kelly threshold is p>90% at b=0.11; unreachable)
- Replaced with: Score-tier base sizes + zone-aware win-rate scalar + tiered drawdown protection
- Win rate tracked per zone (optimal 100-trade lookback, conditional 50-trade lookback) with blended fallback when zone has <20 trades
- Depth haircut added: real_sol < 5 → 0.7× (thin curve, self-impact on exit)
- Tiered drawdown protection: 3 tiers (mild/moderate/severe) based on HWM ratio — replaces binary cliff
- Partial bundle failure documented as real tail risk; emergency exit path required
- Full calibration roadmap (50/200/500/1000/2000 trade milestones)
- SniperEntrySignal + SniperTradeLog migration guide included
- KELLY_SIZING_REPLACEMENT.md source file (can be deleted after this commit)

**v1.4 Changes (Opus quant review — T1/T2 audit):**
- BREAKING: spec-wide vsol convention fixed. All thresholds now use `real_sol = vsol_raw - 30.0`. Graduation = 85 real_sol. fill_pct = real_sol / 85.0.
- S1: thresholds adjusted (sweet spot now 0.15 SOL/s, spike risk redefined at 1.5+ SOL/s)
- S3: rewritten to match G4 zones exactly (2–5 → 12pts, 5–15 → 15pts, 15–20 → 6pts)
- S4: early-entry handling split (trade_count 5-9 now uses relative indexing; consecutive buys with no sells = 10pts)
- SS1: missing middle band added (5-9 tokens, success 0.20-0.40 → +400 bps); N/A annotations for G3-blocked ranges
- SS1: success definition lowered from >20 SOL to ≥12 SOL real inflow (better reflects meaningful traction)
- SS4: weighting formula made explicit in code comment
- Final score floor: minimum on_chain_score of 30 required before social multiplier can push to entry threshold (prevents weak on-chain + high social from firing)

**v1.3 Changes (Opus quant review — G4 revision):**
- G4 rewritten: zone-aware (TooEarly/Optimal/Conditional/TooLate) replacing flat 25 SOL ceiling
- Entry zone: 2–15 SOL optimal, 15–20 SOL conditional (score ≥ 60), <2 SOL floor, >20 SOL fail
- Bonding curve follow-on math corrected (previous figures were wrong — 4.2 SOL at vSol=25 was an error; correct is 1.43 SOL)
- `curve_fill_ok: bool` → `CurveFillZone` enum + resolved bool (cleaner, debuggable)
- Config: `max_vsol_entry` lowered to 20, added `min_vsol_entry`, `conditional_vsol_entry_min`, `conditional_vsol_min_score`

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

Entry fires when: all Tier 0 gates pass AND `on_chain_score >= 30` (floor) AND `on_chain_score * social_multiplier_bps / 10_000 >= entry_threshold`.

**Key insight (Opus review):** At fresh mint (<60s), most Tier 2 social signals don't exist or are fabricated. The system is biased toward Tier 0 gates and Tier 1 on-chain signals for early entries. Social layer is weighted toward dev history (only reliably available signal at mint).

**ArXiv 2602.14860 finding:** vsol/second (inflow rate) > vsol/trade as a velocity metric. Moderate-fast inflow (0.3 SOL/s) scores higher than spike inflow (>1 SOL/s) because coordinated snipers create instant spikes then exit. Wallet diversity > bot ratio detection.

**Economic breakeven (ArXiv eq. 3):** p(+12% move) > entry_price²/exit_price² — at vSol=5 entry this is extremely low. Atomic bundle structure means max loss = Jito tip only, so the bar for positive EV is lower than intuition suggests.

---

## Bonding Curve Math (Reference)

### vsol Convention (spec-wide — apply everywhere)

```
vsol_raw:  actual virtual SOL reserve. Starts at 30.0 at mint. Graduation at 115.0.
real_sol:  real SOL inflow from buyers = vsol_raw - 30.0. Starts at 0.0. Graduation at 85.0.
fill_pct:  real_sol / 85.0  (NOT vsol_raw / 115.0)

ALL gate thresholds and score thresholds in this spec use real_sol unless
explicitly annotated "vsol_raw". When the spec says "vsol=5" it means
real_sol=5 (5 SOL of real inflow, vsol_raw=35).
```

### Curve Math

```
k = 30 × 1.073e9 = 3.219e10

Price P = vsol_raw² / k
       = (real_sol + 30)² / k

Buy:
  effective_sol_in = delta_sol × 0.9875  (1.25% fee)
  vsol_raw_new = vsol_raw + effective_sol_in
  tokens_out = vtok - k / vsol_raw_new

Sell P2 target (+12%):
  P2 = P1 × 1.12
  vsol_raw_exit = sqrt(P1 × 1.12 × k)
  sol_out = (vsol_raw_exit - vsol_raw_after_buy) × 0.9875
```

### Follow-on SOL needed for +12% (verified, 0.05 SOL position)

All values in real_sol (real inflow above virtual seed):

```
real_sol=2:   0.068 SOL  (EASY)   ← G4 floor
real_sol=3:   0.127 SOL  (EASY)
real_sol=5:   0.245 SOL  (EASY)   ← optimal zone
real_sol=7:   0.363 SOL  (EASY)
real_sol=9:   0.481 SOL  (EASY)
real_sol=10:  0.540 SOL  (OK)
real_sol=12:  0.658 SOL  (OK)
real_sol=15:  0.836 SOL  (OK)     ← conditional zone begins
real_sol=18:  1.013 SOL  (HARD)
real_sol=20:  1.131 SOL  (HARD)   ← G4 hard ceiling
real_sol=25:  1.426 SOL  (HARD)

Optimal entry zone: real_sol 2–15. Above 20, net follow-on > 1.13 SOL
required — EV-negative for non-graduating tokens given sell pressure from
earlier entrants taking profit.
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

### G4: Curve Fill — Entry Zone Gate

**Source:** Helius BC `accountSubscribe` (vsol_reserves) OR ShredStream trade sum.

**Logic (zone-aware):**

| vSol Range | Decision | Follow-on for +12% | Rationale |
|---|---|---|---|
| `< 2.0` | **SKIP** | < 0.07 SOL | Too early. 1–2 trades max. Signal pipeline has no data to score. |
| `2.0 – 14.99` | **PASS** | 0.07 – 0.78 SOL | Optimal zone. Follow-on ≤ 0.84 SOL is achievable organically. |
| `15.0 – 20.0` | **PASS if `on_chain_score ≥ 60`** | 0.84 – 1.13 SOL | Elevated zone. Follow-on achievable but needs above-average momentum. Gate on score. |
| `> 20.0` | **FAIL** | > 1.13 SOL | Too late. Net inflow required exceeds what non-graduating tokens sustain. EV-negative. |

**Why 20 SOL ceiling:** Tokens at 20+ SOL fill (~17%+ of graduation) that haven't built graduation momentum are statistically dying. Earlier entrants are taking profit — gross inflow must significantly exceed net 1.13 SOL target. Risk/reward flips EV-negative.

**Why 2 SOL floor:** Below vSol=2, the token has seen 1–2 trades. G1–G3 signal pipeline has no data. Follow-on is trivial but may have no organic buyers to provide it.

**Calibration note:** `on_chain_score ≥ 60` for the conditional zone should be recalibrated after 1000 trades. Target threshold: top 25–30% of scores seen in the 15–20 SOL range.

**Rust implementation:**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveFillZone {
    TooEarly,     // vsol < 2.0  → SKIP
    Optimal,      // 2.0 <= vsol < 15.0  → PASS
    Conditional,  // 15.0 <= vsol <= 20.0  → PASS if on_chain_score >= 60
    TooLate,      // vsol > 20.0  → FAIL
}

pub fn classify_curve_zone(vsol: f64) -> CurveFillZone {
    match vsol {
        v if v < 2.0   => CurveFillZone::TooEarly,
        v if v < 15.0  => CurveFillZone::Optimal,
        v if v <= 20.0 => CurveFillZone::Conditional,
        _              => CurveFillZone::TooLate,
    }
}

pub fn curve_fill_ok(zone: CurveFillZone, on_chain_score: u8) -> bool {
    match zone {
        CurveFillZone::TooEarly    => false,
        CurveFillZone::Optimal     => true,
        CurveFillZone::Conditional => on_chain_score >= 60,
        CurveFillZone::TooLate     => false,
    }
}
```

**Struct fields:** `curve_zone: CurveFillZone`, `curve_fill_ok: bool` (resolved after score gate)

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
**Metric: real_sol_per_second = real_sol / max(seconds_since_create, 1)**
**Where: real_sol = vsol_raw - 30.0** (see vsol convention above)

**Why real_sol/second beats vsol/trade:** Bots fragment orders into many small trades to look organic. Per-trade metrics reward fragmentation. Per-second measures actual capital commitment rate — harder to fake.

**Rate interpretation in G4 optimal zone (2–15 real_sol):**
- 0.02 SOL/s → reaches 15 SOL in ~12 min. Slow organic discovery.
- 0.05 SOL/s → reaches 15 SOL in 5 min. Steady build.
- 0.15 SOL/s → reaches 15 SOL in ~100s. Organic crowd rush. **Sweet spot.**
- 0.5 SOL/s → reaches 15 SOL in 30s. Aggressive momentum — viable for scalp.
- 1.5+ SOL/s → 15 SOL in <10s. Almost certainly coordinated — spike risk.

```
real_sol_per_second = real_sol / max(seconds_since_create, 1)

Score:
  real_sol/s >= 1.5  → 12 pts  (likely coordinated, high post-entry dump risk)
  real_sol/s >= 0.5  → 20 pts  (aggressive momentum, viable for 12% scalp)
  real_sol/s >= 0.15 → 30 pts  (sweet spot — organic crowd accumulation)
  real_sol/s >= 0.05 → 22 pts  (steady build)
  real_sol/s >= 0.02 → 12 pts  (slow — may stall before 12% move)
  real_sol/s <  0.02 → 5 pts   (stagnant)
```

**Sub-10-trade substitute (when trade_count < 10):**
```
Use first non-creator buy size as early proxy:
  first_buy_sol 0.1–0.5 SOL  → 8 pts  (human-sized, plausible organic)
  first_buy_sol 0.5–2.0 SOL  → 5 pts  (large, possible whale/bot)
  first_buy_sol > 2.0 SOL    → 3 pts  (single large actor, spike risk)
  first_buy_sol < 0.01 SOL   → 0 pts  (dust/bot test)
  no trades yet               → 0 pts
Entry threshold drops to 40 (from 50) when trade_count < 10.
```

**Rust fields:** `inflow_rate_score: u8`, `real_sol_per_second: f64`  
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

### S3: Curve Position Score (0–15 pts)
**Metric: real_sol (= vsol_raw - 30.0). Zones aligned exactly with G4.**

**Why align with G4:** S3 scoring should reward the same zone G4 identifies as optimal, and penalize the conditional zone to make G4's score≥60 threshold harder to reach without strong other signals.

```
fill_pct = real_sol / 85.0  (85.0 = real SOL to graduate)

real_sol < 2.0          → 0 pts   (G4 SKIP — should never score, defensive)
real_sol 2.0–5.0        → 12 pts  (early optimal — high upside, thinner data)
real_sol 5.0–15.0       → 15 pts  (peak optimal zone — best risk/reward)
real_sol 15.0–20.0      → 6 pts   (conditional zone — G4 requires score≥60)
real_sol > 20.0         → 0 pts   (G4 FAIL — dead code, defensive)
```

**Zone rationale:**
- 2–5 real_sol: follow-on ≤ 0.25 SOL needed — favorable math. Slightly lower pts than 5–15 because thinner data.
- 5–15 real_sol: peak. Enough trades to validate momentum quality, follow-on 0.25–0.84 SOL.
- 15–20 real_sol: conditional. S3 contributes only 6 pts here, making it harder to reach entry threshold without strong S1/S2 scores. This correctly penalizes late entry.

**Rust field:** `fill_score: u8`, `fill_zone: CurveFillZone` (shared with G4)

---

### S4: Sell Pressure Timing Score (0–15 pts)
**Metric: index of first sell trade. Early-entry aware.**

**Why timing beats ratio:** First N trades are almost always buys — nobody has tokens to sell yet. A 1.0 buy ratio at 5 trades is noise. What matters: when does the first sell appear, and is it a partial take or a full dump?

**Early-entry handling:** At 2–5 real_sol (G4 floor), tokens have 4–10 trades. Absolute index thresholds don't work at this scale — use relative indexing for small trade counts.

```
trade_count < 5:
  → 3 pts flat (too little data, don't reward or punish)

trade_count 5–9, no sells observed:
  → 10 pts (all buys in early window = bullish sustained interest)

trade_count 5–9, first sell exists:
  first_sell_index > trade_count × 0.70  → 8 pts  (late relative — bullish)
  first_sell_index > trade_count × 0.40  → 5 pts  (mid relative)
  first_sell_index ≤ trade_count × 0.40  → 1 pt   (early relative — bearish)

trade_count >= 10:
  first_sell_index > 15  → 15 pts  (strong sustained buying)
  first_sell_index 10–15 → 12 pts
  first_sell_index 5–10  → 7 pts
  first_sell_index < 5   → 0 pts   (immediate dump = dev/sniper exiting)

Bonus (additive, applied after base score):
  first_sell_pct < 0.20  → +3 pts  (partial take = still bullish on position)
  first_sell_pct > 0.80  → -5 pts  (full exit = exit liquidity signal)

Floor: 0 pts total
```

**Rust fields:** `sell_timing_score: u8`, `first_sell_index: u32`, `first_sell_pct: f32`

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
dev_is_blacklisted (from our own loss log)
  → social_multiplier = 0 (hard veto — overrides everything)

dev_tokens_launched == 0:
  → +500 bps (first-time dev — not automatically bad, slight positive)

dev_tokens_launched 1–4, rug_rate < 0.20:
  → +2000 bps (track record: mostly clean launches)

dev_tokens_launched 1–4, rug_rate 0.20–0.40:
  → neutral (0 bps delta)

dev_tokens_launched 1–4, rug_rate > 0.40:
  → -2000 bps (>2 in 5 rugged)

dev_tokens_launched 5–9, rug_rate > 0.40:
  → N/A (G3 blocks: ≥5 tokens AND rug_rate > 0.40 → gate fail)

dev_tokens_launched 5–9, success_rate > 0.40:
  → +1500 bps (consistent launcher, decent success record)

dev_tokens_launched 5–9, success_rate 0.20–0.40:
  → +400 bps (some track record, not proven — mild positive)

dev_tokens_launched 5–9, success_rate < 0.20:
  → -2000 bps (launched several, almost none succeeded)

dev_tokens_launched ≥ 10:
  → N/A (G3 blocks: ≥10 tokens → gate fail regardless of rate)
```

**"Success" definition:** Token graduated OR reached ≥ 12 real_sol (real SOL inflow). 12 SOL represents meaningful traction within G4's optimal zone — deep enough to show real demand, not just a brief pump.

*Note: Previous definition used >20 SOL, which was the G4 ceiling. Lowered to 12 SOL to better reflect genuine demand vs near-graduation tokens.*

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

// Explicit formula:
// final_multiplier_bps = 10_000
//   + (dev_delta × W_DEV / 100)    // 55% — only reliable signal at mint
//   + (met_delta × W_MET / 100)    // 25% — instant, in create event
//   + (twi_delta × W_TWI / 100)    // 10% — presence only, weak but free
//   + (eng_delta × W_ENG / 100)    //  5% — disabled <120s
//   + (tg_delta  × W_TG  / 100)    //  5% — disabled <120s
// Result clamped to [5_000, 20_000] = 0.5× to 2.0× multiplier
//
// SS4 max impact: +2200 bps raw × 0.25 = +550 bps effective (~5.5% boost)
// SS4 max drag:  -1500 bps raw × 0.25 = -375 bps effective (~3.75% drag)
fn compute_multiplier(s: &SocialScore) -> u16 {
    if s.dev_is_blacklisted { return 0; }

    // Disable time-sensitive signals for tokens age < 120s
    let eng = if s.token_age_secs < 120 { 10_000i32 } else { s.engagement_score_bps as i32 };
    let tg  = if s.token_age_secs < 120 {
        // Pre-built large community at mint = coordinated launch = weak negative
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
// FLOOR CHECK: on_chain_score must be >= 30 before social can push to threshold.
// Prevents: weak on_chain=27 × social=1.5× = 40.5 → entry fires on noise.
on_chain_score < 30? → keep monitoring (social multiplier cannot rescue this)
                      ↓ on_chain_score >= 30

final_score < 40? → keep monitoring
                  ↓ final_score >= 40

Position sizing: → see §Position Sizing below for full spec
  position_sol = SniperSizer::compute_position_size(
                   final_score, curve_zone, real_sol, wallet_sol, &tracker)
  // Bootstrap (trades < 50): flat 0.02 SOL
  // Post-bootstrap: score-tier base × WR scalar × zone mult × depth mult × DD mult
  // clamp(0.01, 0.10 SOL)

Bundle construction:
  TX1: buy position_sol on bonding curve
  TX2: sell all received tokens at target_vsol = sqrt(P1 × 1.12 × k)
  Submit to Jito

Log SniperTradeLog → data/sniper_trades.jsonl
Token age > 300s with no entry → drop tracking
```

---

## Position Sizing — Score-Tier with Adaptive Win-Rate Scaling

### Why Not Kelly

The previous spec used standard Kelly: `f = (p×b - (1-p)) / b` with b ≈ 0.11 (net +12% target minus fees). This is **mathematically inappropriate** for this payoff structure, and the worked examples in v1.4 proved it — Kelly is negative at every realistic win rate.

**The math is unambiguous.** Kelly turns positive when `p > 1/(1+b)`. For b=0.11, that threshold is p > 0.9009 — i.e., a 90.1% win rate. No pump.fun sniper achieves this. At b=0.09 (thin-curve haircut), the threshold is p > 0.9174. Even worse.

**Why the asymmetric-loss Kelly doesn't help either.** The generalized formula `f = (p×b - (1-p)×L) / (b×L)` accounts for partial loss L < 1. Jito atomic bundles have L ≈ 0 when they work correctly (cost = tip only on rejection). But as L → 0, Kelly → ∞ — it recommends infinite leverage. This is the degenerate case: Kelly assumes you can lose your bet, and when you can't, it loses its regularizing force.

**Even with realistic L, Kelly is degenerate here.** Suppose we set L=0.5 (partial bundle failure: TX1 lands, TX2 fails, you exit at 50% loss via emergency sell). Then the threshold becomes `p > L/(b+L) = 0.5/(0.11+0.5) = 0.820`. Still unreachable. At L=0.3: threshold = 0.3/0.41 = 0.732. At L=0.1: threshold = 0.1/0.21 = 0.476. Only at L ≤ 0.1 does Kelly turn positive at achievable win rates — but at L=0.1 it recommends 40%+ of bankroll per trade, which is suicide for a high-frequency bot.

**Conclusion:** Kelly is not just impractical here — it is the wrong framework. The payoff structure (small positive b, near-zero L, high frequency) means Kelly either vetoes everything or recommends reckless sizing. Neither is useful.

### Chosen Framework: Score-Tier Base Sizing + Win-Rate Scalar

This is a **transparent, tunable, data-driven** system. Every parameter maps to an observable quantity. No assumptions about theoretical loss fraction L that we can't measure. The system bootstraps safely and adapts as trade data accumulates.

**Design principles:**
1. **Score drives base size.** Higher conviction signals deserve larger positions. The scoring system (Tier 0 gates + Tier 1 on-chain + Tier 2 social) already does the hard work of quantifying entry quality.
2. **Win rate scales size up or down.** After bootstrap, rolling performance modulates the base — rewarding consistent edge, throttling during drawdowns.
3. **Zone caps enforce curve-depth awareness.** Thin curves get smaller bets regardless of score.
4. **Drawdown protection is wallet-level, not per-trade.** HWM-based auto-reduction prevents ruin spirals.
5. **Every parameter is config-driven and recalibratable from trade logs.**

---

### Phase 1: Bootstrap (first 50 trades)

No adaptive sizing. Flat probe.

```
if trade_history.total_trades < BOOTSTRAP_TRADES:
    return BOOTSTRAP_SIZE_SOL  // 0.02 SOL
```

**Rationale:** 50 trades at 0.02 SOL = 1.0 SOL max exposure. Builds the dataset needed for win-rate estimation. No sizing logic runs — the bot is learning, not optimizing.

**Bootstrap trades count ALL outcomes:** bundle landed (win), bundle landed (loss — TX2 didn't hit target), bundle rejected, bundle expired. Only landed-and-profitable counts as a win.

---

### Phase 2: Adaptive Score-Tier Sizing (trades 50+)

#### Step 1: Score-Tier Base Size

The `final_score` (0–100, computed from on-chain score × social multiplier) maps to a base position size:

```
final_score 80–100 (conviction)     → base = 0.07 SOL
final_score 65–79  (normal)         → base = 0.04 SOL
final_score 50–64  (probe)          → base = 0.02 SOL
final_score 40–49  (floor probe)    → base = 0.01 SOL
final_score < 40                    → no entry (should not reach sizing)
```

**Why these sizes:** Max position is 0.10 SOL. The conviction tier (0.07) leaves room for the win-rate scalar to push to 0.10 at peak performance. The floor probe (0.01) is the minimum meaningful position — below this, Jito tip cost dominates and the trade is EV-negative.

#### Step 2: Win-Rate Scalar

After bootstrap, compute rolling win rate from the last 100 trades (or all post-bootstrap trades if fewer than 100). Win rate is tracked **per zone** (optimal vs conditional) because these have structurally different success rates.

```
// Zone-specific rolling win rate
p_optimal     = wins_in_optimal_zone / trades_in_optimal_zone     (lookback: last 100 optimal-zone trades)
p_conditional = wins_in_conditional_zone / trades_in_conditional_zone (lookback: last 50 conditional-zone trades)

// Use the zone-appropriate rate for this trade
p = if curve_zone == Optimal { p_optimal } else { p_conditional }

// Minimum sample size: use blended rate until zone has 20+ trades
if zone_trade_count < 20:
    p = blended_win_rate  // all trades, both zones

// Win-rate scalar mapping
WR_scalar:
  p >= 0.55  → 1.40  (strong edge confirmed — scale up)
  p >= 0.45  → 1.00  (baseline — no adjustment)
  p >= 0.35  → 0.70  (underperforming — reduce exposure)
  p <  0.35  → 0.50  (distress — minimum viable sizing)
```

**Why zone-separated tracking:** Conditional-zone trades (real_sol 15–20) face harder follow-on math (0.84–1.13 SOL needed for +12%). Their win rate will be structurally lower. Blending them with optimal-zone trades contaminates the signal — a 45% blended rate might mask 50% optimal / 30% conditional, which should trigger different sizing for each.

**Why 100/50 lookback:** 100 trades for optimal zone (high volume, stable estimate). 50 for conditional zone (lower volume, don't wait too long to adapt). Both use VecDeque with FIFO eviction.

#### Step 3: Zone Multiplier

```
Optimal zone     (real_sol 2–15):   zone_mult = 1.0
Conditional zone (real_sol 15–20):  zone_mult = 0.6, hard cap 0.06 SOL
```

**Why 0.6× and 0.06 cap:** At real_sol=18, follow-on needed is 1.013 SOL. The curve is deep enough that our position doesn't move it much, but the probability of organic follow-through is lower. Capping at 0.06 limits conditional-zone exposure to 60% of max — the trade is speculative by definition (G4 required score ≥ 60 to even enter).

#### Step 4: Depth Haircut (early optimal zone)

At real_sol < 5, our buy moves the curve measurably. A 0.07 SOL buy in a 3 SOL curve is 2.3% of total reserves — our own price impact eats into the +12% exit target.

```
if real_sol < 5.0:
    depth_mult = 0.7  // haircut for thin depth / self-impact
else:
    depth_mult = 1.0
```

#### Step 5: Drawdown Protection

Wallet-level protection against ruin spirals:

```
// Track session high water mark (HWM) — highest wallet balance seen this session
hwm = max(hwm, wallet_sol)

// Drawdown tiers
if wallet_sol < 0.05 SOL:
    return 0.01 SOL  // hard floor — survival mode, probes only

if wallet_sol < hwm × 0.70:
    dd_mult = 0.40  // severe drawdown (>30% from peak) — aggressive reduction

else if wallet_sol < hwm × 0.80:
    dd_mult = 0.60  // moderate drawdown (>20% from peak) — significant reduction

else if wallet_sol < hwm × 0.90:
    dd_mult = 0.80  // mild drawdown (>10% from peak) — slight reduction

else:
    dd_mult = 1.00  // no drawdown
```

**Why tiered (not binary):** A binary 50% cut at -20% creates a cliff — the bot swings between full size and half size with every small balance fluctuation near the threshold. Tiered reduction is smoother and more stable.

#### Step 6: Final Computation

```
raw_size    = base_size × WR_scalar × zone_mult × depth_mult × dd_mult
position_sol = clamp(raw_size, MIN_POSITION_SOL, MAX_POSITION_SOL)

// Conditional zone hard cap (after clamp, as additional ceiling)
if curve_zone == Conditional:
    position_sol = min(position_sol, CONDITIONAL_ZONE_CAP_SOL)

// Never bet more than 20% of wallet
position_sol = min(position_sol, wallet_sol × 0.20)
```

---

### Win-Rate Tracking Implementation

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeResult {
    Win,          // Bundle landed, TX2 executed at or above target
    Loss,         // Bundle landed, TX2 missed target (or partial bundle failure)
    Rejected,     // Bundle rejected by Jito — cost = tip only
    Expired,      // Bundle expired — cost = tip only
}

impl TradeResult {
    pub fn is_win(&self) -> bool { matches!(self, TradeResult::Win) }
    pub fn is_landed(&self) -> bool { matches!(self, TradeResult::Win | TradeResult::Loss) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizingTradeRecord {
    pub result: TradeResult,
    pub final_score: u8,
    pub curve_zone: CurveFillZone,
    pub real_sol_at_entry: f64,
    pub position_sol: f64,
    pub pnl_sol: f64,
    pub timestamp_ms: u64,
}

#[derive(Debug)]
pub struct WinRateTracker {
    /// Optimal-zone trades, FIFO, max 100
    pub optimal_trades: VecDeque<SizingTradeRecord>,
    /// Conditional-zone trades, FIFO, max 50
    pub conditional_trades: VecDeque<SizingTradeRecord>,
    /// Total trades across all zones (monotonic counter)
    pub total_trades: u64,
    /// Session high water mark (wallet SOL balance)
    pub session_hwm_sol: f64,
}

impl WinRateTracker {
    pub fn new() -> Self {
        Self {
            optimal_trades: VecDeque::with_capacity(100),
            conditional_trades: VecDeque::with_capacity(50),
            total_trades: 0,
            session_hwm_sol: 0.0,
        }
    }

    pub fn record_trade(&mut self, trade: SizingTradeRecord) {
        self.total_trades += 1;
        match trade.curve_zone {
            CurveFillZone::Optimal => {
                self.optimal_trades.push_back(trade);
                if self.optimal_trades.len() > 100 {
                    self.optimal_trades.pop_front();
                }
            }
            CurveFillZone::Conditional => {
                self.conditional_trades.push_back(trade);
                if self.conditional_trades.len() > 50 {
                    self.conditional_trades.pop_front();
                }
            }
            _ => {} // TooEarly/TooLate should never reach sizing
        }
    }

    pub fn update_hwm(&mut self, wallet_sol: f64) {
        if wallet_sol > self.session_hwm_sol {
            self.session_hwm_sol = wallet_sol;
        }
    }

    /// Zone-specific win rate. Falls back to blended if zone has < min_zone_trades.
    pub fn win_rate(&self, zone: CurveFillZone, min_zone_trades: usize) -> f64 {
        let (zone_wins, zone_total) = match zone {
            CurveFillZone::Optimal => self.zone_stats(&self.optimal_trades),
            CurveFillZone::Conditional => self.zone_stats(&self.conditional_trades),
            _ => (0, 0),
        };

        if zone_total >= min_zone_trades {
            zone_wins as f64 / zone_total as f64
        } else {
            // Blended fallback
            let (ow, ot) = self.zone_stats(&self.optimal_trades);
            let (cw, ct) = self.zone_stats(&self.conditional_trades);
            let total = ot + ct;
            if total == 0 { 0.0 } else { (ow + cw) as f64 / total as f64 }
        }
    }

    fn zone_stats(&self, trades: &VecDeque<SizingTradeRecord>) -> (usize, usize) {
        let wins = trades.iter().filter(|t| t.result.is_win()).count();
        (wins, trades.len())
    }
}
```

---

### Sizer Implementation

```rust
use std::collections::VecDeque;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizerConfig {
    // Bootstrap
    pub bootstrap_trades: u64,
    pub bootstrap_size_sol: f64,
    // Base sizes per score tier
    pub base_conviction_sol: f64,    // score 80–100
    pub base_normal_sol: f64,        // score 65–79
    pub base_probe_sol: f64,         // score 50–64
    pub base_floor_probe_sol: f64,   // score 40–49
    // Win-rate scalar thresholds
    pub wr_strong_threshold: f64,    // 0.55
    pub wr_strong_scalar: f64,       // 1.40
    pub wr_baseline_threshold: f64,  // 0.45
    pub wr_baseline_scalar: f64,     // 1.00
    pub wr_weak_threshold: f64,      // 0.35
    pub wr_weak_scalar: f64,         // 0.70
    pub wr_distress_scalar: f64,     // 0.50
    // Zone
    pub conditional_zone_mult: f64,  // 0.60
    pub conditional_zone_cap_sol: f64, // 0.06
    pub early_depth_mult: f64,       // 0.70
    pub early_depth_threshold: f64,  // 5.0 real_sol
    // Drawdown protection
    pub dd_mild_threshold: f64,      // 0.90
    pub dd_mild_mult: f64,           // 0.80
    pub dd_moderate_threshold: f64,  // 0.80
    pub dd_moderate_mult: f64,       // 0.60
    pub dd_severe_threshold: f64,    // 0.70
    pub dd_severe_mult: f64,         // 0.40
    pub survival_wallet_sol: f64,    // 0.05
    // Limits
    pub min_position_sol: f64,       // 0.01
    pub max_position_sol: f64,       // 0.10
    pub max_wallet_pct: f64,         // 0.20
    // Win-rate tracking
    pub min_zone_trades_for_split: usize, // 20
}

impl Default for SizerConfig {
    fn default() -> Self {
        Self {
            bootstrap_trades: 50,
            bootstrap_size_sol: 0.02,
            base_conviction_sol: 0.07,
            base_normal_sol: 0.04,
            base_probe_sol: 0.02,
            base_floor_probe_sol: 0.01,
            wr_strong_threshold: 0.55,
            wr_strong_scalar: 1.40,
            wr_baseline_threshold: 0.45,
            wr_baseline_scalar: 1.00,
            wr_weak_threshold: 0.35,
            wr_weak_scalar: 0.70,
            wr_distress_scalar: 0.50,
            conditional_zone_mult: 0.60,
            conditional_zone_cap_sol: 0.06,
            early_depth_mult: 0.70,
            early_depth_threshold: 5.0,
            dd_mild_threshold: 0.90,
            dd_mild_mult: 0.80,
            dd_moderate_threshold: 0.80,
            dd_moderate_mult: 0.60,
            dd_severe_threshold: 0.70,
            dd_severe_mult: 0.40,
            survival_wallet_sol: 0.05,
            min_position_sol: 0.01,
            max_position_sol: 0.10,
            max_wallet_pct: 0.20,
            min_zone_trades_for_split: 20,
        }
    }
}

pub struct SniperSizer {
    pub config: SizerConfig,
}

impl SniperSizer {
    pub fn new(config: SizerConfig) -> Self {
        Self { config }
    }

    /// Main sizing entry point. Returns position size in SOL, or 0.0 for "skip."
    /// Caller must verify final_score >= 40 and all gates passed before calling.
    pub fn compute_position_size(
        &self,
        final_score: u8,
        curve_zone: CurveFillZone,
        real_sol: f64,
        wallet_sol: f64,
        tracker: &WinRateTracker,
    ) -> f64 {
        let cfg = &self.config;

        // Phase 1: Bootstrap
        if tracker.total_trades < cfg.bootstrap_trades {
            return cfg.bootstrap_size_sol;
        }

        // Survival mode: wallet nearly empty
        if wallet_sol < cfg.survival_wallet_sol {
            return cfg.min_position_sol;
        }

        // Step 1: Score-tier base size
        let base = self.score_tier_base(final_score);

        // Step 2: Win-rate scalar (zone-aware)
        let p = tracker.win_rate(curve_zone, cfg.min_zone_trades_for_split);
        let wr_scalar = self.wr_scalar(p);

        // Step 3: Zone multiplier
        let zone_mult = match curve_zone {
            CurveFillZone::Conditional => cfg.conditional_zone_mult,
            _ => 1.0,
        };

        // Step 4: Depth haircut
        let depth_mult = if real_sol < cfg.early_depth_threshold {
            cfg.early_depth_mult
        } else {
            1.0
        };

        // Step 5: Drawdown multiplier
        let dd_mult = self.drawdown_mult(wallet_sol, tracker.session_hwm_sol);

        // Step 6: Combine and clamp
        let raw = base * wr_scalar * zone_mult * depth_mult * dd_mult;
        let mut size = raw.clamp(cfg.min_position_sol, cfg.max_position_sol);

        // Conditional zone hard cap
        if curve_zone == CurveFillZone::Conditional {
            size = size.min(cfg.conditional_zone_cap_sol);
        }

        // Wallet percentage cap
        size = size.min(wallet_sol * cfg.max_wallet_pct);

        // Final floor
        size.max(cfg.min_position_sol)
    }

    fn score_tier_base(&self, final_score: u8) -> f64 {
        let cfg = &self.config;
        match final_score {
            s if s >= 80 => cfg.base_conviction_sol,
            s if s >= 65 => cfg.base_normal_sol,
            s if s >= 50 => cfg.base_probe_sol,
            _            => cfg.base_floor_probe_sol,
        }
    }

    fn wr_scalar(&self, p: f64) -> f64 {
        let cfg = &self.config;
        if p >= cfg.wr_strong_threshold {
            cfg.wr_strong_scalar
        } else if p >= cfg.wr_baseline_threshold {
            cfg.wr_baseline_scalar
        } else if p >= cfg.wr_weak_threshold {
            cfg.wr_weak_scalar
        } else {
            cfg.wr_distress_scalar
        }
    }

    fn drawdown_mult(&self, wallet_sol: f64, hwm: f64) -> f64 {
        let cfg = &self.config;
        if hwm <= 0.0 { return 1.0; }
        let ratio = wallet_sol / hwm;
        if ratio < cfg.dd_severe_threshold {
            cfg.dd_severe_mult
        } else if ratio < cfg.dd_moderate_threshold {
            cfg.dd_moderate_mult
        } else if ratio < cfg.dd_mild_threshold {
            cfg.dd_mild_mult
        } else {
            1.0
        }
    }
}
```

---

### Config Block

```json
"sniper_sizing": {
  "bootstrap_trades": 50,
  "bootstrap_size_sol": 0.02,
  "score_tiers": {
    "conviction":  { "min_score": 80, "base_sol": 0.07 },
    "normal":      { "min_score": 65, "base_sol": 0.04 },
    "probe":       { "min_score": 50, "base_sol": 0.02 },
    "floor_probe": { "min_score": 40, "base_sol": 0.01 }
  },
  "win_rate_scalars": {
    "strong":   { "min_wr": 0.55, "scalar": 1.40 },
    "baseline": { "min_wr": 0.45, "scalar": 1.00 },
    "weak":     { "min_wr": 0.35, "scalar": 0.70 },
    "distress": { "min_wr": 0.00, "scalar": 0.50 }
  },
  "zone": {
    "conditional_mult": 0.60,
    "conditional_cap_sol": 0.06,
    "early_depth_mult": 0.70,
    "early_depth_threshold_real_sol": 5.0
  },
  "drawdown_protection": {
    "mild":     { "threshold_pct": 0.90, "mult": 0.80 },
    "moderate": { "threshold_pct": 0.80, "mult": 0.60 },
    "severe":   { "threshold_pct": 0.70, "mult": 0.40 },
    "survival_wallet_sol": 0.05
  },
  "limits": {
    "min_position_sol": 0.01,
    "max_position_sol": 0.10,
    "max_wallet_pct": 0.20
  },
  "tracking": {
    "optimal_zone_lookback": 100,
    "conditional_zone_lookback": 50,
    "min_zone_trades_for_split": 20
  }
}
```

---

### Worked Examples

**Example A — Bootstrap Trade (trade #23)**
```
Input:
  total_trades = 23 (< 50 bootstrap threshold)
  final_score = 72
  real_sol = 8.0, curve_zone = Optimal
  wallet_sol = 2.5 SOL

Computation:
  total_trades < 50 → return bootstrap_size_sol

Result: 0.02 SOL
  No sizing logic applied. Building dataset.
```

**Example B — Post-Bootstrap, Low Win Rate (trade #78)**
```
Input:
  total_trades = 78
  final_score = 58 (probe tier → base = 0.02 SOL)
  real_sol = 11.0, curve_zone = Optimal
  wallet_sol = 1.8 SOL, session_hwm = 2.5 SOL

Win rate:
  optimal_zone trades = 62, wins = 22 → p = 22/62 = 0.355
  (>= 20 zone trades, so zone-specific rate is used)

Computation:
  base        = 0.02 SOL  (probe tier, score 58)
  wr_scalar   = 0.70      (p=0.355, in [0.35, 0.45) → weak)
  zone_mult   = 1.0       (optimal zone)
  depth_mult  = 1.0       (real_sol=11 >= 5.0)
  dd_mult     = 0.60      (1.8/2.5 = 0.72, < 0.80 moderate threshold)
  
  raw = 0.02 × 0.70 × 1.0 × 1.0 × 0.60 = 0.0084
  clamp(0.0084, 0.01, 0.10) = 0.01 SOL  (floor)
  wallet cap: min(0.01, 1.8 × 0.20) = 0.01

Result: 0.01 SOL
  Low win rate + moderate drawdown → minimum sizing.
  Bot is still participating but protecting capital.
```

**Example C — Post-Bootstrap, High Win Rate, High Score (trade #185)**
```
Input:
  total_trades = 185
  final_score = 87 (conviction tier → base = 0.07 SOL)
  real_sol = 9.5, curve_zone = Optimal
  wallet_sol = 3.8 SOL, session_hwm = 3.8 SOL

Win rate:
  optimal_zone trades = 100 (full lookback), wins = 58 → p = 0.58
  (>= 20 zone trades, zone-specific rate used)

Computation:
  base        = 0.07 SOL  (conviction tier, score 87)
  wr_scalar   = 1.40      (p=0.58, >= 0.55 → strong)
  zone_mult   = 1.0       (optimal zone)
  depth_mult  = 1.0       (real_sol=9.5 >= 5.0)
  dd_mult     = 1.0       (wallet_sol == hwm, no drawdown)
  
  raw = 0.07 × 1.40 × 1.0 × 1.0 × 1.0 = 0.098
  clamp(0.098, 0.01, 0.10) = 0.098 SOL
  wallet cap: min(0.098, 3.8 × 0.20) = 0.098

Result: 0.098 SOL
  Near-max sizing. High conviction signal + confirmed edge + no drawdown.
```

**Example D — Conditional Zone Entry (trade #130)**
```
Input:
  total_trades = 130
  final_score = 74 (normal tier → base = 0.04 SOL)
  real_sol = 17.2, curve_zone = Conditional
  wallet_sol = 2.1 SOL, session_hwm = 2.3 SOL

Win rate:
  conditional_zone trades = 14 (< 20 threshold)
  Fallback to blended: 100 optimal + 14 conditional = 114 trades, 51 wins → p = 0.447

Computation:
  base        = 0.04 SOL  (normal tier, score 74)
  wr_scalar   = 0.70      (p=0.447, in [0.35, 0.45) → weak)
  zone_mult   = 0.60      (conditional zone)
  depth_mult  = 1.0       (real_sol=17.2 >= 5.0)
  dd_mult     = 1.0       (2.1/2.3 = 0.913 >= 0.90 → no drawdown)
  
  raw = 0.04 × 0.70 × 0.60 × 1.0 × 1.0 = 0.0168
  clamp(0.0168, 0.01, 0.10) = 0.0168
  conditional cap: min(0.0168, 0.06) = 0.0168
  wallet cap: min(0.0168, 2.1 × 0.20) = 0.0168

Result: 0.017 SOL (rounded)
  Conditional zone + weak blended WR → small position despite decent score.
  Zone-separated tracking will give better signal once 20+ conditional trades accumulate.
```

---

### Sizing Output Table (Quick Reference)

Assumes: no drawdown, no depth haircut, optimal zone, wallet_sol > 1 SOL.

| Score Tier | Base | WR < 35% | WR 35–45% | WR 45–55% | WR ≥ 55% |
|---|---|---|---|---|---|
| 80–100 (conviction) | 0.07 | 0.035 | 0.049 | 0.070 | 0.098 |
| 65–79 (normal) | 0.04 | 0.020 | 0.028 | 0.040 | 0.056 |
| 50–64 (probe) | 0.02 | 0.010 | 0.014 | 0.020 | 0.028 |
| 40–49 (floor) | 0.01 | 0.010 | 0.010 | 0.010 | 0.014 |

*Floor at 0.01 SOL applies to all values below it. Max at 0.10 SOL caps the top.*

---

### Partial Bundle Failure: The Real Tail Risk

Jito atomic bundles are designed to be all-or-nothing, but edge cases exist where TX1 lands and TX2 does not (e.g., TX2 slippage check fails because another large buy moved the curve between our bundle's TX1 and TX2, making our sell price exceed the min_sol_output). In this case:

- **We hold tokens we didn't want.** This is the real tail risk — not the Jito tip.
- **Emergency exit:** The system must detect partial bundle outcomes (TX1 confirmed, TX2 failed/dropped) and submit an emergency sell at market within the same slot or next slot.
- **Expected loss on emergency exit:** ~5–15% of position depending on curve depth and sell pressure. This is the actual L for risk modeling purposes.

**How this interacts with sizing:** Partial bundle failures are rare (Jito's bundle atomicity is strong) but not zero. The sizing system handles this through:
1. **Max position cap (0.10 SOL):** Even worst-case partial failure loses at most 0.015 SOL (15% of 0.10).
2. **Wallet percentage cap (20%):** Prevents any single partial failure from being catastrophic.
3. **Drawdown protection:** If partial failures cluster, the tiered drawdown multipliers kick in.

**Tracking:** Partial bundle failures should be logged as `TradeResult::Loss` with the actual PnL from the emergency exit. This feeds the win-rate tracker and naturally reduces future sizing if the issue is systematic.

---

### Calibration Roadmap

After deployment, recalibrate these parameters in order:

| Trades | Calibration Action |
|---|---|
| 50 | Bootstrap complete. Begin adaptive sizing. Review initial win rates. |
| 200 | First calibration. Check: are score tiers predictive? Is win rate meaningfully different between score 50–64 vs 80+? Adjust base sizes if needed. |
| 500 | Zone calibration. Enough conditional-zone trades to verify p_conditional vs p_optimal. Adjust conditional_zone_mult if actual ratio differs from assumed 0.85×. |
| 1000 | Depth calibration. Review slippage data at real_sol < 5 vs real_sol 5–15. Adjust early_depth_mult based on actual price impact. |
| 2000 | WR scalar calibration. Are the scalar breakpoints (0.35/0.45/0.55) at the right thresholds? Run a sweep and find the actual WR percentiles. |

**Config is JSON.** All parameters are hot-reloadable. No code changes needed for recalibration — just edit `canary.json` and the sizer picks up new values on next trade.

---

### Migration from Kelly Section

**Removed:** `kelly_fraction`, `prior_p`, `prior_weight`, `kelly_veto_floor_score`, `b_early_optimal`, `b_peak_optimal`, `b_conditional`, `conditional_p_multiplier`, `score_tier_multipliers` (Kelly-multiplied).

**Added:** Score-tier base sizes (SOL), win-rate scalar thresholds, zone multipliers, depth haircut, tiered drawdown protection, zone-separated win-rate tracking with configurable lookback windows.

**SniperEntrySignal struct update:**
```rust
// REMOVE these fields:
// pub kelly_p: f32,

// REPLACE with:
pub position_size_sol: f64,         // from SniperSizer::compute_position_size()
pub sizing_win_rate: f64,           // zone-specific p used for WR scalar
pub sizing_wr_scalar: f64,          // which WR scalar band was applied
pub sizing_score_tier: &'static str, // "conviction" | "normal" | "probe" | "floor_probe"
pub sizing_zone_mult: f64,          // 1.0 or 0.6
pub sizing_dd_mult: f64,            // drawdown multiplier applied
```

**SniperTradeLog update:**
```rust
// REMOVE:
// pub kelly_p: f32,

// ADD:
pub sizing_win_rate: f64,
pub sizing_wr_scalar: f64,
pub sizing_score_tier: String,
pub sizing_zone_mult: f64,
pub sizing_dd_mult: f64,
pub sizing_depth_mult: f64,
```

These fields ensure every trade is fully auditable — you can reconstruct exactly why the sizer chose the position it did.

---

### Decision Flowchart Update

Replace the Kelly sizing block in the main decision flowchart with:

```
... final_score >= 40 ↓

Position sizing:
  trade_count < 50?  → 0.02 SOL flat (bootstrap)
  else → SniperSizer::compute_position_size(
           final_score, curve_zone, real_sol, wallet_sol, &tracker)
  position_sol == 0.0? → skip (should not happen if score >= 40 post-bootstrap)
  
Bundle construction:
  TX1: buy position_sol on bonding curve
  TX2: sell all received tokens at target_vsol = sqrt(P1 × 1.12 × K)
  Submit to Jito
```## Bundle Construction

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
  "min_on_chain_score_for_entry": 30,
  "final_score_threshold": 40,
  "min_trades_for_velocity": 10,
  "skip_mayhem_mode": true,
  "max_vsol_entry": 20.0,
  "min_vsol_entry": 2.0,
  "conditional_vsol_entry_min": 15.0,
  "conditional_vsol_min_score": 60,
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
