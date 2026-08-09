# Dev Wallet Tracking & Creator Trust Assessment — Citadel Quant Report

**Date:** 2026-08-08
**Author:** Principal Citadel Pump.fun Memecoin Quant
**Status:** Informational — areas of opportunity for end-to-end feature build
**Constitution refs:** §27 (creator taxonomy), §28 (smart money), §29.9 (creator ledger), §26 (dump veto), §6.4 (unknown-stays-unknown)

---

## Executive Summary

The codebase has **substantial dev wallet infrastructure** — a full creator-ledger, deployer-credibility feature bundle, 7-archetype classifier, smart-money authentication gates, and a wallet-graph clustering framework. But three critical gaps prevent it from delivering real trust-score value:

1. **No persistence** — the entire creator ledger is in-memory and lost on every daemon restart.
2. **The rich 7-archetype classifier is starved** — the engine feeds it mostly zeros, so it can only ever produce `SerialRug`, `VolumeFarmer`, or `Unknown`. `CommunityBuilder`, `StreamerMeta`, `ShortLivedRunner`, and `Copycat` are **structurally unreachable** in live trading.
3. **No positive creator signal** — the engine only penalizes bad creators (veto, haircut). There is no size boost, threshold relaxation, or priority queueing for proven good devs. Trust is used exclusively as a risk gate, never as a conviction amplifier.

This report maps what exists, what's wired, what's broken, and the concrete opportunities to build a full end-to-end dev-wallet trust system.

---

## Part 1: What Exists Today (Infrastructure Inventory)

### 1.1 `pump-quant-wallet-graph` Crate

A dedicated crate with five modules:

| Module | Purpose | Status |
|---|---|---|
| `creator_ledger.rs` | Per-creator launch/migration/rug history with point-in-time discipline | **Wired** to engine (3 feed sites) |
| `deployer_credibility.rs` | Point-in-time deployer features: prior-CA count, serial-deploy burst, verified vs self-claimed partnerships, social reach | **Partially wired** (engine calls `compute_deployer_credibility` but passes empty partnerships + empty social reach) |
| `creator_classifier.rs` | 7-archetype deterministic classifier over measured creator inputs | **Starved** — engine feeds it zeros for 6 of 9 fields |
| `smart_money.rs` | Smart-money authentication: PnL truth rules, skill-vs-luck stats, lagged-shadow follower PnL, copy-bait detection | **Not wired** — pure library, no live data feed |
| `tier2_wallet_graph.rs` | Wallet clustering: UnionFind, discovery-time-stamped edges, family grouping, ML fold integrity | **Not wired** — pure library, no on-chain edge construction |

### 1.2 CreatorLedger (the core tracker)

**Data model:**
- Keyed on `WalletId(creator_pubkey_hash)` — a u64 FNV-1a hash of the creator's pubkey
- Per-creator entry stores: launch events (slot-stamped), migration events, rug events
- 4096-entry cap, evicts lexicographically-smallest key (NOT LRU — this is a weakness)
- Classification cascade (§29.9):
  1. **Toxic** — ≥ `min_rugs_for_toxic` rugs observed
  2. **Serial** — ≥ `serial_min_launches` launches inside the serial window
  3. **Proven** — ≥ `min_survived_for_proven` survived launches, zero rugs, untruncated history
  4. **Unknown** — everything else (§6.4: absent history is absent, not "clean")

**What it tracks per launch:**
- Launch slot (when the mint was created)
- Migration slot (when the bonding curve completed — a survival signal)
- Rug slot (when the creator was confirmed to have dumped past veto threshold)
- Survival horizon: a launch "survived" if it reached migration and hasn't rugged within `survival_horizon_slots`

### 1.3 DeployerCredibility (feature bundle)

Produces **distinct components** (NOT an opaque score, per §27/§28):
- `prior_ca_count` — number of prior launches strictly before decision slot
- `serial_deploy_flag` — whether launches cluster into a rapid burst
- `max_launches_in_window` — the burst occupancy count
- `key_follower_reach` — verified "key" follower count (anti-fabrication)
- `mutual_follower_reach` — shared followers with trusted reference set
- `verified_partnership_count` vs `self_claimed_partnership_count`

### 1.4 CreatorClassifier (7 archetypes)

The full taxonomy (§27), priority cascade:

| Archetype | Trigger | Extractive? |
|---|---|---|
| `SerialRug` | ≥2 resolved rugs, ≥50% rugged ratio | Yes — most extractive |
| `VolumeFarmer` | ≥3 launches in serial window + ≥40% dump intensity | Yes |
| `Copycat` | ≥70% metadata similarity to existing token | Neutral (per-launch tell) |
| `ShortLivedRunner` | ≥2 prior launches, median survival < 1hr | Negative |
| `StreamerMeta` | ≥60% launches driven by livestream meta | Context-dependent |
| `CommunityBuilder` | ≥2 prior launches, ≥60% retention, ≥24hr survival, ≤20% dump | **Positive** |
| `Unknown` | Thin evidence (§6.4) | Neutral |

### 1.5 SmartMoney (smart-money authentication)

Two gates before any wallet may be treated as "smart" (§28):

1. **PnlScreen** — realized (never marked) PnL, executable proceeds, external-counterparty netted at operator-family level, self-dealing excluded, minimum-sample floor, top-trade-removed concentration screen.
2. **lagged_shadow** — the only admissible definition of smart money: simulate entering at THIS system's observation + decision + execution latency after the wallet acted, exiting under THIS system's own policy, at THIS system's size, with full costs. Compare against activity-matched control cohort.

Combined classifier produces `WalletQualityState` including copy-bait / legibility screens.

### 1.6 Tier2WalletGraph (clustering infrastructure)

- `UnionFind` — deterministic connected components
- `WalletGraph` — discovery-time-stamped edges with typed provenance:
  - `SameCreator`, `SameDeployer`, `Funding`, `SameFundingRoot`, `SameFeePayer`, `SameTipPayer`, `SameBundle`, `CoBuySameBlock`, `CoBuyFirstN`, `SellSync`, `MetadataReuse`, `SocialAmplification`
- `families_as_of()` — point-in-time family grouping (future edges invisible to past decisions, §6.5)
- `FamilyHoldout` — ML fold integrity (no family straddles train/test boundary, §53)
- `build_activity_matched_placebo` — control cohort construction (§46)

---

## Part 2: How It's Wired Into the Engine

### 2.1 Feed Sites (3 sites in engine.rs)

| Event | Engine Method | Ledger Call | Line |
|---|---|---|---|
| Mint creation detected | `observe_creation()` | `record_launch(creator, token, slot)` | 1962 |
| Migration detected | (migration handler) | `record_migration(creator, token, slot)` | 1829 |
| Creator dump confirmed | `observe_creator_action()` | `record_rug(creator, token, slot)` | 2052 |

**Data source:** All three are fed from the on-chain swap/metadata stream that the daemon decodes from Helius WS + LaserStream gRPC. No external API calls — purely from the bot's own observed event stream.

### 2.2 Gate Influence (how creator data affects trade decisions)

**Path 1 — Hard veto (§26):** `creator_dump_active()`
- If creator has sold ≥ veto fraction of peak holdings → force exit at current mark
- Known extractors (SerialRug/VolumeFarmer) get a **stricter** (lower) veto threshold
- This is a hard binary — it can force-close a held position

**Path 2 — Size haircut (§70.9):** `deployer_screen_mult_bp()`
- Returns a reduce-only multiplier (bps) based on deployer credibility
- Identity (10_000 = 1.0×) when deployer is unknown
- Haircut applied to position size — penalizes risky deployers
- `deployer_screen_haircut_bp()` class-conditions the haircut on known-extractor status

**Path 3 — Brain fingerprint:** `creator_class` field in `SetupInputs`
- Collapsed to 4-value brain enum: `Unknown` (0), `Proven` (1), `Toxic` (2), `Serial` (3)
- Used in the `SetupFingerprint` → episode recall → archetype matching
- The Sniper lens (archetype.rs:46) notes: "creator class carries unusual weight — it is nearly the only prior that exists at second thirty" for new-mint entries
- BUT: the fingerprint is a **categorical one-hot**, not a weighted prior — it affects which historical episodes get recalled, not a direct size/threshold adjustment

### 2.3 What the Engine Does NOT Do With Creator Data

- ❌ No positive size boost for Proven/CommunityBuilder creators
- ❌ No threshold relaxation for known-good creators (e.g., wider mcap band, lower authenticity floor)
- ❌ No priority queueing of mints from tracked good devs
- ❌ No "watch list" of creators whose new launches get fast-tracked into the watchlist
- ❌ No cross-session persistence of any creator data
- ❌ No on-chain wallet graph construction (edges never populated)
- ❌ No smart-money PnL screening (no trade data fed to PnlScreen)
- ❌ No social reach data collection (SocialReachInput always default = zeros)

---

## Part 3: Critical Gaps (The "Why This Doesn't Work Yet" Analysis)

### GAP-A: No Persistence (CRITICAL)

**The entire `CreatorLedger` is in-memory.** When the daemon restarts — which happens on every code update, every watchdog health-kill, every Windows update — ALL creator history is lost. Every restart is a cold start where every creator is `Unknown`.

**Impact:** The `Proven` classification requires observing a creator's prior launches surviving to migration. With no persistence, the bot can only classify creators it has seen launch within the current session. On a fresh restart, even a prolific community builder looks `Unknown`.

**Evidence:** No serde derives on `CreatorLedger`, no save/load methods, no disk I/O. The daemon saves "brain snapshots" but not creator ledger state.

### GAP-B: Classifier Starvation (CRITICAL)

The engine's `creator_is_known_extractor()` method (engine.rs:5109) constructs `CreatorInputs` with:
- `resolved_launch_count: 0` — always zero
- `rugged_launch_count: 0` — always zero
- `median_survival_secs: 0` — always zero
- `community_retention_bps: 0` — always zero
- `streamer_launch_ratio_bps: 0` — always zero
- `copycat_similarity_bps: 0` — always zero

**Consequence:** The 7-archetype classifier can ONLY ever return:
- `SerialRug` — NO (needs resolved_launch_count ≥ 2)
- `VolumeFarmer` — YES (only needs window launches + dump intensity, which ARE populated)
- `Copycat` — NO (needs copycat_similarity_bps)
- `ShortLivedRunner` — NO (needs median_survival_secs + history)
- `StreamerMeta` — NO (needs streamer_launch_ratio_bps)
- `CommunityBuilder` — NO (needs community_retention + survival)
- `Unknown` — YES (fallback)

So in live trading, the classifier produces `VolumeFarmer` or `Unknown`. That's it. The `CommunityBuilder` archetype — the positive signal Alon wants — is **structurally unreachable**.

### GAP-C: Brain Enum Collapse (MODERATE)

The brain's `CreatorClass` enum has only 4 values: `Unknown`, `Proven`, `Toxic`, `Serial`. The library's 7-archetype taxonomy is collapsed:
- `SerialRug` + `VolumeFarmer` → `Toxic` (or `Serial` depending on path)
- `ShortLivedRunner` → `Unknown` or `Serial`
- `CommunityBuilder` → `Proven` (if it were ever reachable, which it isn't)
- `StreamerMeta` → `Unknown`
- `Copycat` → `Unknown`

The brain can't distinguish a `CommunityBuilder` from a `StreamerMeta` from a first-time `Unknown` — they all collapse to `Unknown` or `Proven`. The archetype-matching recall system loses discrimination.

### GAP-D: No Positive Creator Signal (MODERATE)

The engine's creator system is **purely punitive**:
- Toxic → veto exit
- Serial → size haircut
- Unknown → identity (no effect)
- Proven → ??? (nothing — there's no boost path)

There is no code path where a `Proven` or `CommunityBuilder` classification results in:
- A size multiplier > 1.0
- A relaxed gate threshold
- Priority admission into the watchlist
- A wider mcap band
- A lower authenticity floor

The Sniper lens comment says creator class "carries unusual weight" at second 30 of a new mint — but that weight is only realized through episode recall (the fingerprint matches historical episodes with similar creator classes). If no episodes exist (cold start, no persistence), the creator class has zero effective impact.

### GAP-E: No On-Chain Wallet Graph Construction (MODERATE)

`tier2_wallet_graph.rs` defines the full clustering infrastructure — UnionFind, typed edges, family grouping — but **no code ever calls `add_edge()`**. The wallet graph is never populated. The infrastructure for detecting multi-wallet creator families (same deployer using different wallets, funding clusters, coordinated amplification) exists but is dormant.

**Impact:** A serial rugger using 5 different wallets to launch 5 different tokens would be seen as 5 separate `Unknown` creators, not one `Serial` family. The clustering that would catch this is built but never fed.

### GAP-F: No Smart Money Data Feed (MODERATE)

`smart_money.rs` has the full PnL screen and lagged-shadow logic, but:
- No code feeds actual wallet trades into `PnlScreen`
- No code constructs the `Trade` structs from on-chain data
- No code runs `lagged_shadow` on observed wallet activity
- No code calls `classify_smart_money()`

The smart-money authentication system — which could identify which wallets consistently profit and use them as conviction signals — is pure library code with zero live wiring.

### GAP-G: Eviction Policy is Lexicographic, Not LRU (MINOR)

The `CreatorLedger` evicts the lexicographically-smallest `WalletId` when full (4096 entries). This is NOT LRU — a prolific, long-tracked creator could be evicted if their hash happens to be small. An LRU or frequency-weighted eviction would preserve high-value tracked creators.

---

## Part 4: Areas of Opportunity (Build Plan)

### OPP-1: Creator Ledger Persistence (HIGH IMPACT, LOW RISK)

**What:** Serialize `CreatorLedger` to disk on a periodic cadence (every N ticks or on graceful shutdown) and reload on startup.

**Why:** Without persistence, every other improvement is degraded — the bot forgets every creator on restart. This is the foundation.

**How:**
- Add `serde::Serialize/Deserialize` to `CreatorLedger` and its inner types
- Daemon saves `data/creator_ledger.bin` (bincode) on shutdown + periodic checkpoint
- Daemon loads on startup before entering the event loop
- Watchdog health check verifies ledger loaded (entry count > 0 on warm start)

**Risk:** Low — additive, no change to decision path. The ledger is already a pure data structure.

### OPP-2: Feed the Full Classifier (HIGH IMPACT, MODERATE RISK)

**What:** Populate the 6 zeroed fields in `creator_is_known_extractor()` with data the engine already has or can derive.

**Why:** Unlock `CommunityBuilder`, `ShortLivedRunner`, `StreamerMeta`, `Copycat`, and proper `SerialRug` classification. The classifier is built and tested — it just needs real inputs.

**Fields to populate:**
- `resolved_launch_count` — count from the ledger how many of this creator's launches have a terminal state (migrated OR rugged)
- `rugged_launch_count` — already tracked in the ledger
- `median_survival_secs` — compute from ledger entries (migration_slot - launch_slot for surviving launches, or rug_slot - launch_slot for rugged ones)
- `community_retention_bps` — derive from holder-flow data (the engine already tracks `holder_flow` per mint; retention = holders still holding at migration vs initial holders)
- `streamer_launch_ratio_bps` — requires a new input stream (pump.fun livestream API or social feed). Can default to 0 until available.
- `copycat_similarity_bps` — requires metadata similarity scoring against existing tokens. The engine has `mint_category` data; a metadata-hash comparison could produce this.

**Phasing:** Start with the 4 ledger-derivable fields (resolved, rugged, survival, retention). Add copycat similarity and streamer ratio in later phases.

### OPP-3: Positive Creator Boost (HIGH IMPACT, MODERATE RISK)

**What:** Add a `deployer_screen_boost_bp()` that returns a multiplier > 1.0 for `Proven`/`CommunityBuilder` creators.

**Why:** The current system only penalizes. A proven community builder should get a size boost (or relaxed threshold) — the upside probability is empirically higher. This turns creator tracking from a pure risk gate into a conviction signal.

**Design:**
- `CommunityBuilder` → 1.15× size multiplier (or +150 bps to the size cap)
- `Proven` (survived ≥ threshold migrations, zero rugs) → 1.08× multiplier
- Unknown → 1.0× (identity, current behavior)
- Serial/Toxic → existing haircut/veto (unchanged)
- Cap the boost at 1.20× to prevent runaway position sizing
- Constitution check: this must be reduce-only in the aggregate (the boost can't exceed what the gate would have allowed without it — it shifts allocation, doesn't increase total risk)

### OPP-4: Brain Enum Expansion (MODERATE IMPACT, LOW RISK)

**What:** Expand the brain's `CreatorClass` from 4 values to 7, matching the library's full archetype taxonomy.

**Why:** The episode recall system loses discrimination when `CommunityBuilder`, `StreamerMeta`, `ShortLivedRunner`, and `Copycat` all collapse to `Unknown`. A `CommunityBuilder` episode and a `StreamerMeta` episode have different forward distributions — the brain should be able to distinguish them.

**How:** Add the missing variants to `brain::fingerprint::CreatorClass`, update `ordinal()` and the one-hot encoding, and remap `brain_creator_class()` to pass through the full taxonomy instead of collapsing it.

**Risk:** Low — additive enum variants. Existing episodes recorded with the old 4-value enum stay valid (ordinal 0-3 preserved, new variants get ordinals 4-6).

### OPP-5: Dev Wallet Watchlist + Priority Admission (MODERATE IMPACT, LOW RISK)

**What:** Maintain a persistent set of "tracked good dev" wallet addresses. When a new mint from a tracked dev appears, fast-track it into the watchlist (skip the normal discovery queue latency).

**Why:** Currently every mint enters the watchlist through the same discovery pipeline. A dev with a proven track record should get lower latency to first evaluation — the bot should be ready to act faster on their launches.

**How:**
- Persistent file: `data/tracked_devs.json` — array of `{pubkey, trust_score, last_updated}`
- Populated from the creator ledger: any creator with `Proven` or `CommunityBuilder` classification gets added
- On mint creation detection, check if creator is in tracked_devs → if yes, immediately create a discovery candidate (already partially done via `first_sighting`, but could add a `priority` flag that affects evaluation ordering)
- Trust score decays over time (a dev who hasn't launched in 30 days has a stale score)

### OPP-6: On-Chain Wallet Graph Construction (HIGH IMPACT, HIGH RISK)

**What:** Populate `tier2_wallet_graph.rs` edges from on-chain data — detect funding links, shared fee payers, co-buy clusters, and metadata reuse.

**Why:** A serial rugger using N wallets appears as N separate `Unknown` creators. Clustering would identify the family and propagate the `Toxic`/`Serial` classification across all member wallets.

**Data sources needed:**
- `getSignaturesForAddress` / transaction parsing — detect shared fee payers, funding sources
- LaserStream transaction data — detect co-buy patterns, sell synchronization
- Metadata comparison — detect reused token names, symbols, images (copycat detection)

**Risk:** High — requires new data ingestion, careful point-in-time discipline (§6.5), and the wallet graph must never use edges discovered after the decision time. But the infrastructure (UnionFind, typed edges, family grouping) is already built and tested.

### OPP-7: Smart Money PnL Screening (HIGH IMPACT, HIGH RISK)

**What:** Feed observed wallet trades into `PnlScreen` and run `lagged_shadow` to identify genuinely smart wallets — then use their activity as conviction signals.

**Why:** The smart-money authentication system (§28) is the most rigorous definition of "profitable wallet" in the codebase. It excludes self-dealing, requires executable proceeds, and demands follower-executable PnL. A wallet that passes these gates is genuinely skilled, not just lucky.

**How:**
- Parse all swaps from the LaserStream event stream
- Attribute trades to wallet families (using the wallet graph from OPP-6)
- Feed `Trade` structs into `PnlScreen` with family-netted realized PnL
- Run `lagged_shadow` — simulate entering after the wallet's observed action at our system's latency, exit under our policy, compare to control cohort
- Wallets that pass both gates → `WalletQualityState::Smart` → their current holdings become conviction signals (they're buying this token = positive signal)

**Risk:** High — significant data pipeline work, and the lagged-shadow simulation must use the exact same latency/exit policy as the live system to be admissible. But the library code is complete and tested.

### OPP-8: Eviction Policy Upgrade (LOW IMPACT, LOW RISK)

**What:** Replace lexicographic eviction with frequency-weighted LRU in `CreatorLedger`.

**Why:** A prolific creator with a small hash could be evicted in favor of a one-off creator with a large hash. LRU (or a "launches count" weight) preserves high-value tracked creators.

**How:** Track last-access slot per entry, evict the oldest-accessed entry when full. Or weight by total launch count (prolific creators are harder to evict).

---

## Part 5: Recommended Build Priority

| Priority | Opportunity | Impact | Risk | Dependencies |
|---|---|---|---|---|
| **P0** | OPP-1: Ledger persistence | Foundation — everything else degrades without it | Low | None |
| **P1** | OPP-2: Feed the full classifier | Unlocks 5 of 7 archetypes, including CommunityBuilder | Moderate | Needs OPP-1 for survival data |
| **P2** | OPP-3: Positive creator boost | Turns creator tracking from risk gate into conviction signal | Moderate | Needs OPP-2 for CommunityBuilder classification |
| **P2** | OPP-4: Brain enum expansion | Improves episode recall discrimination | Low | Needs OPP-2 for the new values to be populated |
| **P3** | OPP-5: Dev wallet watchlist | Reduces latency to evaluation for proven devs | Low | Needs OPP-1 for persistent tracked_devs |
| **P4** | OPP-6: Wallet graph construction | Catches multi-wallet creator families | High | New data ingestion pipeline |
| **P4** | OPP-7: Smart money PnL screening | Identifies genuinely skilled wallets | High | Needs OPP-6 for family attribution |
| **P5** | OPP-8: Eviction policy upgrade | Prevents high-value creator eviction | Low | None |

---

## Part 6: Honest Limitations (§6.4 Discipline)

1. **Trust scores are not truth.** A `CommunityBuilder` classification is a measured prior, not a guarantee. Community builders can rug. The boost must be capped and reduce-only in aggregate.

2. **Point-in-time discipline is non-negotiable.** Every creator feature must be computed as-of the decision slot. A launch that hasn't happened yet is invisible. This is already enforced in the library code and must be preserved in any new wiring.

3. **Unknown stays unknown.** A first-time creator with no history is `Unknown`, not "probably good" or "probably bad." The system must never coerce absence into a benign label. This is constitutionally binding (§6.4).

4. **Social reach and partnership data are not currently collected.** The `deployer_credibility` module supports key-follower and mutual-follower inputs, but no data feed populates them. Adding social data would enrich the deployer credibility bundle but requires a social API integration (pump.fun social, Twitter/X, Telegram).

5. **Smart money is adversarial.** On-chain "profitability" is manufactured by default (wash trading, self-dealing, bait sequences). The §28 gates exist precisely to filter this. Any smart-money signal must pass through `PnlScreen` AND `lagged_shadow` — never just "this wallet has high realized PnL."

---

## Part 7: What This Would Look Like End-to-End

**Today (broken):**
1. Daemon starts → creator ledger empty → every creator is `Unknown`
2. Mint detected → creator has no history → `Unknown` → no boost, no penalty
3. Creator dumps → `Toxic` → veto exit (if still holding)
4. Daemon restarts → ledger wiped → creator is `Unknown` again
5. Same creator launches again → still `Unknown` → no learning

**After full build:**
1. Daemon starts → loads persisted ledger → creator history preserved
2. Mint detected → creator classified from historical data → `CommunityBuilder` (if earned)
3. `CommunityBuilder` → size boost 1.15×, priority watchlist admission, wider mcap band
4. Creator dumps → `Toxic` → veto exit + ledger records rug
5. Daemon restarts → ledger reloaded → creator's rug count preserved
6. Same creator launches again → `SerialRug` (2 rugs in history) → size haircut or hard reject
7. Wallet graph detects creator using 3 wallets → family = `Toxic` → all 3 wallets inherit `Toxic`
8. Smart money PnL screen identifies genuinely profitable wallets → their buys become conviction signals

---

## Appendix: Key File Map

| File | Role | Lines |
|---|---|---|
| `crates/pump-quant-wallet-graph/src/creator_ledger.rs` | Creator launch/migration/rug ledger + classification | 598 |
| `crates/pump-quant-wallet-graph/src/deployer_credibility.rs` | Point-in-time deployer feature bundle | 147 |
| `crates/pump-quant-wallet-graph/src/creator_classifier.rs` | 7-archetype deterministic classifier | 490 |
| `crates/pump-quant-wallet-graph/src/smart_money.rs` | Smart-money PnL + lagged-shadow authentication | ~450 |
| `crates/pump-quant-wallet-graph/src/tier2_wallet_graph.rs` | Wallet clustering + family grouping infrastructure | 420 |
| `crates/pump-quant-app/src/engine.rs` | Engine — 3 feed sites, 2 gate paths, classifier call | 6080 |
| `crates/pump-quant-app/src/measured_state.rs` | MeasuredState wrapper — owns the CreatorLedger | ~700 |
| `crates/pump-quant-brain/src/fingerprint.rs` | Brain CreatorClass enum (4 values) + fingerprint | 1635 |
| `crates/pump-quant-brain/src/archetype.rs` | Style lenses — Sniper notes creator class weight | 1396 |
| `crates/pump-quant-junction/src/bin/pq_daemon.rs` | Daemon — brain snapshot save/load (no ledger save) | ~2050 |

---

*End of report. Ready for operator review.*