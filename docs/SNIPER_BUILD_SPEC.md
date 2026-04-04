# SniperEngine — Implementation Build Spec

**Version:** 1.0  
**Date:** 2026-04-04  
**Author:** Apollo (Opus 4.6 quant architect)  
**Status:** Ready for implementation  
**Companion:** `SNIPER_SIGNAL_SPEC.md v2.0` (signal system, scoring, sizing, exit logic)

---

## 1. Overview

This is the **implementation blueprint** for the SniperEngine — a pump.fun bonding curve sniper that enters tokens before graduation via Jito atomic bundles and manages positions with an event-driven exit engine.

| Topic | Document |
|---|---|
| Hard gates (G0–G7), on-chain scoring (S1–S5), social multiplier (SS1–SS5), entry thresholds, position sizing, exit decision engine, bonding curve math | **SNIPER_SIGNAL_SPEC.md v2.0** |
| Concurrent position management, queue system, feed wiring, data dependencies, Phase 0 logging, paper mode, error handling, config block, build phases | **This document** |

SNIPER_SIGNAL_SPEC.md defines the **what**. This document defines the **how**.

---

## 2. Architecture Diagram

```
┌──────────────────────┐    ┌──────────────────────────────────────────┐
│  ShredStream gRPC    │    │            PumpPortal WebSocket          │
│  (shredstream.rs)    │    │  TokenCreated events + social metadata   │
│  create v1/v2, trade,│    └──────────────┬───────────────────────────┘
│  migrate             │                   │
└─────────┬────────────┘                   │
          │                                │
          ▼                                ▼
┌──────────────────────────────────────────────────────────────┐
│                      SniperEngine                            │
│                                                              │
│  Phase0Logger ◄── always-on create logging                   │
│       │                                                      │
│       ▼                                                      │
│  TokenRegistry (DashMap<mint, BondingCurveState>)            │
│  creator_map (DashMap<mint, creator_pubkey>)                 │
│       │                                                      │
│       ├── GateEvaluator (G0–G7) ─── all pass? ──┐           │
│       │                                          ▼           │
│       ├── ScoreEngine (S1–S5 on-chain)    PositionManager   │
│       │   SocialEnricher (SS1–SS5)        ├─ open[max=10]   │
│       │                                   ├─ queue[max=5]   │
│       │                                   ├─ WinRateTracker │
│       │                                   └─ SniperSizer    │
│       │                                          │           │
│       │                                          ▼           │
│  ┌────┴──────────────────┐   ┌───────────────────────────┐  │
│  │ Helius WS             │   │ Jito Bundle / Paper Log   │  │
│  │ accountSubscribe (BC) │   └───────────┬───────────────┘  │
│  │ getAssetsByCreator    │               │ bundle landed     │
│  └───────────────────────┘               ▼                   │
│                                    ExitEngine                │
│                                    (per-position,            │
│                                     event-driven)            │
│                                          │                   │
│                                          ▼                   │
│                                    TradeLogger               │
│                                    sniper_trades.jsonl       │
│                                    sniper_paper_trades.jsonl │
└──────────────────────────────────────────────────────────────┘

External data files:
  data/smart_wallets.json        ← S5 smart money (weekly refresh)
  data/dumper_wallets.json       ← S5 negative signal (self-populated)
  data/sniper_create_log.jsonl   ← Phase 0 always-on log
```

---

## 3. Concurrent Position Management

### 3.1 Is Max 10 Rational?

**Yes.** Math: max hold 120s, typical 15–60s, max exposure 10 × 0.10 = 1.0 SOL, expected ~0.30 SOL. Signal rate after filtering: 1–3/min peak. Need ~15 signals/min to saturate 10 slots at 40s avg hold. Unlikely. Correlated worst case: 10 × 0.10 × 0.30 = 0.30 SOL simultaneous loss. Survivable.

**Calibrate at 500 trades.** If rarely > 5 occupied, reduce to 7. If queue > 5 consistently, increase to 12.

### 3.2 Queue: Scored Priority Queue (Max-Heap)

- **Structure:** `BinaryHeap<QueuedCandidate>` ordered by `final_score` descending
- **Admission:** Only queue if `final_score ≥ 40`
- **Max depth: 5.** If full and new signal has higher score → evict lowest
- **Eviction triggers:** age > 300s, creator sell, real_sol > 20, concentration > 15%

### 3.3 QueuedCandidate Struct

```rust
pub const MAX_OPEN_POSITIONS: usize = 10;
pub const MAX_QUEUE_DEPTH: usize = 5;
pub const MAX_TOKEN_AGE_SECS: u64 = 300;

#[derive(Debug, Clone)]
pub struct QueuedCandidate {
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],
    pub assoc_bonding_curve: [u8; 32],
    pub final_score: u8,
    pub on_chain_score: u8,
    pub social_multiplier_bps: u16,
    pub curve_zone: CurveFillZone,
    pub real_sol_at_queue: f64,
    pub created_at_ms: u64,
    pub queued_at_ms: u64,
    pub gate_snapshot: GateSnapshot,
}

/// Which gates passed and are time-insensitive (don't re-check on slot open).
#[derive(Debug, Clone)]
pub struct GateSnapshot {
    pub g0_mayhem_ok: bool,           // immutable per token
    pub g1_dev_prebuy_ok: bool,       // frozen after first 5 trades
    pub g2_coordinated_bundle_ok: bool, // frozen after create slot
    pub g3_serial_rugger_ok: bool,    // dev history static
    pub g6_throwaway_ok: bool,        // creator balance at creation
    // G4 (curve zone) — TIME-SENSITIVE, re-check
    // G7 (concentration) — TIME-SENSITIVE, re-check
}

impl Ord for QueuedCandidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.final_score.cmp(&other.final_score)
            .then_with(|| other.queued_at_ms.cmp(&self.queued_at_ms))
    }
}
// PartialOrd, PartialEq, Eq derive from Ord + mint equality
```

### 3.4 Re-Validation on Slot Open

When a position closes → pop best from queue → re-validate:

**Skip re-check (time-insensitive):** G0, G1, G2, G3, G6  
**MUST re-check (time-sensitive):** G4 (curve zone), G7 (concentration)  
**Re-compute:** S1–S5 from current BondingCurveState. Social: use cached values.

```
pop_best() → age check → get current BC state →
  re-check G4 (classify zone, check ceiling) →
  re-check G7 (concentration < 15%) →
  re-compute on_chain_score → floor check (≥30) →
  apply social_multiplier → final_score ≥ 40 →
  compute position size → submit entry
```

If any check fails → drop candidate, pop next. Only one entry per slot-open event.

### 3.5 Continuous Queue Maintenance

While queued, candidates receive ShredStream trade updates. Immediate removal on:
- Creator sell (from trade event cross-ref with creator_map)
- real_sol > 20.0 (G4 TooLate ceiling exceeded)
- Single wallet concentration > 15% (G7 violation)
- Token age > 300s

---

## 4. Feed Wiring

### 4.1 create_v2 Dual-Discriminator Support

**Problem:** Pump.fun migrated to `create_v2`. Missing it = silently losing ~50% of new tokens.

**Discriminator bytes:**

```rust
/// Legacy create: SHA256("global:create")[..8]
const CREATE_DISCRIMINATOR: [u8; 8] = [24, 30, 200, 40, 5, 28, 7, 119];

/// New create_v2: SHA256("global:create_v2")[..8]
const CREATE_V2_DISCRIMINATOR: [u8; 8] = [250, 108, 44, 12, 241, 75, 254, 108];
```

> **⚠️ VERIFY BEFORE DEPLOY:** Confirm bytes against a live mainnet `create_v2` transaction. If pump.fun uses non-standard Anchor derivation, extract correct bytes from tx data[0..8].

**Parsing function (shredstream.rs):**

```rust
/// Parse pump.fun create/create_v2. Returns (FeedEvent, mint, creator).
/// Account layout (both versions):
///   [0] mint  [2] bondingCurve  [7] user (creator)
fn parse_pump_create(
    tx: &VersionedTransaction, slot: u64, now_ms: u64,
) -> Option<(FeedEvent, [u8; 32], [u8; 32])> {
    let keys = tx.message.static_account_keys();
    for ix in tx.message.instructions() {
        let prog = keys.get(ix.program_id_index as usize)?;
        if prog.to_bytes() != PUMP_PROGRAM_ID || ix.data.len() < 8 { continue; }
        let disc = &ix.data[..8];
        if disc != CREATE_DISCRIMINATOR && disc != CREATE_V2_DISCRIMINATOR { continue; }
        if ix.accounts.len() < 8 { continue; }

        let mint = keys.get(ix.accounts[0] as usize)?.to_bytes();
        let creator = keys.get(ix.accounts[7] as usize)?.to_bytes();
        let event = FeedEvent::TokenCreated(TokenCreatedEvent {
            mint, ts_ms: now_ms, is_mayhem: false, is_tokenized_agent: false,
        });
        return Some((event, mint, creator));
    }
    None
}
```

**Integration:** In `process_grpc_entry`, check for creates **before** trades/migrations. Populate `creator_map` immediately on detection. ShredStream gives us the mint + creator 80–200ms before PumpPortal's TokenCreated event.

### 4.2 Creator Sell Detection

Implemented in SniperEngine event handler (needs `creator_map` access):

```rust
// On each ShredStream sell event:
if !trade.is_buy {
    if self.creator_map.get(&trade.mint) == Some(&trade.trader) {
        // Open position on this mint → emergency full exit
        // Queued candidate on this mint → remove from queue
        // Mark bc_state.creator_sold = true
    }
}
```

No new FeedEvent variant needed — `CreatorSell { mint, ts_ms }` already exists in `feeds/mod.rs`.

### 4.3 Graduation Detection via Helius accountSubscribe

**Mechanism:** Subscribe to the bonding curve account. Detect graduation when the `complete` flag flips to `true`.

**Bonding curve account layout (Anchor):**
```
Offset  Field                    Type
[0..8]  discriminator            [u8; 8]
[8..16] virtual_token_reserves   u64
[16..24] virtual_sol_reserves    u64
[24..32] real_token_reserves     u64
[32..40] real_sol_reserves       u64
[40..48] token_total_supply      u64
[48..49] complete                bool  ← GRADUATION FLAG
```

**Implementation:** Reuse the `price_feed.rs` `accountSubscribe` WebSocket pattern:

```rust
pub struct BondingCurveSubscriber {
    ws_url: String,
    subscriptions: DashMap<[u8; 32], u64>,  // mint → subscription_id
    event_tx: Sender<FeedEvent>,
}

// On each accountNotification:
//   1. Decode fields from account data bytes
//   2. Update BondingCurveState.vsol, .vtok, .real_sol (authoritative)
//   3. Set vsol_source = Confirmed
//   4. If complete == true → emit FeedEvent::Migration { mint, source: ShredStream }
//   5. Unsubscribe on graduation or token age-out (300s)

// Lifecycle:
//   subscribe() → on ShredStream create detection (before PumpPortal)
//   unsubscribe() → on graduation, position close + no queue entry, age-out
```

**Fallback on Helius WS disconnect:** Estimate vsol from ShredStream trade sums:
```
estimated_vsol = 30.0 + Σ(buy_sol × 0.9875) − Σ(sell_sol / 0.9875)
```
Flag as `VsolSource::Estimated`. Less accurate but sufficient for G4 zone classification.

### 4.4 BondingCurveState Tracker

Core data structure updated on every ShredStream trade event for the mint.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsolSource { Confirmed, Estimated }

pub struct BondingCurveState {
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],
    pub creator: [u8; 32],
    pub is_mayhem_mode: bool,

    // Curve state
    pub vsol: f64,                // virtual SOL reserves
    pub vtok: f64,                // virtual token supply
    pub real_sol: f64,            // vsol - 30.0
    pub vsol_source: VsolSource,

    // Trade tracking
    pub trade_count: u32,
    pub buy_count: u32,
    pub sell_count: u32,
    pub unique_wallets: u32,
    pub wallet_set: HashSet<[u8; 32]>,

    // Concentration (G7)
    pub wallet_token_holdings: HashMap<[u8; 32], u64>,

    // Slot tracking (G2)
    pub create_slot: u64,
    pub create_slot_wallets: HashSet<[u8; 32]>,
    pub create_slot_sol: f64,

    // First trades (G1)
    pub first_5_trades: Vec<TradeRecord>,

    // Velocity (S1)
    pub vsol_per_second: f64,

    // Sell pressure (S4)
    pub first_sell_index: u32,       // u32::MAX if no sells
    pub first_sell_pct: f32,
    pub recent_events: VecDeque<bool>, // last 5: true=sell (cascade detection)

    // Timing
    pub created_at_ms: u64,
    pub last_trade_ms: u64,
    pub last_buy_ms: u64,            // momentum stop: 15s no buys

    // Status
    pub creator_sold: bool,
    pub graduated: bool,
    pub first_buy_sol: f64,          // S1 sub-10-trade substitute

    // Social metadata (PumpPortal, async)
    pub social_enriched: bool,
    pub token_name: Option<String>,
    pub token_symbol: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub twitter: Option<String>,
    pub telegram: Option<String>,
    pub website: Option<String>,
}
```

**Update logic per trade event:**

```rust
impl BondingCurveState {
    pub fn on_trade(&mut self, trade: &TradeEvent, now_ms: u64) {
        self.trade_count += 1;
        self.last_trade_ms = now_ms;
        self.age_secs = (now_ms - self.created_at_ms) as f64 / 1000.0;

        // Wallet tracking
        if self.wallet_set.insert(trade.trader) {
            self.unique_wallets += 1;
        }

        if trade.is_buy {
            self.buy_count += 1;
            self.last_buy_ms = now_ms;

            // Concentration tracking (G7, first 20 trades)
            if self.trade_count <= 20 {
                *self.wallet_token_holdings
                    .entry(trade.trader).or_insert(0) += trade.token_amount;
            }

            // First non-creator buy size (S1 substitute)
            if self.first_buy_sol == 0.0 && trade.trader != self.creator {
                self.first_buy_sol = trade.sol_amount as f64 / 1e9;
            }

            // Estimated vsol update (if Helius WS is down)
            if self.vsol_source == VsolSource::Estimated {
                self.vsol += (trade.sol_amount as f64 / 1e9) * 0.9875;
                self.real_sol = self.vsol - 30.0;
            }
        } else {
            self.sell_count += 1;

            // First sell tracking (S4)
            if self.first_sell_index == u32::MAX {
                self.first_sell_index = self.trade_count - 1;
                // Compute sell percentage of trader's holdings
                let held = self.wallet_token_holdings
                    .get(&trade.trader).copied().unwrap_or(0);
                self.first_sell_pct = if held > 0 {
                    trade.token_amount as f32 / held as f32
                } else {
                    1.0 // unknown holdings, assume full exit
                };
            }

            if self.vsol_source == VsolSource::Estimated {
                self.vsol -= (trade.sol_amount as f64 / 1e9) / 0.9875;
                self.real_sol = self.vsol - 30.0;
            }
        }

        // Cascade tracking (last 5 events)
        self.recent_events.push_back(!trade.is_buy);
        if self.recent_events.len() > 5 {
            self.recent_events.pop_front();
        }

        // Velocity (S1)
        let age = self.age_secs.max(1.0);
        self.vsol_per_second = self.real_sol / age;

        // G1: first 5 trades
        if self.first_5_trades.len() < 5 {
            self.first_5_trades.push(TradeRecord {
                trader: trade.trader,
                is_buy: trade.is_buy,
                sol_amount: trade.sol_amount,
                token_amount: trade.token_amount,
                slot: trade.slot,
            });
        }

        // G2: create-slot tracking
        if trade.slot == self.create_slot {
            self.create_slot_wallets.insert(trade.trader);
            if trade.is_buy {
                self.create_slot_sol += trade.sol_amount as f64 / 1e9;
            }
        }
    }

    /// Update from authoritative Helius accountSubscribe data.
    pub fn on_helius_update(&mut self, vsol_lamports: u64, vtok: u64, complete: bool) {
        self.vsol = vsol_lamports as f64 / 1e9;
        self.vtok = vtok as f64;
        self.real_sol = self.vsol - 30.0;
        self.vsol_source = VsolSource::Confirmed;
        if complete { self.graduated = true; }
    }

    /// Reset for a new mint.
    pub fn new(mint: [u8; 32], bonding_curve: [u8; 32], creator: [u8; 32],
               created_at_ms: u64, create_slot: u64) -> Self {
        Self {
            mint, bonding_curve, creator,
            is_mayhem_mode: false,
            vsol: 30.0, vtok: 1.073e9, real_sol: 0.0,
            vsol_source: VsolSource::Estimated,
            trade_count: 0, buy_count: 0, sell_count: 0,
            unique_wallets: 0, wallet_set: HashSet::new(),
            wallet_token_holdings: HashMap::new(),
            create_slot, create_slot_wallets: HashSet::new(), create_slot_sol: 0.0,
            first_5_trades: Vec::with_capacity(5),
            vsol_per_second: 0.0,
            first_sell_index: u32::MAX, first_sell_pct: 0.0,
            recent_events: VecDeque::with_capacity(6),
            created_at_ms, last_trade_ms: created_at_ms, last_buy_ms: created_at_ms,
            creator_sold: false, graduated: false,
            first_buy_sol: 0.0,
            social_enriched: false,
            token_name: None, token_symbol: None, description: None,
            image_url: None, twitter: None, telegram: None, website: None,
        }
    }

    pub fn real_inflow(&self) -> f64 { self.real_sol }
    pub fn max_wallet_concentration(&self) -> f32 {
        let total: u64 = self.wallet_token_holdings.values().sum();
        if total == 0 { return 0.0; }
        let max = self.wallet_token_holdings.values().copied().max().unwrap_or(0);
        max as f32 / total as f32
    }
}
```

---

## 5. Data Dependencies

### 5.1 smart_wallets.json

**Purpose:** S5 smart money signal. Without this file, S5 contributes 0 points (neutral).

```json
{
  "tier1": ["pubkey_base58", "..."],
  "tier2": ["pubkey_base58", "..."]
}
```

| Parameter | Value |
|---|---|
| Tier 1 size | Top 100 pump.fun traders by PnL → 15 pts |
| Tier 2 size | Top 101–500 → 10 pts |
| Source | GMGN.ai leaderboard scrape or Helius historical indexing |
| Refresh cadence | Weekly (Sunday 06:00 UTC cron job) |
| Refresh method | Script that scrapes GMGN.ai `/api/leaderboard/pump` or queries Helius for top PnL wallets on pump.fun trades in last 30 days |
| Cold start behavior | S5 = 0 pts (neutral, not penalized). Log warning at startup. |
| File location | `data/smart_wallets.json` |
| Runtime loading | Load into `HashSet<[u8; 32]>` at startup. Reload on file change (inotify or periodic 1hr check). |

**⚠️ EMPIRICAL CALIBRATION:** 100/500 split is a starting point. After 500 trades, analyze whether tier-1 wallet presence actually predicts our wins at a higher rate than tier-2. If not, flatten to a single tier.

### 5.2 dumper_wallets.json

**Purpose:** S5 negative signal. Wallets that frequently appear in our losing trades.

```json
{
  "dumpers": ["pubkey_base58", "..."]
}
```

| Parameter | Value |
|---|---|
| Source | Self-populated from our own trade log |
| Population logic | After each losing trade: identify wallets that bought within 5 trades before our entry AND sold before our exit. If a wallet appears in ≥ 3 losing trades → add to dumpers. |
| Initial cold start | Empty file, 0 pts penalty |
| Scoring impact | Any dumper wallet in first 10 buys → −10 pts (from S5) |
| File location | `data/dumper_wallets.json` |
| Update cadence | After every 50 trades (batch update, not per-trade) |

### 5.3 Dev History Cache

**Purpose:** SS1 (dev wallet history) and G3 (serial rugger gate).

| Parameter | Value |
|---|---|
| API | Helius `getAssetsByCreator(creator_pubkey)` |
| Cache structure | `DashMap<[u8; 32], DevHistoryEntry>` keyed by creator pubkey |
| Cache TTL | 1 hour |
| On cache hit | Return cached `dev_tokens_launched`, `rug_count`, `success_rate` |
| On cache miss | Fire async Helius request. Return neutral immediately: G3=PASS, SS1=10,000 bps (neutral) |
| On Helius failure | Same as cache miss: return neutral, log warning. Do NOT block or retry synchronously. |
| Helius rate limit | Respect 10 RPS limit. Queue requests, drop if queue > 50. |

```rust
pub struct DevHistoryEntry {
    pub creator: [u8; 32],
    pub tokens_launched: u16,
    pub rug_count: u16,
    pub success_count: u16,   // graduated OR reached ≥12 real_sol
    pub success_rate_bps: u16, // success_count / tokens_launched × 10_000
    pub is_blacklisted: bool,
    pub fetched_at_ms: u64,
}

impl DevHistoryEntry {
    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.fetched_at_ms) > 3_600_000 // 1 hour
    }
}
```

---

## 6. Phase 0 Logging

**Phase 0 runs always, even when the sniper is disabled.** Every TokenCreated event (from ShredStream create detection or PumpPortal) is logged to build the training dataset.

### 6.1 SniperCreateLog Struct

```rust
#[derive(Debug, Serialize)]
pub struct SniperCreateLog {
    pub mint: String,              // base58
    pub creator: String,           // base58
    pub timestamp_ms: u64,
    pub source: String,            // "shredstream" or "pumpportal"
    // Social metadata (from PumpPortal, may be null if ShredStream-only)
    pub name: Option<String>,
    pub ticker: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
    pub twitter: Option<String>,
    pub telegram: Option<String>,
    pub website: Option<String>,
    // Curve state at creation
    pub initial_vsol: f64,         // always 30.0 at creation
    pub bonding_curve_pubkey: String, // base58
    // Detection metadata
    pub is_mayhem: bool,
    pub is_tokenized_agent: bool,
    pub create_version: u8,        // 1 or 2 (discriminator version)
}
```

### 6.2 Logging Implementation

```rust
pub struct Phase0Logger {
    file: tokio::sync::Mutex<tokio::fs::File>,
    path: String,
}

impl Phase0Logger {
    pub async fn new(path: &str) -> Self {
        let file = tokio::fs::OpenOptions::new()
            .create(true).append(true).open(path).await
            .expect("Failed to open Phase 0 log file");
        Self { file: tokio::sync::Mutex::new(file), path: path.to_string() }
    }

    pub async fn log_create(&self, entry: &SniperCreateLog) {
        let mut line = serde_json::to_string(entry).unwrap_or_default();
        line.push('\n');
        let mut file = self.file.lock().await;
        let _ = tokio::io::AsyncWriteExt::write_all(&mut *file, line.as_bytes()).await;
    }
}
```

**File:** `data/sniper_create_log.jsonl`  
**Format:** One JSON object per line (JSONL)  
**Rotation:** No rotation needed initially. At ~1 token/2–3s = ~30K/day ≈ 3MB/day. Rotate weekly if needed.

---

## 7. Paper Mode

### 7.1 Behavior

Paper mode runs the **exact same logic** as live mode — same gates, same scoring, same sizing, same exit engine — but substitutes actual Jito bundle submission and sell TX execution with log entries.

| Component | Live Mode | Paper Mode |
|---|---|---|
| Gate evaluation | Real | Same |
| Score computation | Real | Same |
| Position sizing | Real | Same |
| Entry execution | Jito bundle → gRPC | Log entry event, record `entry_price` |
| Exit monitoring | ShredStream events + ticks | Same (real events, simulated decisions) |
| Sell execution | RPC sendTransaction | Log exit event, compute PnL from real curve |
| PnL computation | Real SOL received | Computed: `(exit_price - entry_price) / entry_price × position_sol` |
| Trade log | `data/sniper_trades.jsonl` | `data/sniper_paper_trades.jsonl` |
| WinRateTracker | Updated with real results | Updated with simulated results |

### 7.2 Paper Entry

```rust
fn submit_paper_entry(&mut self, mint: &[u8; 32], size_sol: f64,
                       bc_state: &BondingCurveState, now_ms: u64) {
    let entry_price = bc_state.vsol * bc_state.vsol / BondingCurveState::K;
    let tokens_notional = size_sol * 0.9875 / entry_price; // fee-adjusted

    let position = SniperPosition {
        mint: *mint,
        entry_price,
        entry_vsol: bc_state.vsol,
        position_sol: size_sol,
        tokens_notional,
        tokens_remaining: tokens_notional,
        entry_ms: now_ms,
        paper_mode: true,
        // ... exit engine state ...
    };

    self.open_positions.insert(*mint, position);
}
```

### 7.3 Paper Exit

Same exit engine logic runs. When a sell would be executed:

```rust
fn execute_paper_sell(&mut self, mint: &[u8; 32], tokens: f64,
                       reason: ExitReason, bc_state: &BondingCurveState, now_ms: u64) {
    let current_price = bc_state.vsol * bc_state.vsol / BondingCurveState::K;
    let sol_received = tokens * current_price * 0.9875; // fee-adjusted
    // Log to sniper_paper_trades.jsonl with all fields
    // Update WinRateTracker with paper result
}
```

### 7.4 Promotion Criteria (Paper → Live)

Paper mode must demonstrate viability before promoting to live trading. All criteria must be met simultaneously:

| Criterion | Threshold | Rationale |
|---|---|---|
| Sample size | ≥ 100 paper trades | Statistical minimum for WR estimate (±9.8% at 95% CI) |
| Win rate | ≥ 48% | Above break-even for this payoff structure (see SNIPER_SIGNAL_SPEC.md §Exit Architecture) |
| Net PnL | > 0 SOL | System must be profitable, not just frequent |
| Max drawdown | < 20% of starting paper balance | Demonstrates risk management works |
| Consecutive losses | < 15 max streak | No degenerate losing spirals |

**Promotion is manual.** The system logs a recommendation when all criteria are met but does NOT auto-promote. The operator reviews paper trade logs, confirms the metrics, then sets `paper_mode: false` in config.

**Paper balance tracking:**
```rust
pub struct PaperTracker {
    pub starting_balance_sol: f64,    // virtual, default 5.0 SOL
    pub current_balance_sol: f64,
    pub high_water_mark_sol: f64,
    pub total_trades: u64,
    pub wins: u64,
    pub losses: u64,
    pub net_pnl_sol: f64,
    pub max_drawdown_pct: f64,
    pub consecutive_losses: u32,
    pub max_consecutive_losses: u32,
    pub promotion_eligible: bool,     // true when all criteria met
}
```

---

## 8. Error Handling

### 8.1 Per-Source Fallback Table

| Source | Failure Mode | Fallback | Alert Level |
|---|---|---|---|
| Helius `getAssetsByCreator` (SS1, G3) | Timeout/error | Return neutral: G3=PASS, SS1=10,000 bps | `warn` |
| Helius BC `accountSubscribe` (§4.3) | WS disconnected | Fall back to ShredStream trade-sum vsol estimate (`VsolSource::Estimated`) | `warn` + reconnect loop |
| PumpPortal social data | Missing/timeout | SS4 metadata = 0 pts, SS3 twitter = -800 bps, token name/symbol = "unknown" | `debug` |
| `smart_wallets.json` | File not found | S5 = 0 pts (neutral). Log warning at startup only (not per-trade). | `warn` (once) |
| `dumper_wallets.json` | File not found | S5 negative signal disabled. 0 pts penalty. | `info` (once) |
| ShredStream stale (>45s) | Feed stale | **Pause new entries.** Existing positions continue with time-based exits. Alert operator. Resume on reconnect. | `error` + alert |
| Sell TX failure (exit) | RPC error | Retry up to 3× with 200ms intervals. After 3 failures → alert operator, mark position as `exit_failed`. | `error` + alert |
| Jito bundle submission | gRPC error | Retry 1× with 100ms delay. Log rejection reason. Do NOT retry aggressively — stale bundles are worse than no entry. | `warn` |
| PumpPortal WebSocket | Disconnected | ShredStream create detection continues (dual-path). Social metadata unavailable → SS scores use neutral defaults. | `warn` + reconnect |
| Helius RPC general | Rate limited (429) | Exponential backoff: 100ms → 200ms → 400ms → ... → 5s cap. Queue requests, drop if queue > 50. | `warn` |

### 8.2 Circuit Breakers

```rust
pub struct SniperCircuitBreaker {
    /// Pause entries when ShredStream has not delivered data for this many ms.
    pub feed_stale_threshold_ms: u64,     // default: 45_000
    /// Pause entries after this many consecutive bundle rejections.
    pub max_consecutive_rejections: u32,   // default: 10
    /// Pause entries after this many consecutive exit failures.
    pub max_consecutive_exit_fails: u32,   // default: 3
    /// Resume after this many ms of healthy operation.
    pub resume_after_ms: u64,             // default: 30_000
    /// Current state
    pub paused: bool,
    pub paused_since_ms: u64,
    pub pause_reason: Option<String>,
}
```

### 8.3 Graceful Degradation Priority

When multiple systems are degraded simultaneously, the engine degrades in this order:

1. **Social enrichment down** → Continue with on-chain signals only (social = neutral 1.0×). Minimal impact.
2. **Helius BC subscribe down** → Use estimated vsol from ShredStream. Reduced accuracy but functional.
3. **PumpPortal down** → ShredStream create detection still works. No social metadata. G0 mayhem detection may be unreliable.
4. **ShredStream down** → **Full pause.** No reliable trade data = no scoring = no entries. Existing positions exit on time stops.
5. **Jito gRPC down** → No entries possible. Existing positions exit normally via RPC sells.

---

## 9. canary.json Sniper Config Block

Complete `"sniper": {}` block, ready to paste into `canary.json`:

```json
{
  "sniper": {
    "enabled": false,
    "paper_mode": true,

    "signals": {
      "entry_threshold_with_velocity": 50,
      "entry_threshold_no_velocity": 40,
      "min_on_chain_score_for_entry": 30,
      "final_score_threshold": 40,
      "min_trades_for_velocity": 10,
      "skip_mayhem_mode": true,
      "social_disable_before_age_secs": 120
    },

    "gates": {
      "g1_dev_prebuy_first_n_trades": 5,
      "g2_coordinated_min_wallets": 2,
      "g2_coordinated_min_sol": 2.0,
      "g3_serial_rugger_hard_limit": 10,
      "g3_serial_rugger_min_tokens": 5,
      "g3_serial_rugger_max_rug_rate": 0.40,
      "g4_min_real_sol": 2.0,
      "g4_max_real_sol": 20.0,
      "g4_conditional_min_real_sol": 15.0,
      "g4_conditional_min_score": 60,
      "g6_min_creator_balance_sol": 0.05,
      "g7_max_single_wallet_concentration": 0.15,
      "g7_window_trades": 20
    },

    "sizing": {
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
    },

    "exit": {
      "hard_stop_bp": 3000,
      "max_hold_sec": 120,
      "buy_gap_timeout_sec": 15,
      "sell_cascade_count": 3,
      "sell_cascade_window": 5,
      "tp1_threshold_bp": 2000,
      "tp1_sell_permille": 300,
      "tp2_threshold_bp": 5000,
      "tp2_sell_permille": 300,
      "tp3_threshold_bp": 10000,
      "tp3_sell_permille": 300,
      "trail_initial_bp": 1500,
      "trail_tp2_bp": 1000,
      "trail_tp3_bp": 800,
      "exit_on_creator_sell": true,
      "exit_on_graduation": true,
      "sell_slippage_bp": 1500,
      "exit_priority_fee_microlamports": 50000,
      "exit_max_retries": 3,
      "exit_retry_delay_ms": 200,
      "tick_interval_ms": 500
    },

    "execution": {
      "jito_tip_lamports": 100000,
      "buy_slippage_pct": 15,
      "jito_block_engine_url": "https://ny.mainnet.block-engine.jito.wtf"
    },

    "concurrency": {
      "max_open_positions": 10,
      "max_queue_depth": 5,
      "max_token_age_secs": 300
    },

    "feeds": {
      "shredstream_create_v2_enabled": true,
      "helius_bc_subscribe_enabled": true,
      "pumpportal_social_enabled": true,
      "feed_stale_threshold_ms": 45000
    },

    "data": {
      "smart_wallets_path": "data/smart_wallets.json",
      "dumper_wallets_path": "data/dumper_wallets.json",
      "dev_cache_ttl_secs": 3600,
      "social_cache_ttl_secs": 300,
      "pumpfun_api_rps": 5
    },

    "logging": {
      "live_trade_log": "data/sniper_trades.jsonl",
      "paper_trade_log": "data/sniper_paper_trades.jsonl",
      "create_log": "data/sniper_create_log.jsonl",
      "phase0_always_on": true
    },

    "paper": {
      "starting_balance_sol": 5.0,
      "promotion_min_trades": 100,
      "promotion_min_wr_pct": 48,
      "promotion_min_net_pnl_sol": 0.0,
      "promotion_max_drawdown_pct": 20,
      "promotion_max_consecutive_losses": 15,
      "auto_promote": false
    },

    "circuit_breaker": {
      "max_consecutive_rejections": 10,
      "max_consecutive_exit_fails": 3,
      "resume_after_ms": 30000
    }
  }
}
```

---

## 10. Implementation Phases

### Phase 0: Data Collection (no trading)
**Duration:** Can start immediately, runs indefinitely  
**Build:** Phase0Logger + ShredStream create_v2 dual-discriminator + PumpPortal metadata enrichment  
**Output:** `data/sniper_create_log.jsonl` populating continuously  
**Acceptance criteria:**
- [ ] Every TokenCreated event (v1 + v2) logged with all metadata fields
- [ ] Log rate matches observed pump.fun creation rate (~1/2–3s during peak)
- [ ] Logging continues even when `sniper.enabled = false`
- [ ] JSONL format valid — every line is parseable JSON

### Phase 1: BondingCurveState Tracker + Gate Evaluation
**Duration:** 3–5 days  
**Build:** BondingCurveState struct, on_trade update logic, Helius BC accountSubscribe, GateEvaluator (G0–G7), creator_map population  
**Dependencies:** Phase 0 (ShredStream create detection)  
**Acceptance criteria:**
- [ ] BondingCurveState updates on every ShredStream trade for tracked mints
- [ ] Helius accountSubscribe provides authoritative vsol/vtok with VsolSource::Confirmed
- [ ] Fallback to VsolSource::Estimated when Helius WS disconnects
- [ ] All 7 gates (G0–G7) evaluate correctly with test vectors
- [ ] creator_map populated from ShredStream create detection (not waiting for PumpPortal)
- [ ] creator_sell detected within 1 trade event of occurrence
- [ ] Token registry prunes entries older than 300s

### Phase 2: Score Engine + Social Enricher
**Duration:** 3–5 days  
**Build:** S1–S5 on-chain scoring, SS1–SS5 social scoring, SocialEnricher async pipeline, dev history cache  
**Dependencies:** Phase 1 (BondingCurveState provides inputs to all S1–S5)  
**Acceptance criteria:**
- [ ] S1–S5 scores match SNIPER_SIGNAL_SPEC.md scoring tables for test inputs
- [ ] SS1 dev history fetched async, cache hit returns in <1ms, cache miss returns neutral
- [ ] SS2/SS5 disabled for tokens age < 120s
- [ ] Social multiplier clamped to [5,000, 20,000] bps range
- [ ] Blacklisted dev → social_multiplier_bps = 0 (hard veto)
- [ ] on_chain_score floor check (≥30) prevents social-only entries
- [ ] `smart_wallets.json` loaded at startup, missing file → S5=0 + warning

### Phase 3: Position Manager + Sizing
**Duration:** 2–3 days  
**Build:** PositionManager with open[] + CandidateQueue, SniperSizer, WinRateTracker  
**Dependencies:** Phase 2 (scoring feeds PositionManager decisions)  
**Acceptance criteria:**
- [ ] Max 10 concurrent positions enforced
- [ ] Queue accepts candidates, ordered by final_score descending
- [ ] Queue depth capped at 5 with lowest-score eviction
- [ ] On slot open: pop best, re-validate G4+G7+scores, enter or drop
- [ ] Bootstrap sizing: flat 0.02 SOL for first 50 trades
- [ ] Post-bootstrap: score-tier × WR scalar × zone × depth × DD multipliers applied
- [ ] Position size clamped to [0.01, 0.10] SOL
- [ ] WinRateTracker separates optimal/conditional zones
- [ ] Stale candidates (>300s) evicted from queue

### Phase 4: Paper Mode + Exit Engine
**Duration:** 4–6 days  
**Build:** ExitEngine (TP tiers + trailing stop + hard/time/momentum/cascade/creator/graduation stops), paper mode simulation, PaperTracker  
**Dependencies:** Phase 3 (position lifecycle)  
**Acceptance criteria:**
- [ ] Exit engine processes ShredStream events + 500ms ticks
- [ ] Priority order: emergency → stop-loss → time → momentum → TP → hold
- [ ] TP1/TP2/TP3 partial sells at correct percentages (30%/30%/30%)
- [ ] Trailing stop activates after TP1 at 15%, tightens at TP2 (10%), TP3 (8%)
- [ ] Hard stop triggers at −30%
- [ ] Time stop at 120s, momentum stop at 15s no buys, cascade at 3 sells in 5 events
- [ ] Creator sell → immediate full exit
- [ ] Graduation → sell on PumpSwap (live) or simulated (paper)
- [ ] Paper mode logs to `data/sniper_paper_trades.jsonl`
- [ ] Paper PnL computed from real curve movements
- [ ] PaperTracker computes all promotion criteria metrics
- [ ] WinRateTracker updated from paper results

### Phase 5: Live Entry (Jito Integration)
**Duration:** 2–3 days  
**Build:** Jito bundle construction [TX1_buy + tip_tx], gRPC submission, bundle status tracking  
**Dependencies:** Phase 4 (full pipeline validated in paper mode)  
**Acceptance criteria:**
- [ ] TX1 constructs correct pump.fun buy instruction (discriminator [102,6,61,18,1,218,235,234])
- [ ] Tip TX sends correct lamports to Jito tip program
- [ ] Bundle submitted via Jito gRPC to NY block engine
- [ ] Bundle landed → position opens in PositionManager
- [ ] Bundle rejected/expired → cost = tip only, logged as rejection
- [ ] Live trades logged to `data/sniper_trades.jsonl`

### Phase 6: Live Exit (RPC Sell)
**Duration:** 2–3 days  
**Build:** Sell TX construction (pump.fun sell discriminator), RPC sendTransaction with priority fee, retry logic  
**Dependencies:** Phase 5 (live positions exist that need real exits)  
**Acceptance criteria:**
- [ ] Sell TX constructs correct pump.fun sell instruction (discriminator [51,230,133,164,1,127,131,173])
- [ ] Priority fee applied (50,000 microlamports default)
- [ ] Retry up to 3× with 200ms intervals on RPC failure
- [ ] Partial sells calculate correct token amounts for TP tiers
- [ ] Graduation exit sells on PumpSwap (different program, different instruction)
- [ ] Exit failure → alert operator, position marked as `exit_failed`
- [ ] All sell proceeds reconciled against expected amounts

### Phase 7: Data Pipeline + Calibration
**Duration:** Ongoing  
**Build:** smart_wallets.json refresh script, dumper_wallets.json self-population, calibration analysis tools  
**Dependencies:** Phase 6 (need real trade data for calibration)  
**Acceptance criteria:**
- [ ] smart_wallets.json refresh runs weekly (cron or manual trigger)
- [ ] dumper_wallets.json updated every 50 trades from loss analysis
- [ ] Calibration dashboard or script reads trade logs and reports: WR by zone, WR by score tier, PnL distribution, drawdown curves, exit reason distribution, TP hit rates
- [ ] Parameter recommendations generated at 200/500/1000 trade milestones per SNIPER_SIGNAL_SPEC.md calibration roadmap

### Build Order Summary

```
Phase 0 ──► Phase 1 ──► Phase 2 ──► Phase 3 ──► Phase 4 ──► Phase 5 ──► Phase 6
  │                                                                         │
  │ (runs continuously in parallel)                                         │
  └─────────────────────────────────────────────────────────────────────────►│
                                                                   Phase 7 (ongoing)
```

**Critical path:** Phases 0–4 must complete before any trading. Phase 4 paper validation produces the data needed to justify going live in Phase 5.

**Parallel work:** Phase 0 logging and smart_wallets.json sourcing can begin immediately. Helius BC accountSubscribe pattern exists in price_feed.rs and can be adapted early.

---

## Appendix A: Open Calibration Questions

These parameters are set to reasonable defaults but require empirical tuning after sufficient trade data:

| Parameter | Default | Calibrate When | Method |
|---|---|---|---|
| `max_open_positions` | 10 | 500 trades | Measure avg/max slot utilization. Reduce if avg < 5. |
| `max_queue_depth` | 5 | 500 trades | Measure queue overflow rate. Increase if > 10% of signals dropped. |
| `g4_conditional_min_score` | 60 | 1000 trades | Top 25–30% of scores in 15–20 zone should pass. Adjust to match. |
| `g7_max_concentration` | 0.15 | 500 trades | False positive rate (good tokens blocked). Adjust ±0.05. |
| `hard_stop_bp` | 3000 | 200 trades | Distribution of max drawdown on winners. Widen if >10% of winners touch -25%. |
| `buy_gap_timeout_sec` | 15 | 100 trades | Inter-buy gap distribution on active tokens. Widen to 20 if normal gaps reach 12–15s. |
| `tp1_threshold_bp` | 2000 | 500 trades | What % reach +20%? Lower to 1500 if <30%. Raise to 2500 if >70%. |
| `trail_initial_bp` | 1500 | 500 trades | Peak-to-exit analysis. Too many stops at +15%? Widen. |
| Smart wallet tier sizes | 100/500 | 500 trades | Does tier-1 presence predict wins at higher rate than tier-2? |
| `create_v2` discriminator bytes | See §4.1 | Pre-deploy | Verify against live mainnet transaction. |

---

## Appendix B: SniperPosition Struct (Exit Engine State)

```rust
pub struct SniperPosition {
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],
    pub assoc_bonding_curve: [u8; 32],
    pub entry_price: f64,
    pub entry_vsol: f64,
    pub position_sol: f64,
    pub tokens_total: f64,         // total tokens acquired
    pub tokens_remaining: f64,     // after partial sells
    pub entry_ms: u64,
    pub paper_mode: bool,

    // Exit engine state
    pub peak_price: f64,           // highest price seen since entry
    pub peak_pnl_bp: i32,         // max unrealized PnL (basis points)
    pub current_pnl_bp: i32,
    pub trailing_stop_active: bool,
    pub trailing_stop_bp: u16,     // current trailing distance (tightens at each TP)
    pub tp1_hit: bool,
    pub tp2_hit: bool,
    pub tp3_hit: bool,
    pub partial_exits: u8,         // count of partial sells executed
    pub total_sol_received: f64,   // sum of all sell proceeds

    // Trade monitoring during hold
    pub trades_during_hold: u32,
    pub buys_during_hold: u32,
    pub sells_during_hold: u32,
    pub last_buy_ms: u64,
    pub recent_sell_flags: VecDeque<bool>, // last 5 events for cascade

    // Final
    pub exit_reason: Option<SniperExitReason>,
    pub exit_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum SniperExitReason {
    HardStop,          // PnL ≤ -30%
    TrailingStop,      // Below trail from peak
    TimeStop,          // Hold ≥ 120s
    MomentumStop,      // No buys for ≥ 15s
    SellCascade,       // 3+ sells in 5 events
    CreatorSell,       // Creator dumped
    Graduation,        // Token graduated, sold on PumpSwap
    ExitFailed,        // Sell TX failed after retries
}
```

---

*SNIPER_BUILD_SPEC.md v1.0 | 2026-04-04 | Apollo (Opus 4.6)*  
*Implementation blueprint for the SniperEngine. Companion to SNIPER_SIGNAL_SPEC.md v2.0.*