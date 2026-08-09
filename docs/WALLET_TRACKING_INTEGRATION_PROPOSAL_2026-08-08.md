# Dev Wallet Tracking & Whale Signal Integration — Refined End-to-End Proposal v2

**Date:** 2026-08-08 (revised)
**Author:** Principal Citadel Pump.fun Memecoin Quant
**Status:** Proposal — awaiting build authorization
**Supersedes:** v1 (same date)
**Wallet list:** 302 wallets parsed from Google Doc, saved to `data/tracked_wallets.json`

---

## Research Foundation (ArXiv + Firecrawl)

This proposal is grounded in 8 academic papers and 10+ industry sources researched via ArXiv API and Firecrawl web scraping. Each design decision below cites the evidence that motivates it.

### ArXiv Papers

| Paper | Key Finding We Leverage |
|---|---|
| **arXiv:2504.07132** — SolRPDS: Solana Rug Pull Dataset (ACM CODASPY 2025) | 62,895 suspicious pools from 3.69B txns; **inactivity states** are a key rug indicator; rug patterns are detectable from liquidity-add/remove timing |
| **arXiv:2603.24625** — From Hype to Collapse: Rug Pulls on Solana (2026) | 3 distinct rug patterns: **Freeze Authority Abuse, Liquidity Withdrawal, Pump-and-Dump**; 76,469 rug tokens in 100K sampled; "highly organized group behaviors" — rug families are real and common |
| **arXiv:2512.11850** — The Memecoin Phenomenon on Solana/pump.fun (IEEE ISCC 2025) | pump.fun = 71.1% of Solana token mints, 40-67.4% of DEX txns; **<2% graduate**; 60K-260K daily active users — the signal-to-noise ratio is extreme |
| **arXiv:2606.08232** — Hour-Aware Memecoin Trading (our companion paper, q-fin.TR) | 15-day deployment, 190 trades, 40.5% win rate; **top 3 trades (1.6%) flip cumulative return** — confirms fat-tail design; rejection-tracking shows 56.25% of rejected events hit -50% drawdown |
| **arXiv:2505.09313** — Sybil Address Detection: Subgraph Feature Propagation (IEEE ICBC 2025) | Two-layer deep transaction subgraph + **temporal features** (first tx, gas acquisition, participation, last tx); lightGBM classifier; F1/AUC >0.9; method directly transferable to wallet-cluster detection |
| **arXiv:2509.01168** — Rug Pull Detection on TON Blockchain (2025) | Gradient Boosting, **5-minute early-warning window**; TVL-based vs idle-based rug definitions; AUC 0.891; feature distributions differ across exchanges → platform-aware models needed |
| **arXiv:2202.03866** — NFT Wash Trading (2022) | Wash-trading quantification methodology; **on-chain volume authenticity scoring** — transferable to memecoin fake-volume detection |
| **arXiv:2209.04603** — Fighting Sybils in Airdrops (2022) | Multi-account detection heuristics; **funding-source clustering** — same funder → same operator |

### Industry Sources (Firecrawl)

| Source | Key Insight We Leverage |
|---|---|
| **DeFade.org** — Dev Wallet Analysis Guide + API (22 endpoints) | Dev wallet history is the #1 rug predictor: **token creation rate, previous token outcomes, dev sell timing, wallet age + SOL source**; serial ruggers create 50+ tokens/month |
| **RugRade.fun** — Real-time pump.fun rug radar | **42 rug heuristic patterns** scored in <800ms: bundle patterns, sniper buys, dev-wallet concentration, fake-volume detection |
| **NoesisAPI** — Bundle Detection API | Holder classification: **Bundlers / Snipers / Rat Traders / Organic**; risk rating: <10% bundler = green, >25% = red; GMGN bundle detector + wallet freshness from Helius history |
| **MadeOnSol** — Smart Money Identification | 4 criteria: (1) **20+ token sample size** minimum, (2) consistency across 7d/30d/all-time, (3) positive net PnL (not just win rate), (4) **low bot_confidence** — filter out MEV/sniping bots |
| **MadeOnSol** — Whale Tracking Guide | 4 whale types: Volume / Profit / Smart Money / Insider; **quality over quantity — 3-10 focused wallets > 50 random**; backtrack from winning tokens to find early buyers |
| **PANTEREX** — Whale Wallet Finding Guide | **30+ trades minimum**; win rate must hold over time; clean funding (no airdrop/team tokens); sensible position sizing; recent activity required |
| **Solana Forensic Analysis Tool** (GitHub, apostleoffinance) | Transaction flow mapping, wallet behavior analysis, **clustering of suspicious activity**, entity labeling — open-source reference architecture |
| **Pumpfun Rug Watch Dashboard** (GitHub) | Deployer action tracking, sniper wallet identification, early seller analysis, transaction flow mapping |

---

## Executive Summary

The mev_bot already has a sophisticated creator-analysis stack (`pump-quant-wallet-graph` crate) with 7-archetype classification, a launch-outcome ledger, deployer credibility scoring, smart-money PnL screening, and wallet-graph clustering. **None of it is fully operational.** The classifier is starved (6 of 9 fields hardcoded to 0), the ledger isn't persisted (wiped on every daemon restart), the wallet graph has no edges, the PnL screen gets no trade data, and the system is purely punitive — it never rewards good creators.

Your 302-wallet list provides the addresses to OBSERVE. The academic literature and industry tooling provide the methods to SCORE, CLUSTER, and ACT on what those wallets do. This proposal integrates both into a single end-to-end pipeline spanning all 8 lifecycle stages you specified: refinement, pre-entry, entry, hold, monitor, exit, post-exit, and cross-session calibration.

The single highest-leverage fix: LaserStream **already extracts** `account_keys: Vec<[u8; 32]>` from every decoded transaction but hardcodes `buyer_entity: 0`. The raw wallet addresses are right there — they just never reach the engine. Fixing this one value unlocks 5 subsystems.

---

## What Exists (Codebase Audit)

### `pump-quant-wallet-graph` Crate — 5 Modules

| Module | What It Does | Status |
|---|---|---|
| `creator_classifier.rs` (22K chars) | 7-archetype `CreatorClass` enum: SerialRug, VolumeFarmer, Copycat, ShortLivedRunner, StreamerMeta, CommunityBuilder, Unknown. `classify_creator()` takes 9 input fields. | **Starved** — 6 of 9 fields hardcoded to 0. Only VolumeFarmer and Unknown reachable. CommunityBuilder (the positive signal) is structurally unreachable. |
| `creator_ledger.rs` (27K chars) | Tracks `record_launch()`, `record_migration()`, `record_rug()` per creator. Maintains launch count, migration count, rug count, survival rate. | **In-memory only.** No serde, no disk I/O. Every daemon restart wipes all creator history. |
| `deployer_credibility.rs` (7K chars) | Prior-launch count, serial-deploy burst detection, verified vs self-claimed partnerships, social reach. | **Reduce-only.** Only penalizes bad devs. Never boosts good ones. |
| `tier2_wallet_graph.rs` (17K chars) | UnionFind clustering, typed edges (SameCreator, Funding, SameFeePayer, CoBuyFirstN, SellSync). Family grouping. | **Dormant.** `add_edge()` never called. No edges populated. |
| `smart_money.rs` | PnL truth rules, lagged-shadow follower PnL, self-dealing exclusion (§28). | **Wired but unfed.** Never receives trade data. |

### Engine Integration (Confirmed)

- `creator_class` flows into gate decision as fingerprint feature
- `archetype.rs` notes "creator class carries unusual weight"
- Engine feeds ledger via `record_launch`/`record_migration`/`record_rug` at detection sites (engine.rs:1940-2070)
- `AppEvent::WalletAction` event lane EXISTS (event.rs) — carries `mint`, `followable: bool`, `size_lamports: u64`
- `AppEvent::CreatorAction` with `CreatorActionKind::Init/Buy/Sell/LinkedBuy` — full creator action tracking
- LaserStream adapter decodes `account_keys: Vec<[u8;32]>` per transaction (parse.rs:70) — raw wallet addresses available but unused

### The Critical Blind Spot

LaserStream's `instructions_to_events()` (parse.rs:250, 266, 282, 298) hardcodes `buyer_entity: 0` for all event types. The `account_keys` vector contains every wallet address in the transaction — the buyer, the seller, the fee payer, the creator — but none of them reach the engine. The system is **blind to WHO is trading**. This is the root cause of all 5 dormant subsystems.

---

## The 7 Gaps This Proposal Closes

| # | Gap | Evidence | Fix |
|---|---|---|---|
| **G1** | No wallet identity in events (buyer_entity = 0) | Codebase audit: parse.rs hardcodes 0 | Extract real `buyer_entity` from `account_keys[0]` — already available in every LaserStream transaction |
| **G2** | No tracked-wallet registry | User's 302-wallet list | `TrackedWalletMatcher` — HashSet of tracked addresses, O(1) lookup per transaction, tiered by whale/dev classification |
| **G3** | Creator ledger not persisted | Codebase audit: no serde/disk I/O | Serialize to `data/creator_ledger.json` via serde + atomic write; reload on startup. Academic backing: SolRPDS (2504.07132) confirms longitudinal creator history is the primary rug predictor |
| **G4** | Classifier starved (6 zeros) | Codebase audit: engine feeds zeros | Feed real values from persisted ledger. Academic backing: MadeOnSol's 4-criteria smart money framework, DeFade's dev wallet analysis fields |
| **G5** | No positive signal path | Codebase audit: reduce-only credibility | Tracked Proven/CommunityBuilder creators get trust boost (margin relaxation, not gate bypass). Academic backing: arXiv:2606.08232 fat-tail design — good creators' coins are where the tail events come from |
| **G6** | No rug-type classification | arXiv:2603.24625: 3 distinct rug patterns | Extend `record_rug()` to classify rug TYPE: FreezeAuthority / LiquidityWithdrawal / PumpAndDump. Different exit strategies per rug type |
| **G7** | No bundle/sniper detection | NoesisAPI bundle classification, RugRade 42 patterns | Add `BundleDetector` that classifies holders as Bundler/Sniper/RatTrader/Organic using first-buy-slot + wallet-freshness analysis. Risk: >25% bundler supply = hard veto |

---

## Wallet Tiering Strategy

Research consensus (PANTEREX, MadeOnSol): **quality over quantity**. 3-10 focused wallets beat 50 random ones. Your 302-wallet list should not be a flat list — it should be tiered.

### Tier Architecture

| Tier | Label | Criteria | Signal Weight | Count (estimated) |
|---|---|---|---|---|
| **T0** | Verified Smart Money | ≥20 tokens traded, positive net PnL, win rate holds across 7d/30d/all-time, low bot confidence | **High** — whale buy = discovery boost, whale sell = trailing tighten | ~10-30 wallets |
| **T1** | Profit Whale | Consistent positive returns, ≥30 trades, sensible position sizing | **Medium** — corroborative signal, needs ≥2 for conviction | ~30-80 wallets |
| **T2** | Volume Whale | Large trade sizes, high frequency, but PnL unverified | **Low** — liquidity signal only, not directional | ~50-100 wallets |
| **T3** | Known Dev Deployer | Creator wallets from the list — deploy tokens on pump.fun | **Contextual** — dev buy = bullish, dev sell = exit trigger, dev history = trust score | ~20-50 wallets |
| **T4** | Insider/Unverified | Wallets connected to project teams or with suspicious funding patterns | **Flag only** — don't follow, but monitor for rug signals | ~30-50 wallets |

**Tiering happens at load time**, not at runtime. The `tracked_wallets.json` file carries a `tier` field per wallet. The §28 PnL truth screen can promote/demote wallets across tiers based on observed performance.

### Why Tiering Matters

From arXiv:2512.11850: pump.fun generates **71.1% of all Solana token mints** with <2% graduation rate. The noise is extreme. A flat 302-wallet list generates 302x more signal traffic than a focused T0 set of 10. The tiering system ensures the engine spends its evaluation budget on high-conviction wallets, not noise.

---

## End-to-End Lifecycle Integration

### Stage 1: Refinement (cross-session)

**What happens:** The refiner runs after 24h paper-trade data collection. It recalibrates config knobs based on observed performance.

**Tracked-wallet integration:**
- Per-wallet PnL attribution: "when we entered because T0 whale X bought, what was our outcome?"
- Per-tier aggregate edge: "do T0-corroborated entries outperform non-corroborated entries?"
- Rug-type frequency: "what fraction of our rejected tokens had PumpAndDump pattern vs FreezeAuthority?"
- Bundle-detection hit rate: "what % of tokens we entered had >10% bundler supply?"

**Config knobs calibrated:**
- `tracked_dev_boost_max_bps` — max trust boost for Proven creators (start 200bps = 20%)
- `tracked_whale_min_corroboration` — minimum T0/T1 whales for conviction (start 2)
- `bundle_rejection_threshold_pct` — bundler supply % that triggers hard veto (start 25%, per NoesisAPI)
- `rug_type_exit_aggression` — per rug-type exit speed multipliers

**Academic backing:** arXiv:2606.08232 — "removing the top 3 trades (1.6% of sample) flips cumulative return unprofitable" → we must identify which trades ARE the tail and preserve them. Per-wallet PnL attribution identifies which whales lead us into tail events.

### Stage 2: Pre-Entry (discovery & screening)

**What happens:** LaserStream streams transactions. The engine screens mints for entry candidacy.

**Tracked-wallet integration:**
1. **Wallet identity extraction** (G1): Every LaserStream transaction now carries the real `buyer_entity` extracted from `account_keys[0]`
2. **Tracked-wallet match** (G2): `TrackedWalletMatcher::check(addr) -> Option<TrackedWalletInfo>` — O(1) HashSet lookup returns tier + label + historical PnL
3. **Dev deployer detection**: When a T3 (Known Dev Deployer) wallet creates a new mint, the mint is tagged with the creator's trust score from the persisted ledger
4. **Whale buy signal**: T0/T1 whale buy on a mint → `discovery_rank_boost` (mint moves up in evaluation priority)
5. **Bundle detection** (G7): First-50-buyers analysis classifies holders as Bundler/Sniper/RatTrader/Organic; >25% bundler = hard veto (NoesisAPI threshold)

**Academic backing:**
- DeFade dev wallet analysis: "number of tokens created, outcome of previous tokens, dev sell behavior, wallet age + SOL source" — all extracted from our persisted creator ledger
- RugRade: 42 rug heuristic patterns scored in <800ms — our pre-entry screen runs a subset of these
- arXiv:2509.01168: 5-minute early-warning window for rug probability — our pre-entry screen computes a 5-min rug-score

### Stage 3: Entry (gate decision)

**What happens:** The gate evaluates a screened mint against entry criteria.

**Tracked-wallet integration:**
1. **Trust boost for Proven creators** (G5): If the mint's creator has `CreatorClass::Proven` or `CommunityBuilder` (now reachable after G4 fix), the gate gets a `margin_relaxation_bps` parameter. This relaxes the cold-start prior threshold by up to `tracked_dev_boost_max_bps` (default 200bps). **This is the first time creator credibility INCREASES a gate parameter** — all existing logic is reduce-only.

2. **Whale corroboration**: If ≥`tracked_whale_min_corroboration` T0/T1 whales have bought this mint, the entry gets a `conviction_multiplier`. This doesn't bypass the gate — it adjusts the prior. The §29 anti-copy-trading law is preserved: we don't blindly follow whales, we use their activity as a Bayesian prior update.

3. **Bundle veto**: If bundler supply > `bundle_rejection_threshold_pct`, hard veto regardless of other signals. This is a new gate criterion.

4. **Creator ledger check**: The persisted ledger is queried for this creator's historical rug rate. A creator with >50% rug rate gets a gate penalty. A creator with >70% migration rate gets the trust boost.

**Constitution note:** The trust boost (G5) requires a §27 amendment. The current law says "don't trust unverified claims." Empirically-verified Proven creators with migration-survival evidence are a different category — the law was written before tracked wallets existed. The amendment would add a "verified-creator boost" clause, distinct from the "unverified-claim penalty" the law currently addresses.

### Stage 4: Hold (position management)

**What happens:** We hold a position. The engine monitors for exit conditions.

**Tracked-wallet integration:**
1. **Whale sell → trailing tighten**: If a T0 whale sells our held mint, the trailing stop tightens by a configurable amount. This is a deterministic, rule-based response — not a discretionary one.

2. **Whale buy → hold pressure**: If new T0/T1 whale buys arrive during hold, the trailing stop widens slightly (counter-signals: someone smart is still accumulating).

3. **Creator dump exit**: If the mint's creator wallet (identified via ledger) sells, trigger `CreatorDump` exit. This already exists as an exit pathway — but currently relies on `CreatorAction::Sell` events that only fire for the known creator. With wallet identity (G1), we can now detect creator sells even when the creator uses a different wallet (funding-graph connected).

4. **Rug-type monitoring** (G6): If on-chain signals indicate FreezeAuthority abuse or LiquidityWithdrawal, the exit becomes immediate market-order regardless of PnL. Different rug types → different exit aggression:
   - FreezeAuthority: immediate full exit (token can be frozen, can't sell)
   - LiquidityWithdrawal: immediate full exit (pool draining)
   - PumpAndDump: accelerated scale-out (dev is pumping before dump)

**Academic backing:**
- arXiv:2603.24625: "extremely short lifecycles" for rug tokens → our hold monitoring must detect rug patterns within minutes
- DeFade: "dev wallet that consistently sells within first 30 minutes" = pump-and-dump assembly line → our creator sell timer starts at entry

### Stage 5: Monitor (watched-mint surveillance)

**What happens:** The engine maintains a list of mints under observation (not held, but tracked for potential entry).

**Tracked-wallet integration:**
1. **Priority queueing**: Mints with T0 whale activity get priority in the evaluation queue. With pump.fun's 71.1% token-mint volume (arXiv:2512.11850), the engine must triage. Tracked-wallet activity is the triage signal.

2. **Wallet-graph edge construction** (G4 enabler): As transactions stream in, the wallet graph builds edges:
   - `CoBuyFirstN`: two wallets bought the same mint within first N slots → possible coordination
   - `Funding`: wallet A funded wallet B → same operator (per arXiv:2505.09313 subgraph method)
   - `SellSync`: two wallets sold within same slot → coordinated exit
   These edges feed the clustering engine (UnionFind in tier2_wallet_graph.rs) to identify rug families.

3. **Smart-money PnL screening** (fed for first time): The §28 lagged-shadow PnL screen receives trade data and computes per-wallet realized PnL. Wallets with positive lagged-shadow edge get promoted to T0. This is the meritocracy mechanism — your 302-wallet list is a candidate pool, not a trust list.

### Stage 6: Exit (position closure)

**What happens:** We close a position via TP, hard stop, trailing, or creator-dump.

**Tracked-wallet integration:**
1. **Whale-exit-aligned scale-out**: If T0 whales are selling, our scale-out accelerates. We exit in proportion to whale exit velocity, not ahead of it (we don't front-run our own signal providers).

2. **Deterministic CreatorDump exit**: Now that we have wallet identity (G1), the creator-dump exit can match on wallet address directly, not just on `CreatorAction::Sell` events. This makes the exit deterministic and faster.

3. **Rug-type-specific exit** (G6):
   - FreezeAuthority detected → immediate 100% exit at market
   - LiquidityWithdrawal detected → immediate 100% exit at market
   - PumpAndDump pattern → 50% exit immediately, 50% trail the dev's pump

4. **Bundle-driven exit**: If post-entry analysis reveals bundler wallets starting to dump, accelerate exit.

### Stage 7: Post-Exit (tape & learning)

**What happens:** The trade is recorded in `tape.jsonl` and the brain updates its fingerprint.

**Tracked-wallet integration:**
1. **Tape records tracked-wallet context**: Each tape entry gains a `tracked_wallets_present` field listing which tier wallets were active during the trade lifecycle.

2. **Per-wallet PnL update**: The smart-money PnL screen updates each tracked wallet's realized PnL based on our trade outcome. This is the feedback loop — wallets that led us into profitable trades get stronger signal weight over time.

3. **Brain fingerprint expansion**: The brain's episode memory gains a `tracked_whale_inflow` dimension. Episodes where T0 whales were present are tagged differently from solo entries. This gives the brain better recall discrimination for future similar situations.

4. **Creator ledger update**: The creator's launch outcome is recorded (migration, rug, or moon). Over time, this builds the longitudinal history that feeds the classifier (G4) and the trust boost (G5).

**Academic backing:** arXiv:2606.08232 — "rejection-tracker collected 4,874 forward-sample observations across 184 rejection events; 56.25% reached -50% drawdown" → we must track not just what we traded, but what we rejected and why, to validate the gate over time.

### Stage 8: Cross-Session (persistent learning)

**What happens:** Across daemon restarts and refinement cycles.

**Tracked-wallet integration:**
1. **Creator ledger persistence** (G3): The ledger survives restarts. A prolific community builder stays `Proven` across sessions. This is the foundation — without it, every other subsystem degrades to cold-start every restart.

2. **Per-wallet edge calibration**: Over multiple sessions, we build a per-wallet edge profile: "following T0 whale X produced +2.1% avg edge over 50 trades." Wallets with positive edge get stronger weight. Wallets with negative edge get demoted.

3. **Wallet graph persistence**: The UnionFind clusters (rug families) persist across sessions. A rug family identified in session 1 is still known in session 2. New wallets connected to known rug-family members inherit the family's Toxic classification.

4. **Tier promotion/demotion**: The §28 PnL screen can promote a T2 whale to T0 if its accumulated PnL record meets the 4-criteria smart-money threshold (MadeOnSol: 20+ tokens, consistency, positive PnL, low bot confidence). Conversely, a T0 whale with degrading performance gets demoted.

---

## Build Plan (7 Phases)

### Phase 1: Wallet Identity Pipeline (G1 + G2) — SAFE during data hold

**Scope:**
- Modify `instructions_to_events()` in parse.rs to extract `buyer_entity` from `account_keys[0]` for buy events, `account_keys[1]` for sell events (pump.fun instruction layout: account[0] = buyer/seller, account[1] = counterparty)
- Build `TrackedWalletMatcher` struct in `pump-quant-wallet-graph`:
  - Loads `data/tracked_wallets.json` at startup
  - `HashMap<[u8;32], TrackedWalletInfo>` where `TrackedWalletInfo` = { tier, label, tag, pnl_score }
  - `check(&[u8;32]) -> Option<&TrackedWalletInfo>` — O(1) lookup
  - Thread-safe via `Arc<RwLock<>>` (reads dominate, occasional tier updates)

**Changes trade behavior?** NO — this is observability. The wallet identity reaches the engine as a new field on `MarketTrade` events, but no gate logic changes. Existing gate decisions are unaffected.

**Lines:** ~300 Rust
**Risk:** Low — additive change, no existing logic modified

### Phase 2: Creator Ledger Persistence (G3) — SAFE during data hold

**Scope:**
- Add `serde::Serialize` / `Deserialize` to `CreatorLedger` and its inner types
- Implement `save_to_disk(&self, path: &Path)` — atomic write (write to temp, rename)
- Implement `load_from_disk(path: &Path) -> Result<Self>` — called at daemon startup
- Add `flush()` call on graceful shutdown and periodic 5-minute autosave
- Daemon startup: `CreatorLedger::load_from_disk("data/creator_ledger.json")` → if file exists, load; if not, start fresh

**Changes trade behavior?** NO — the ledger already tracks the same data; this just makes it survive restarts. The in-memory behavior is identical.

**Lines:** ~200 Rust
**Risk:** Low — serialization only, no logic change

### Phase 3: Feed the Classifier (G4) — SAFE during data hold

**Scope:**
- Replace the 6 hardcoded zeros in the classifier feed with real values from the persisted ledger:
  - `prior_launch_count` → `ledger.launch_count(creator)`
  - `prior_rug_count` → `ledger.rug_count(creator)`
  - `prior_migration_count` → `ledger.migration_count(creator)`
  - `avg_launch_survival_slots` → computed from ledger entries
  - `dev_sell_pattern` → derived from recorded creator sell timing
  - `wallet_age_slots` → from creator's first-seen slot
- The `CommunityBuilder` archetype (requires high migration rate + low rug rate + moderate launch count) becomes reachable for the first time

**Changes trade behavior?** TECHNICALLY YES — the creator_class fingerprint changes, which affects gate decisions. BUT: during paper trading with `entry_mode_leaves_enable = 0`, the gate still requires on-chain confirmation. The classifier change only affects the fingerprint weight, not the gate threshold. The impact is marginal (fingerprint is one of many features).

**Mitigation:** Build behind a config flag `classifier_real_values_enable: bool` (default false during data hold, flip to true after 24h data collection).

**Lines:** ~150 Rust
**Risk:** Low-medium — config-gated, fingerprint impact only

### Phase 4: Trust Boost + Whale Signal Integration (G5) — BUILT BUT DISABLED until refinement

**Scope:**
- Add `trust_boost_bps` to gate decision: if `creator_class == Proven || CommunityBuilder`, gate threshold relaxes by `trust_boost_bps` (config: `tracked_dev_boost_max_bps`, default 200)
- Add `whale_corroboration` to gate decision: if ≥`tracked_whale_min_corroboration` T0/T1 wallets bought, `conviction_multiplier` applies to position size (not gate threshold — size only)
- Add `bundle_veto` to gate: if `bundler_supply_pct > bundle_rejection_threshold_pct`, hard reject
- Wire `TrackedWalletMatcher` into the `MarketTrade` event processing: when a tracked wallet buys, emit a `WalletAction` event with `followable: true` and the wallet's tier info

**Changes trade behavior?** YES — this is the first positive creator signal. Disabled until refiner runs.

**Config:** `trust_boost_enable: bool` (default false), `whale_corroboration_enable: bool` (default false)

**Lines:** ~400 Rust
**Risk:** Medium — new gate logic, but config-gated and paper-traded before live

### Phase 5: Rug-Type Classification + Bundle Detection (G6 + G7) — BUILT BUT DISABLED

**Scope:**
- Extend `record_rug()` to accept a `RugType` enum: `FreezeAuthority`, `LiquidityWithdrawal`, `PumpAndDump`, `Unknown`
- Add detection logic for each type:
  - FreezeAuthority: monitor `freeze_authority` account changes + token freeze instructions
  - LiquidityWithdrawal: monitor pool reserve withdrawals
  - PumpAndDump: pattern match on creator buy → pump → sell sequence
- Build `BundleDetector`:
  - For each mint, track first-50 buyers' wallet freshness (transaction count from Helius history)
  - Classify: Bundler (fresh + GMGN pattern), Sniper (first-slot + profit), RatTrader (quick flip), Organic
  - Compute `bundler_supply_pct` = bundler-held supply / total supply
  - >25% → hard veto; 10-25% → penalty; <10% → clean

**Changes trade behavior?** YES — new veto and exit conditions. Disabled until refiner runs.

**Config:** `rug_type_detection_enable: bool`, `bundle_detection_enable: bool` (both default false)

**Lines:** ~500 Rust
**Risk:** Medium — new detection logic, but config-gated

### Phase 6: Wallet Graph Construction — SAFE during data hold (observability)

**Scope:**
- Wire `add_edge()` calls in the engine:
  - When two wallets buy the same mint within first N slots → `CoBuyFirstN` edge
  - When wallet A funds wallet B (detected via `account_keys` funding patterns) → `Funding` edge
  - When two wallets sell the same mint within same slot → `SellSync` edge
  - When two mints share the same creator → `SameCreator` edge
- The UnionFind clusters identify rug families. New wallets connected to known rug-family members inherit Toxic classification.
- Persist the graph to `data/wallet_graph.json` (same pattern as ledger persistence)

**Changes trade behavior?** NO — graph construction is passive. Clusters inform the classifier and gate only when the gate logic explicitly queries them, which happens in Phase 4 (disabled).

**Lines:** ~350 Rust
**Risk:** Low — additive, observability only

### Phase 7: Smart-Money PnL Screening Activation — DEFERRED

**Scope:**
- Feed trade data to the §28 lagged-shadow PnL screen
- Compute per-wallet realized PnL across all observed trades
- Apply the 4-criteria smart-money threshold (MadeOnSol: 20+ tokens, consistency, positive PnL, low bot confidence)
- Promote/demote wallets across tiers based on accumulated PnL record

**Changes trade behavior?** YES — this changes which wallets carry signal weight. Deferred until after Phase 4-5 are paper-validated.

**Lines:** ~300 Rust
**Risk:** Medium-high — changes signal weights, needs longitudinal data

---

## Phase Dependency Graph

```
Phase 1 (Wallet Identity) ──────┐
                                 ├─→ Phase 4 (Trust Boost) ──┐
Phase 2 (Ledger Persistence) ───┤                            ├─→ Phase 7 (PnL Screening)
                                 ├─→ Phase 5 (Rug-Type) ──────┘
Phase 3 (Feed Classifier) ──────┘
                                 
Phase 6 (Wallet Graph) ─────────→ (feeds Phase 4 via cluster lookup)
```

**Safe during data hold:** Phases 1, 2, 3, 6
**Built but disabled:** Phases 4, 5 (enable after 24h refinement)
**Deferred:** Phase 7 (enable after Phase 4-5 paper-validated)

---

## Configuration Summary

| Knob | Default | Purpose |
|---|---|---|
| `classifier_real_values_enable` | false (flip after 24h) | Feed real classifier values from persisted ledger |
| `trust_boost_enable` | false (flip after refinement) | Enable positive creator boost |
| `whale_corroboration_enable` | false (flip after refinement) | Enable whale corroboration in gate |
| `rug_type_detection_enable` | false (flip after refinement) | Enable rug-type classification |
| `bundle_detection_enable` | false (flip after refinement) | Enable bundle/sniper detection |
| `tracked_dev_boost_max_bps` | 200 (20%) | Max gate threshold relaxation for Proven creators |
| `tracked_whale_min_corroboration` | 2 | Minimum T0/T1 whales for conviction signal |
| `bundle_rejection_threshold_pct` | 25 | Bundler supply % that triggers hard veto |
| `wallet_graph_persistence_enable` | true | Save wallet graph across restarts |
| `creator_ledger_autosave_interval_secs` | 300 | Autosave frequency for creator ledger |

---

## §27 Constitution Amendment Required

**Current law (§27):** Creator credibility is reduce-only. Bad creators get penalized (veto exit, size haircut). No positive signal path exists.

**Proposed amendment:** Add a "verified-creator boost" clause:
> *A creator with empirically verified Proven status (≥5 prior launches with ≥60% migration rate and 0 rugs) or CommunityBuilder classification (fed by real ledger data) is eligible for a trust boost of up to `tracked_dev_boost_max_bps` (default 200bps). This boost relaxes the cold-start prior threshold but does NOT bypass the on-chain confirmation requirement (`entry_mode_leaves_enable`). The boost is distinct from the existing "unverified-claim penalty" — it rewards demonstrated on-chain history, not self-claimed reputation.*

**Rationale:** The law was written before tracked wallets existed. Its intent was "don't trust unverified claims." Empirically-verified Proven creators with migration-survival evidence are a different category. The boost is config-gated and paper-traded before going live.

---

## Limitations (Honest Quant Disclosure)

1. **302 wallets is a candidate pool, not a verified smart-money list.** The §28 PnL screen must run before any wallet's signal carries weight. Most of the 302 will prove to be noise or bots. This is by design — the list gives us addresses to OBSERVE, not to FOLLOW blindly.

2. **The trust boost is the first positive creator signal.** It introduces a new failure mode: a Proven creator who rugs after building reputation. Mitigation: the boost is capped (200bps = 20% threshold relaxation), config-gated, and the existing rug-detection exit pathways remain fully active. If a Proven creator rugs, the exit fires immediately.

3. **Bundle detection requires wallet-freshness data** (transaction count per wallet from Helius history). This adds latency to the pre-entry screen. Mitigation: freshness is computed async and cached; the pre-entry screen reads from cache, not from RPC calls.

4. **Wallet graph construction is O(n²)** in the naive case (every pair of wallets checked for edges). Mitigation: we only build edges within the first-50-buyers window for each mint, not globally. The graph grows incrementally, not in a batch.

5. **Rug-type classification (G6) depends on detecting freeze authority and liquidity withdrawal events.** These events may not always be visible in LaserStream transaction data. Mitigation: we use the on-chain confirm path (which reads bonding-curve account state) as a secondary check.

6. **The 5-minute early-warning window (arXiv:2509.01168)** is based on TON blockchain data. Solana's faster finality (400ms slot time vs TON's async execution) means our window should be shorter — proposed 2-minute initial rug-score computation, revised based on paper-trade data.

7. **Per-wallet PnL attribution (Phase 7)** requires tracking every trade a wallet makes across every mint we observe. This is data-intensive. Mitigation: we only track PnL for wallets in our tracked list (302 addresses), not all wallets.

8. **The fat-tail design (arXiv:2606.08232)** means the top 1.6% of trades drive cumulative return. Trust-boosted entries into Proven-creator coins are designed to increase our exposure to tail events. This is a feature, not a bug — but it increases variance. The 10% moon-bag retention (existing strategy) mitigates premature exit from tail events.

---

## What I Need From You

1. **Build authorization** — all 7 phases, or a subset?
2. **§27 amendment** — approve the verified-creator boost clause?
3. **Confirm** the 302 list is a watch list (candidates for observation), not a trust list (auto-followed)?
4. **Config defaults** — the 10 knobs listed above, any overrides?
5. **Phase ordering** — Phases 1-3+6 are safe during the data hold. Phases 4-5 built but disabled until refiner runs. Phase 7 deferred. Agree?

---

## Appendix A: ArXiv Paper Abstracts

### arXiv:2504.07132 — SolRPDS: A Dataset for Analyzing Rug Pulls in Solana DeFi
*Alhaidari, Kalal, Palanisamy, Sural — ACM CODASPY 2025*

Rug pulls in Solana have caused significant damage to DeFi users. We introduce SolRPDS, the first public rug pull dataset from Solana transactions. Examining ~4 years of DeFi data (2021-2024) covering 3.69 billion transactions, the dataset consists of 62,895 suspicious liquidity pools annotated for inactivity states (a key indicator). 22,195 tokens exhibit rug pull patterns. Preliminary analysis reveals clear distinctions between legitimate and fraudulent pools.

**What we leverage:** Inactivity states as a rug indicator → our creator ledger tracks last-interaction time per creator's tokens. Liquidity-add/remove timing → our on-chain confirm path already reads reserve balances.

### arXiv:2603.24625 — From Hype to Collapse: Investigating Rug Pull Scams on Solana
*Chen, Li, Jiang, He, Zhou, Wu, Zheng — 2026*

Large-scale measurement study of Rug Pulls on Solana. We manually verify 68 incidents and curate 117 confirmed Rug Pull tokens, distilling three on-chain behavioral patterns: **Freeze Authority Abuse, Liquidity Withdrawal, and Pump-and-Dump**. We apply our pipeline to 100,063 tokens on Orca, Raydium, and Meteora (H1 2025), identifying 76,469 Rug Pull tokens. Analysis shows Rug Pulls exhibit extremely short lifecycles, strong price-driven dynamics, severe economic losses, and **highly organized group behaviors**.

**What we leverage:** The three rug patterns → our `RugType` enum (G6). "Organized group behaviors" → our wallet-graph clustering (Phase 6) to identify rug families. "Extremely short lifecycles" → our hold-monitoring must be sub-minute.

### arXiv:2512.11850 — The Memecoin Phenomenon on Solana
*Mancino — IEEE ISCC 2025*

Analyzes the memecoin phenomenon on Solana, focusing on pump.fun during Q4 2024. pump.fun accounted for up to **71.1% of all tokens minted** on Solana and contributed **40-67.4% of total DEX transactions**. Fewer than **2% of tokens successfully transitioned** to major DEXs. Daily active users rose from 60,000 to peaks of 260,000. Memecoins democratize token creation but introduce significant risks to market efficiency and stability.

**What we leverage:** The extreme noise ratio (71.1% of mints, <2% graduate) → our tiering system (T0-T4) ensures we spend evaluation budget on high-conviction wallets, not noise. The <2% graduation rate → our migration-detection exit logic is critical (it's the signal that separates the 2% from the 98%).

### arXiv:2606.08232 — Hour-Aware Adaptive Risk Management for Autonomous Memecoin Trading
*Kamat — q-fin.TR, 2026 (our companion paper)*

15-day paper-traded autonomous memecoin trading deployment on Solana DEXs. 190-trade sample shows 40.5% win rate, mean per-trade return +0.62%, cumulative +117.7%. **Removing the top 3 trades (1.6% of sample) flips cumulative return unprofitable.** Parallel counterfactual rejection-tracker: 56.25% of rejected events hit -50% drawdown. Connects to Kyle (1985) informed-flow, Precup-Sutton-Singh (2000) off-policy evaluation, Bailey-Lopez de Prado (2014) deflated-Sharpe.

**What we leverage:** Fat-tail confirmation → trust-boosted entries into Proven-creator coins increase tail-event exposure (by design). Rejection-tracker validates gate → our post-exit tape records rejections, not just trades. The 1.6% tail-dependency → per-wallet PnL attribution identifies which whales lead us into tail events.

### arXiv:2505.09313 — Detecting Sybil Addresses in Blockchain Airdrops
*Liu, Huang, Fan, Wu, Tang — IEEE ICBC 2025*

Novel sybil address identification using subgraph feature extraction + lightGBM. Constructs **two-layer deep transaction subgraph** for each address, extracts temporal features (first transaction, first gas acquisition, participation timing, last transaction). Also extracts amount and network structure features via feature propagation and fusion. Tested on 193,701 addresses (23,240 sybil), all metrics exceed 0.9. Methods transferable to transaction manipulation identification and token liquidity risk assessment.

**What we leverage:** Two-layer transaction subgraph → our wallet-graph construction (Phase 6) uses subgraph-based edge detection. Temporal features (first tx, gas acquisition, last tx) → our `TrackedWalletInfo` struct carries these temporal markers. Feature propagation → our UnionFind clustering propagates Toxic classification across connected wallets.

### arXiv:2509.01168 — Rug Pull Detection on TON Blockchain
*Yaremus, Li, Kalacheva, Vodolazov, Yanovich — 2025*

ML framework for early rug pull detection on TON DEXs. Two rug definitions: TVL-based (catastrophic liquidity withdrawal) and idle-based (sudden cessation of trading). Gradient Boosting models identify rug pulls within **first 5 minutes** of trading, AUC up to 0.891. Feature distributions differ significantly across exchanges → platform-aware models needed.

**What we leverage:** 5-minute early-warning window → our pre-entry screen computes a 2-minute rug-score (adjusted for Solana's faster finality). TVL-based vs idle-based definitions → our on-chain confirm path reads both reserve balances AND trade activity. Platform-aware models → our calibration is pump.fun-specific, not ported from Ethereum/TON.

---

## Appendix B: Industry Source Details

### DeFade.org — Dev Wallet Analysis Guide
Key red flags for dev wallets:
- **10+ tokens created this week** → "spray and pray" rug operation
- **Dev wallet still holds significant % of supply** → waiting to dump
- **Dev connected to known rugger wallets** → funding trail reveals operator
- **Wallet age 10 minutes, funded from mixer** → highly suspicious
- **Dev consistently sells within first 30 minutes** → pump-and-dump assembly line

What to check in dev wallet history:
1. Number of tokens created (50+/month = volume scam)
2. Outcome of previous tokens (all died = serial rugger)
3. Dev sell behavior on past tokens
4. Wallet age and SOL source

### MadeOnSol — Smart Money Identification
4 properties of real smart money:
1. **Sufficient sample size** — 20+ tokens minimum before win rate is signal
2. **Consistency across time** — positive performance across 7d, 30d, all-time
3. **Positive net PnL** — not just win rate (win rate and PnL can diverge sharply)
4. **Low bot confidence** — filter out MEV/sniping bots (high bot_confidence = automated, not judgment)

### MadeOnSol — Whale Tracking
4 whale types:
- **Volume Whales** — large trade sizes, not always profitable
- **Profit Whales** — consistent high returns regardless of size
- **Smart Money** — consistently buy early before pumps
- **Insider Wallets** — connected to project teams, deployers

Tracking methods: leaderboards (Birdeye, DexScreener, Photon), backtrack from winning tokens, holder analysis. **Quality over quantity: 3-10 focused wallets > 50 random.**

### PANTEREX — Whale Wallet Finding
Wallet worth tracking checklist:
- 30+ trades of history
- Win rate that holds up over time
- Sensible position sizes
- Recent activity
- Clean funding (no airdrop/team tokens)

Red flags: single moonshot flattering the average, suspiciously perfect timing (insider/wash), constant free token receipts (team/airdrop).

### NoesisAPI — Bundle Detection
Holder classification:
1. **Bundlers** — fresh wallets, GMGN bundle detector, little prior activity
2. **Snipers** — first-slot bots with profit, millisecond timing
3. **Rat Traders** — early buyers who dumped within short window
4. **Whales/Traders/Organic** — everyone else by position size and activity

Risk rating: <10% bundler = green, 10-25% = yellow, >25% = red.

### RugRade.fun — Real-Time Pump.fun Radar
42 rug heuristic patterns:
- Bundle patterns, sniper buys, dev-wallet concentration, fake-volume detection
- Sub-800ms scoring per mint
- Telegram + X push alerts
- 12,847 tokens tracked in real-time

---

*End of proposal. Awaiting approval.*