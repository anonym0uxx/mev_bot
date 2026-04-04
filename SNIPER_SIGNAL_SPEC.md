# SniperEngine — Entry Signal System Spec

**Version:** 1.1  
**Date:** 2026-04-04  
**Author:** Apollo (synthesized from ArXiv 2602.14860, social-signal-layer-spec.md, BONDING_CURVE_SNIPER_IDEATION.md)  
**Status:** Ready for implementation  
**Goal:** Bonding curve sniper. Jito atomic bundle (buy+sell). 12%+ scalp target. Max loss per attempt = Jito tip + fees (~5000 lamports).

**v1.1 Changes:**
- Added G0: Mayhem Mode hard gate (skip all Mayhem coins — AI agent poisons velocity + bot ratio signals)
- Added timing-aware velocity: signal is gated behind `min_trades_for_velocity` (default 10). Entry threshold auto-adjusts when velocity data is unavailable.
- Added `create_v2` parsing note (required to detect Mayhem flag and new token creates)
- Signal stack inversion table: documents how signal weights shift at different entry windows

---

## Executive Summary

We enter pump.fun tokens **on the bonding curve**, before graduation, using Jito atomic bundles. TX1 buys at price P1; TX2 sells at price P2 ≥ P1 × 1.12. Both land or neither does — no open position risk.

Signal system has three tiers:
- **Tier 0: Hard Gates** — binary pass/fail from ShredStream. Any failure = skip immediately.
- **Tier 1: On-Chain Score** — continuous 0–100 score from ShredStream + Helius BC state. Feeds Kelly sizing.
- **Tier 2: Social Multiplier** — async 0.5×–2.0× multiplier from PumpPortal metadata + off-chain APIs.

Entry fires when: all Tier 0 gates pass AND `on_chain_score * social_multiplier_bps / 10_000 >= entry_threshold`.

**Key empirical finding (ArXiv 2602.14860):** Liquidity velocity (`vsol / trade_count`) is the #1 predictor of graduation, dominating all other variables. Bot-dominated tokens graduate significantly less. Entry is viable because the economic breakeven at early entry (vSol=5) requires only 0.19% conditional graduation probability — the bar is very low, making atomic bundles profitable even with modest signal conditioning.

---

## Bonding Curve Math (Reference)

```
k = x0_tot_virt × y0_tot_virt = 30 × 1.073e9 = 3.219e10

vsol: current virtual SOL in curve (lamports × 1e-9)
vtok: current virtual token supply = k / vsol

Price P (SOL per token) = vsol / vtok = vsol² / k

Graduation threshold: vsol_total = 115 (virtual). Real SOL raised = 85.

Buy: effective_sol_in = delta_sol × 0.9875 (1.25% fee)
     vsol_new = vsol + effective_sol_in
     tokens_out = vtok - k / vsol_new

Sell P2 target (12% above P1):
     P2 = P1 × 1.12
     vsol_exit² / k = P1 × 1.12
     vsol_exit = sqrt(P1 × 1.12 × k)
     vtok_exit = k / vsol_exit
     sol_out = vsol_exit - vsol_after_buy   (before fees)
     net_sol = sol_out × 0.9875

Economic breakeven (buy-and-hold, from ArXiv eq. 3):
     p(grad | vSol, θ) > vSol² / 115²
     At vSol=5:  need p > 0.0019 (0.19%)
     At vSol=10: need p > 0.0075 (0.75%)
     At vSol=20: need p > 0.030  (3.0%)
```

---

## Tier 0: Hard Gates

**Computed from ShredStream data + Helius BC account. Evaluated within 10ms of TokenCreated. Any failure = immediate skip, free all state.**

### G0: Not a Mayhem Mode Coin ⚠️ CHECK FIRST
**Source:** BondingCurve account data (Helius `accountSubscribe`) OR PumpPortal `TokenCreated` event field OR `create_v2` instruction decode.  
**Logic:** `is_mayhem_mode == true` → FAIL immediately.  
**Rationale:** Mayhem Mode deploys an autonomous AI agent that trades the token for 24h with a random buy/sell walk, using 1B extra minted tokens. This completely corrupts our signal stack:
- **Velocity signal poisoned:** Agent places large buys → high `vsol/trade_count` that looks like strong momentum but is synthetic.
- **Bot ratio signal flips:** Agent invokes contract directly (no pump.fun UI) → flagged `is_bot=true`. Mayhem coins look bot-dominated for the wrong reason.
- **Buy/sell ratio distorted:** Agent does random equal-probability buy/sell → ratio trends toward 50/50 regardless of real demand.
- **Fee routing changed:** Protocol fees go to undisclosed wallets, not tracked on fees.pump.fun. Revenue structure is different.

**Detection:** `is_mayhem_mode` is an immutable boolean stored in the BondingCurve account after `create_v2`. Legacy `create` instruction always produces `is_mayhem_mode = false`.

**Implementation sources (in priority order):**
1. PumpPortal `TokenCreated` event — check if `mayhem_mode` field is present and true (zero cost, already streaming)
2. Helius `accountSubscribe` on BondingCurve account — parse account data, `is_mayhem_mode` byte offset (confirm from pump IDL)
3. ShredStream `create_v2` instruction decode — see Note below

**⚠️ Note — `create_v2` parsing gap:** Pump.fun is transitioning from legacy `create` to `create_v2`. Most existing sniper bots (including our current `parse_pump_transaction()`) only parse the legacy instruction discriminator. **`create_v2` has a different discriminator.** When pump.fun completes the transition, bots that don't handle `create_v2` will silently miss all new token creates. This must be fixed before the sniper goes live. Add dual-discriminator detection in `shredstream.rs` for both `create` and `create_v2`.

**Rust field:** `not_mayhem_mode: bool`

### G1: No Dev Pre-Buy
**Source:** ShredStream — first `trade_count` transactions after mint.  
**Logic:** Scan first 3 ShredStream trade events for this mint. If `trader == creator_map[mint]` and `is_buy == true` → FAIL.  
**Rationale:** Dev pre-loading tokens before public means dev has a head start on dumps.  
**Rust field:** `no_dev_prebuy: bool`  
**Default if creator_map miss:** PASS (can't verify, proceed with lower social score)

### G2: No Same-Block Bundle
**Source:** ShredStream — slot of first N buy trades.  
**Logic:** If ≥ 3 distinct wallet addresses buy in the same slot as the `create` instruction → FAIL.  
**Rationale:** Coordinated bundle = pre-arranged pump, dev team loading up. Paper finding: bot-dominated early activity predicts failure.  
**Rust field:** `no_bundle_detected: bool`  
**Implementation:** Track `(mint → Vec<(slot, trader)>)` for first 5 trades. If `slot_count[create_slot] >= 3 distinct wallets` → fail.

### G3: Not a Serial Rugger Dev
**Source:** Dev wallet history cache (populated async from Helius `getAssetsByCreator`).  
**Logic:** If `dev_tokens_launched >= 20 AND dev_rug_rate > 0.50` → FAIL.  
**Rationale:** ArXiv finding: prolific creators have lower graduation rates. Serial ruggers predictable.  
**Rust field:** `dev_not_blacklisted: bool`  
**Default if cache miss:** PASS (enrich async, use lower social weight)

### G4: Curve Fill Below Ceiling
**Source:** Helius BC account subscribe OR ShredStream trade reconstruction.  
**Logic:** If `vsol >= 40.0` at time of evaluation → FAIL. We don't enter tokens that are already 35%+ filled.  
**Rationale:** Too late to capture 12% at entry >40 SOL (P2 sell would require >~45 SOL, likely already tapped out).  
**Rust field:** `curve_fill_ok: bool`

### G5: Min Trades Not Exceeded
**Source:** ShredStream trade counter per mint.  
**Logic:** If `trade_count > 300` by the time we evaluate → FAIL. Too many trades for the vSol level (low velocity signal, see T1-S1).  
**Rationale:** ArXiv finding: tokens with many small trades at same vSol have lower graduation probability.  
**Rust field:** `velocity_not_exhausted: bool`

---

## Tier 1: On-Chain Score (0–100)

**Computed from ShredStream trade stream + Helius BC account state. Updated in real-time.**

### S1: Liquidity Velocity Score (0–35 pts)
**The #1 signal per ArXiv 2602.14860 — but only meaningful with sufficient trade history.**  
**Source:** ShredStream — track `vsol_accumulated` and `trade_count` per mint.

**Timing awareness:** At mint time, `trade_count` is near zero. `vsol/1` is just the size of the first buy — not a velocity signal, just noise. This signal is gated behind `min_trades_for_velocity` (default: 10).

```
// Timing-aware velocity computation:
if trade_count < min_trades_for_velocity (default 10):
    velocity_score = 0   // insufficient data — neutral, not a fail
    entry_threshold -= 10  // compensate: lower bar since signal is absent
                            // (effective threshold: 35 instead of 45)
else:
    vsol_per_trade = vsol_accumulated / trade_count

    Score:
      vsol_per_trade >= 2.0 SOL/trade  → 35 pts  (massive buyers, very strong)
      vsol_per_trade >= 0.5 SOL/trade  → 25 pts  (strong)
      vsol_per_trade >= 0.1 SOL/trade  → 15 pts  (moderate)
      vsol_per_trade >= 0.02 SOL/trade → 8 pts   (weak, many small buys)
      vsol_per_trade <  0.02           → 3 pts   (bot churn, very weak)
```

**Signal stack inversion by entry window:**

| Entry Window | Primary Signals | Velocity Status | Entry Threshold |
|---|---|---|---|
| vSol 0–5 (mint fresh, <10 trades) | Tier 0 gates + Social multiplier | Disabled (0 pts) | 35 (adjusted) |
| vSol 5–15 (10–50 trades) | Social + early velocity | Weak signal, lower weight | 40 |
| vSol 15–40 (50+ trades) | Velocity dominant | Full weight | 45 |

**Practical implication:** At early entry, the social multiplier carries the most weight since on-chain data is thin. Tier 0 gates (especially G0 Mayhem, G1 dev prebuy, G2 bundle detection) are the strongest fast filters at mint time.

**Rust field:** `velocity_score: u8`  
**Config:** `min_trades_for_velocity: u32 = 10`  
**Note:** Use `vsol_accumulated` from Helius BC accountSubscribe for accuracy; approximate from ShredStream trade sums as fallback.

### S2: Non-Bot Trade Ratio (0–25 pts)
**ArXiv finding: >70% non-bot trades → significantly higher graduation probability.**  
**Source:** ShredStream — transaction log inspection.  
**Bot detection:** A transaction is `is_bot=true` if its program logs do NOT contain a reference to the pump.fun frontend program (`TSLvdd1pWpHVjahSpsvCXUbgwsL3JAcvokwaKt1eokM` — the Pump.fun UI program). Direct contract invocations lack this log entry.  
**Formula:**
```
non_bot_ratio = non_bot_trades / total_trades (first 20 trades observed)

Score:
  non_bot_ratio >= 0.80 → 25 pts
  non_bot_ratio >= 0.60 → 18 pts
  non_bot_ratio >= 0.40 → 10 pts
  non_bot_ratio >= 0.20 → 4 pts
  non_bot_ratio <  0.20 → 0 pts
```
**Rust field:** `bot_ratio_score: u8`  
**Default if <5 trades seen:** 10 pts (neutral pending data)

### S3: Curve Fill % at Entry (0–15 pts)
**Source:** Helius BC accountSubscribe → `vsol_reserves`.  
**Rationale:** Tokens entering at 2-8 SOL are fresh with full upside. 8-20 SOL = still good. 20-40 SOL = getting late.  
**Formula:**
```
fill_pct = vsol / 115.0

Score:
  fill_pct <= 0.05  (vsol ≤ 5.75)   → 15 pts  (very early)
  fill_pct <= 0.10  (vsol ≤ 11.5)   → 12 pts  (early)
  fill_pct <= 0.20  (vsol ≤ 23.0)   → 7 pts   (moderate)
  fill_pct <= 0.35  (vsol ≤ 40.25)  → 2 pts   (late, G4 gate kicks in at 40)
```
**Rust field:** `fill_score: u8`

### S4: Buy/Sell Ratio (0–15 pts)
**Source:** ShredStream — count buy vs sell trades per mint over last 20 events.  
**Formula:**
```
buy_ratio = buy_count / (buy_count + sell_count), window=last 20 trades

Score:
  buy_ratio >= 0.85 → 15 pts
  buy_ratio >= 0.70 → 10 pts
  buy_ratio >= 0.55 → 5 pts
  buy_ratio <  0.55 → 0 pts
```
**Rust field:** `buy_sell_score: u8`

### S5: Smart Money Presence (0–10 pts)
**ArXiv finding: modest positive signal early, non-monotonic. Low weight.**  
**Source:** `smart_wallet_set: HashSet<[u8;32]>` — populated from our own trade history. Any wallet that has previously achieved a winning trade in our logs (exit_reason=trailing_stop OR pnl_sol > 0.01) gets added.  
**Logic:** If any trader in first 10 buys of this token is in `smart_wallet_set` → 10 pts, else 0.  
**Rust field:** `smart_money_score: u8`  
**Note:** Start with empty set; populate from momentum trade log. 0 pts is correct default until we have data.

### On-Chain Score Aggregation
```rust
pub struct OnChainScore {
    pub velocity_score: u8,       // 0-35
    pub bot_ratio_score: u8,      // 0-25
    pub fill_score: u8,           // 0-15
    pub buy_sell_score: u8,       // 0-15
    pub smart_money_score: u8,    // 0-10
    pub total: u8,                // 0-100, sum of above
    pub computed_at_ms: u64,
    pub vsol_at_score: f64,       // vsol when score was computed
    pub trade_count_at_score: u32,
}

impl OnChainScore {
    pub fn total(&self) -> u8 {
        self.velocity_score
            .saturating_add(self.bot_ratio_score)
            .saturating_add(self.fill_score)
            .saturating_add(self.buy_sell_score)
            .saturating_add(self.smart_money_score)
            .min(100)
    }
}
```

**Entry threshold:** `on_chain_score >= 45` to proceed to social check and bundle construction. Tunable in config.

---

## Tier 2: Social Multiplier (0.5×–2.0×)

**Computed ASYNC — fired on TokenCreated, cached in DashMap. Never on critical path.**  
**Applied as:** `final_score = on_chain_score * social_multiplier_bps / 10_000`

### Social Signals

#### SS1: Dev Wallet History (weight: 30%)
**Source:** Helius `getAssetsByCreator(dev_pubkey)` — async, cache result.  
**Score:**
```
dev_is_blacklisted  → social_multiplier = 0 (hard veto — propagates to kill entry)
dev_tokens_launched == 0                          → neutral (10_000 bps)
dev_tokens_launched 1-5, rug_rate < 0.20          → +1500 bps
dev_tokens_launched 1-5, rug_rate 0.20-0.50       → neutral
dev_tokens_launched 1-5, rug_rate > 0.50          → -2000 bps
dev_tokens_launched 6-20, success_rate > 0.30     → +2000 bps
dev_tokens_launched 6-20, success_rate < 0.10     → -1500 bps
dev_tokens_launched > 20                          → -2000 bps (ArXiv: prolific = bad)
```

#### SS2: Pump.fun Engagement (weight: 25%)
**Source:** Pump.fun API `GET https://frontend-api-v2.pump.fun/coins/{mint}` — async.  
**Fields:** `reply_count`, `king_of_the_hill_timestamp`, `is_currently_live`  
**Score:**
```
reply_count 0      → 0 bps delta
reply_count 1-5    → +300 bps
reply_count 6-20   → +800 bps
reply_count > 20   → +1500 bps
is_koth            → +2000 bps
is_live            → +1000 bps
```

#### SS3: Twitter Presence (weight: 20%)
**Source:** PumpPortal TokenCreated event — `twitter` field.  
**Score (metadata only, no X API calls):**
```
twitter present     → +500 bps
twitter absent      → -500 bps
```
*Note: X API lookups deferred until we have data proving Twitter presence is predictive for pump.fun. Start with presence-only signal.*

#### SS4: Metadata Quality (weight: 15%)
**Source:** PumpPortal TokenCreated — `description`, `imageUri`, `website`, `telegram`, `twitter`  
**Score:**
```
link_count = (twitter!=null) + (telegram!=null) + (website!=null)
description_len chars

link_count == 0, description_len < 10  → -800 bps
link_count == 1                        → +200 bps
link_count == 2                        → +500 bps
link_count == 3                        → +800 bps
has_image                              → +300 bps
description_len 20-100                 → +300 bps
description_len > 100                  → +500 bps
```

#### SS5: Telegram Community Size (weight: 10%)
**Source:** Telegram Bot API `getChatMemberCount` if telegram link present — async.  
**Score:**
```
no telegram link       → neutral (10_000 bps)
telegram present, 0-10 members   → -500 bps (empty group = fake)
telegram 11-50 members           → +300 bps
telegram 51-200 members          → +800 bps
telegram > 200 members           → +1500 bps
```

### Social Score Computation

```rust
pub struct SocialScore {
    // Raw signals
    pub has_twitter: bool,
    pub has_telegram: bool,
    pub has_website: bool,
    pub has_image: bool,
    pub description_len: u16,
    pub dev_tokens_launched: u16,
    pub dev_rug_count: u16,
    pub dev_success_rate_bps: u16,   // 0-10000
    pub dev_is_blacklisted: bool,
    pub pumpfun_reply_count: u16,
    pub pumpfun_is_koth: bool,
    pub pumpfun_is_live: bool,
    pub telegram_members: u32,
    // Sub-scores (bps, 10000=neutral)
    pub dev_score_bps: u16,
    pub engagement_score_bps: u16,
    pub twitter_score_bps: u16,
    pub metadata_score_bps: u16,
    pub telegram_score_bps: u16,
    // Final
    pub social_multiplier_bps: u16,  // 5000-20000. 0 = hard veto.
    pub enrichment_complete: bool,
    pub enrichment_ts_ms: u64,
}

// Weights (must sum to 100):
const W_DEV: i32 = 30;
const W_ENG: i32 = 25;
const W_TWI: i32 = 20;
const W_MET: i32 = 15;
const W_TG:  i32 = 10;

// Composite: weighted arithmetic mean of deltas from neutral
fn compute_multiplier(s: &SocialScore) -> u16 {
    if s.dev_is_blacklisted { return 0; }
    let dev = s.dev_score_bps as i32 - 10_000;
    let eng = s.engagement_score_bps as i32 - 10_000;
    let twi = s.twitter_score_bps as i32 - 10_000;
    let met = s.metadata_score_bps as i32 - 10_000;
    let tg  = s.telegram_score_bps as i32 - 10_000;
    let delta = (dev*W_DEV + eng*W_ENG + twi*W_TWI + met*W_MET + tg*W_TG) / 100;
    ((10_000 + delta).clamp(5_000, 20_000)) as u16
}
```

---

## Complete Rust Structs

### BondingCurveState
```rust
/// Real-time per-token state, updated by ShredStream trades + Helius accountSubscribe.
pub struct BondingCurveState {
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],
    pub creator: [u8; 32],
    // Curve state
    pub vsol: f64,           // virtual SOL in curve (SOL, not lamports)
    pub vtok: f64,           // virtual token supply
    pub real_sol: f64,       // real SOL deposited (vsol - 30.0)
    // Trade tracking
    pub trade_count: u32,
    pub buy_count: u32,
    pub sell_count: u32,
    pub bot_trade_count: u32,    // trades flagged as direct contract calls
    pub human_trade_count: u32,  // trades via pump.fun UI
    // Computed
    pub vsol_per_trade: f64,     // vsol / trade_count — velocity signal
    pub non_bot_ratio: f32,      // human_trade_count / trade_count
    // Timing
    pub created_at_ms: u64,      // TokenCreated timestamp
    pub last_trade_ms: u64,      // last ShredStream trade
    pub age_ms: u64,             // now - created_at_ms
    // Status
    pub creator_sold: bool,      // ShredStream creator sell detected
    pub graduated: bool,         // ShredStream migration detected
    pub evaluation_count: u16,   // how many times scorer has run
}

impl BondingCurveState {
    pub const K: f64 = 3.219e10_f64; // 30 * 1.073e9
    pub const GRAD_VSOL: f64 = 115.0;
    pub const FEE_RATE: f64 = 0.9875; // 1 - 0.0125

    pub fn price_sol_per_token(&self) -> f64 {
        self.vsol / self.vtok
    }

    pub fn market_cap_sol(&self) -> f64 {
        // Total tokens in existence * current price
        // 1e9 total supply at mint; vtok decreases as buys happen
        self.vsol * self.vsol / Self::K
    }

    pub fn fill_pct(&self) -> f64 {
        self.vsol / Self::GRAD_VSOL
    }

    /// SOL needed to buy `token_amount` tokens (before fee)
    pub fn sol_for_tokens(&self, token_amount: f64) -> f64 {
        let vtok_new = self.vtok - token_amount;
        let vsol_new = Self::K / vtok_new;
        (vsol_new - self.vsol) / Self::FEE_RATE
    }

    /// Tokens received for `sol_in` SOL (net of fee)
    pub fn tokens_for_sol(&self, sol_in: f64) -> f64 {
        let effective = sol_in * Self::FEE_RATE;
        let vsol_new = self.vsol + effective;
        let vtok_new = Self::K / vsol_new;
        self.vtok - vtok_new
    }

    /// vsol level needed to achieve P2 = entry_price * target_mult
    pub fn vsol_for_price_target(&self, entry_price: f64, target_mult: f64) -> f64 {
        let p2 = entry_price * target_mult;
        (p2 * Self::K).sqrt()
    }
}
```

### SniperEntrySignal
```rust
/// Final decision struct fed to bundle constructor.
pub struct SniperEntrySignal {
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],
    pub assoc_bonding_curve: [u8; 32],
    // Scores
    pub on_chain_score: OnChainScore,
    pub social_score: Option<SocialScore>,   // None if enrichment not yet complete
    pub final_score: u8,                     // on_chain * social_mult / 10000, capped 100
    // Sizing (Kelly)
    pub kelly_p: f32,                        // estimated win probability
    pub position_size_sol: f64,              // SOL to spend on buy
    // Execution parameters
    pub entry_vsol: f64,                     // vsol at decision time
    pub entry_price: f64,                    // P1 = vsol/vtok at entry
    pub target_vsol: f64,                    // vsol where P2 = P1 * 1.12
    pub target_price: f64,                   // P2
    pub buy_sol_lamports: u64,               // TX1 sol_amount
    pub min_tokens_out: u64,                 // TX1 slippage floor
    pub sell_tokens: u64,                    // TX2 token_amount
    pub min_sol_out: u64,                    // TX2 slippage floor
    pub jito_tip_lamports: u64,
    // Meta
    pub decision_ms: u64,
    pub paper_mode: bool,
}
```

### SniperTradeLog
```rust
/// Written to JSONL on every bundle attempt (win, loss, or paper).
#[derive(Serialize)]
pub struct SniperTradeLog {
    // Identity
    pub mint: String,              // base58
    pub token_name: String,
    pub token_symbol: String,
    pub decision_ms: u64,
    pub paper_mode: bool,
    // Curve state at entry
    pub vsol_at_entry: f64,
    pub vtok_at_entry: f64,
    pub fill_pct_at_entry: f32,
    pub trade_count_at_entry: u32,
    pub vsol_per_trade: f64,       // velocity signal
    pub non_bot_ratio: f32,
    pub age_ms_at_entry: u64,
    // Scores
    pub on_chain_score: u8,
    pub velocity_score: u8,
    pub bot_ratio_score: u8,
    pub fill_score: u8,
    pub buy_sell_score: u8,
    pub smart_money_score: u8,
    pub social_multiplier_bps: u16,
    pub final_score: u8,
    pub kelly_p: f32,
    // Execution
    pub position_size_sol: f64,
    pub entry_price: f64,
    pub target_price: f64,
    pub target_vsol: f64,
    pub jito_tip_lamports: u64,
    pub bundle_sig: Option<String>,
    // Outcome
    pub outcome: SniperOutcome,    // BundleLanded | BundleRejected | BundleExpired | Paper
    pub pnl_sol: f64,              // net SOL after fees. 0 if rejected/paper.
    pub landed_at_ms: Option<u64>,
    // Social metadata (for training data)
    pub dev_pubkey: String,
    pub has_twitter: bool,
    pub has_telegram: bool,
    pub has_website: bool,
    pub has_image: bool,
    pub description_len: u16,
    pub dev_tokens_launched: u16,
    pub dev_rug_count: u16,
    pub pumpfun_reply_count: u16,
    pub pumpfun_is_koth: bool,
    pub telegram_members: u32,
    pub social_enrichment_complete: bool,
}

#[derive(Serialize)]
pub enum SniperOutcome {
    BundleLanded,    // TX1+TX2 confirmed — win
    BundleRejected,  // Jito rejected bundle — loss (tip only)
    BundleExpired,   // blockhash expired before landing — loss (tip only)
    Paper,           // paper mode, no real bundle sent
}
```

---

## Scoring Pipeline

```
TokenCreated event arrives
        │
        ▼
[Register in BondingCurveState map]
[Fire async social enrichment worker (non-blocking)]
        │
        ▼
[ShredStream trades update BondingCurveState in real-time]
        │
Every tick (50ms) OR on each ShredStream trade for this mint:
        │
        ▼
[Tier 0 Gate Check]
  G1: no_dev_prebuy?        ──FAIL──▶ Drop token, free state
  G2: no_bundle_detected?   ──FAIL──▶
  G3: dev_not_blacklisted?  ──FAIL──▶
  G4: curve_fill_ok?        ──FAIL──▶
  G5: velocity_not_exhausted? ─FAIL─▶
        │ ALL PASS
        ▼
[Tier 1: On-Chain Score]
  S1: velocity_score    (0-35)
  S2: bot_ratio_score   (0-25)
  S3: fill_score        (0-15)
  S4: buy_sell_score    (0-15)
  S5: smart_money_score (0-10)
  total = sum, capped 100
        │
  total < 45? ──▶ Continue monitoring (re-evaluate next tick)
        │ total >= 45
        ▼
[Tier 2: Social Multiplier]
  Check social_score_cache[mint]
  ├── Hit: apply social_multiplier_bps
  └── Miss: use 10_000 (neutral) — enrichment will arrive async
        │
  final_score = on_chain_score * social_multiplier_bps / 10_000
  social_multiplier_bps == 0? ──▶ Hard veto (blacklisted dev), drop
        │
  final_score < entry_threshold (default 40)? ──▶ Continue monitoring
        │ final_score >= entry_threshold
        ▼
[Kelly Sizing]
  p = score_to_probability(final_score)
      // linear interpolation: score 40 → p=0.35, score 70 → p=0.55, score 100 → p=0.75
  b = 0.12  // 12% target gain on bonding curve
  q = 1 - p
  kelly_f = (p * b - q) / b
  half_kelly = kelly_f * 0.5
  position_sol = wallet_sol * half_kelly
  position_sol = clamp(position_sol, min_size_sol=0.01, max_size_sol=0.10)
        │
        ▼
[Bundle Construction]
  See "Bundle Construction" section below
        │
        ▼
[Submit to Jito]
[Log SniperTradeLog to JSONL]
        │
        ▼
[Outcome]
  BundleLanded → win, log pnl
  BundleRejected/Expired → loss (~5000 lamports tip only), log
```

**Token lifetime limit:** If token age > 300s (5 minutes) without a bundle fire, drop from tracking. ArXiv median graduation time is 4.4 minutes — tokens that haven't generated a signal by 5 minutes are unlikely to graduate.

---

## Bundle Construction

### TX1: Buy on Bonding Curve

Pump.fun bonding curve buy instruction layout:
```
Program: 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P (pump.fun program)
Discriminator: [102, 6, 61, 18, 1, 218, 235, 234]  // "buy" discriminator

Instruction data:
  [0..8]   discriminator
  [8..16]  amount: u64 (tokens to buy)
  [16..24] max_sol_cost: u64 (max lamports to spend, slippage cap)

Accounts (in order):
  [0]  global (PDA)
  [1]  fee_recipient (pump.fun fee wallet)
  [2]  mint
  [3]  bonding_curve (PDA: ["bonding-curve", mint])
  [4]  assoc_bonding_curve (ATA of bonding_curve for mint)
  [5]  associated_user (ATA of wallet for mint — must exist or create)
  [6]  user (wallet, signer)
  [7]  system_program
  [8]  token_program
  [9]  rent
  [10] event_authority (PDA)
  [11] program (pump.fun)
```

**TX1 parameters:**
```
amount = tokens_for_sol(position_size_sol)  // from BondingCurveState math
max_sol_cost = position_size_lamports * 115 / 100  // 15% slippage cap
```

### TX2: Sell on Bonding Curve

```
Discriminator: [51, 230, 133, 164, 1, 127, 131, 173]  // "sell" discriminator

Instruction data:
  [0..8]   discriminator
  [8..16]  amount: u64 (tokens to sell)
  [16..24] min_sol_output: u64 (min lamports to receive, slippage floor)

Accounts (in order):
  [0]  global (PDA)
  [1]  fee_recipient
  [2]  mint
  [3]  bonding_curve (PDA)
  [4]  assoc_bonding_curve (ATA of bonding_curve for mint)
  [5]  associated_user (ATA of wallet for mint)
  [6]  user (wallet, signer)
  [7]  system_program
  [8]  associated_token_program
  [9]  token_program
  [10] event_authority (PDA)
  [11] program
```

**TX2 parameters:**
```
amount = tokens received from TX1 (exact, from BC math)
target_vsol = vsol_for_price_target(entry_price, 1.12)
sol_out_gross = target_vsol - vsol_after_buy
sol_out_net = sol_out_gross * FEE_RATE
min_sol_output = (sol_out_net * 0.85 * 1e9) as u64  // 15% slippage tolerance
```

TX2 is computed AFTER TX1 outputs — they execute sequentially in the bundle.

### Jito Bundle Assembly
```
Bundle = [TX1_buy, TX2_sell, tip_tx]
Tip: 100_000 lamports default (~0.0001 SOL). Configurable.
```

---

## Implementation Phases

### Phase 0: Data Logging (DO FIRST — 2-4 hours)
**Files:** `src/sniper/mod.rs`, `src/sniper/logger.rs`
- Log every `on_token_created()` to `data/sniper_create_log.jsonl` (social metadata)
- Wire ShredStream trades to BondingCurveState counters
- After 300s or on graduation: write outcome fields (vsol_peak, trade_count, graduated, graduation_ms, graduation_steps)
- **Outcome:** ~50k labeled samples/day. Training data for all signal calibration.

### Phase 1: BondingCurveState Tracker (1-2 days)
**Files:** `src/sniper/bc_tracker.rs`
- BondingCurveState struct + DashMap
- `on_token_created()` → insert
- ShredStream TradeEvent → update vsol, vtok, counts, bot flag
- Helius `accountSubscribe(bonding_curve)` for reserve confirmation
- `is_bot()` detection from tx program logs

### Phase 2: Tier 0 Hard Gates (1 day)
**Files:** `src/sniper/gates.rs`
- G1–G5 implementation + unit tests

### Phase 3: Tier 1 On-Chain Scorer (1-2 days)
**Files:** `src/sniper/scorer.rs`
- S1–S5 signals, OnChainScore struct
- Evaluation loop (every ShredStream trade for tracked mints)
- `score_to_probability()` interpolation
- Paper mode logging (log decisions without firing bundles)

### Phase 4: Social Enrichment Worker (2-3 days)
**Files:** `src/sniper/social_enrichment.rs`, `src/sniper/social_score.rs`
- SocialScore struct + compute functions
- tokio::spawn enrichment task per TokenCreated
- Helius DAS + Pump.fun API + Telegram Bot (parallel)
- DashMap social cache + DevWalletCache
- Bayesian weight state (file-backed, recompute every 50 outcomes)

### Phase 5: Bundle Constructor + Jito Submit (2-3 days)
**Files:** `src/sniper/bundle_builder.rs`
- TX1/TX2 instruction builders (account layouts above)
- Kelly sizing from final_score
- Jito bundle submit via `ExecutionContext.jito_client`
- Bundle outcome polling (30s), classify Landed/Rejected/Expired
- SniperTradeLog JSONL write on every attempt
- **Paper mode first.** Enable live only after Phase 0 data validates signal quality.

### Phase 6: Calibration (ongoing)
- Analyze `sniper_create_log.jsonl` — which signals correlate with graduation?
- Tune score thresholds using empirical data
- Update Bayesian weights from live outcomes

---

## Phase 0 Logging Schema

`data/sniper_create_log.jsonl` — one JSON object per line:

```json
{
  "ts_ms": 1743800000000,
  "mint": "base58...",
  "dev_pubkey": "base58...",
  "token_name": "DOGE2",
  "token_symbol": "DOGE2",
  "has_twitter": true,
  "twitter_url": "https://x.com/doge2pump",
  "has_telegram": false,
  "telegram_url": null,
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

Outcome fields filled retroactively via `HashMap<[u8;32], SniperCreateRecord>` in SniperEngine state.

---

## canary.json Config Block

```json
"sniper": {
  "enabled": false,
  "paper_mode": true,
  "entry_threshold": 45,
  "entry_threshold_no_velocity": 35,   // used when trade_count < min_trades_for_velocity
  "min_on_chain_score": 45,
  "min_trades_for_velocity": 10,       // below this, S1 contributes 0 pts
  "skip_mayhem_mode": true,            // G0 gate — skip all is_mayhem_mode=true coins
  "min_position_sol": 0.01,
  "max_position_sol": 0.10,
  "kelly_fraction": 0.5,
  "target_profit_mult": 1.12,
  "sell_slippage_pct": 15,
  "buy_slippage_pct": 15,
  "max_vsol_entry": 40.0,
  "max_trade_count_entry": 300,
  "max_token_age_secs": 300,
  "jito_tip_lamports": 100000,
  "log_path": "data/sniper_trades.jsonl",
  "create_log_path": "data/sniper_create_log.jsonl",
  "social_cache_ttl_secs": 300,
  "dev_cache_ttl_secs": 3600,
  "pumpfun_api_rps": 5,
  "telegram_bot_token": null
}
```

---

## Signal Summary Table

| Signal | Tier | Source | Weight | ArXiv Basis |
|--------|------|--------|--------|-------------|
| Liquidity velocity (vsol/trade) | T1-S1 | ShredStream | 35 pts | **#1 predictor** (2602.14860 §VII) |
| Non-bot trade ratio | T1-S2 | ShredStream logs | 25 pts | Bot share → lower grad prob (§VII) |
| Curve fill % | T1-S3 | Helius BC account | 15 pts | Entry timing constraint |
| Buy/sell ratio | T1-S4 | ShredStream | 15 pts | Momentum confirmation |
| Smart money presence | T1-S5 | Internal wallet set | 10 pts | Weak positive, non-monotonic (§VII) |
| Dev wallet history | T2-SS1 | Helius DAS | 30% weight | Prolific creators → lower grad (§VII) |
| Pump.fun engagement | T2-SS2 | Pump.fun API | 25% weight | KOTH/live = strong attention |
| Twitter presence | T2-SS3 | PumpPortal metadata | 20% weight | Legitimacy signal |
| Metadata quality | T2-SS4 | PumpPortal metadata | 15% weight | Effort indicator |
| Telegram community | T2-SS5 | Telegram Bot API | 10% weight | Real community = real demand |
| **Not Mayhem Mode** | **T0-G0** | **Helius BC / PumpPortal** | **GATE** | **AI agent corrupts all signals** |
| No dev prebuy | T0-G1 | ShredStream | GATE | Rug setup indicator |
| No bundle wallets | T0-G2 | ShredStream | GATE | Coordinated dump setup |
| Dev not blacklisted | T0-G3 | Dev cache | GATE | Known serial ruggers |
| Curve fill ceiling | T0-G4 | BC state | GATE | Too late to enter |
| Velocity not exhausted | T0-G5 | ShredStream counter | GATE | ArXiv: many small trades = bad |

---

*Spec v1.0 | 2026-04-04 | Apollo*
*Sources: ArXiv 2602.14860 (Marino/Tarantelli/Lillo, Feb 2026), social-signal-layer-spec.md, BONDING_CURVE_SNIPER_IDEATION.md*
