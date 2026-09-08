# North Star — Stage 2: Aggregation Inventory (verified)

Status: **numerical foundation inventoried at full scale; permissively-licensed core
located.** Date: 2026-09-07. All counts measured via DuckDB footer scans, not estimates.

Purpose: the source-registry ledger for the D-categories. This is what Astra reviews to
see the actual data we have to build the trading brain from.

---

## Crown jewel: `slinky21_data` (permissively licensed)

Location: `D:/mev_bot-artifacts/rust-data/slinky21_data/`
License: **CLEARED** — operator contacted slinky; public hosted dataset, use authorized.
(README's MIT-vs-CC-BY inconsistency is superseded by direct publisher authorization.)
Temporal span: **2026-06-05 → 2026-07-14 (trades/tokens), snapshots to 07-16.**

| Table | Rows | Cols | Maps to |
|---|---|---|---|
| `tokens.parquet` | 798,430 | 44 | **D02** identity + creator risk (creator, `creator_past_rugs`, `dev_buy_pct`, `initial_top1_pct`) |
| `trades/trades-*.parquet` (18 shards) | **33,581,765** | 15 | **D03** raw trades + **D05** flow + **D06** wallets + **D12** outcomes |
| `snapshots.parquet` | 26,934,864 | 29 | **D05** chart/flow (bucket OHLCV, buy/sell volume, net_flow, buy_pressure) |
| `postgard_snapshots.parquet` | 1,392,133 | 27 | **D04** exit capacity + **D12** post-migration (dex_id, liquidity, price_change_*) |
| `wallet_stats.parquet` | 1,016,374 | 8 | **D06** wallet behavior (first/last_seen, tokens_traded, buy/sell volume) |

**~64.5M rows** of permissively-licensed numerical data. Trades are naturally balanced:
`is_buy` = 17,597,586 buy / 15,984,179 sell (52.4% / 47.6%) — no synthetic balancing needed at
the raw level.

Trade schema (the D03 core): `id, mint, tx_signature, event_time, seconds_since_launch,
is_buy, sol_amount, token_amount, user_wallet, v_tokens_bonding_curve, v_sol_bonding_curve,
market_cap_sol, price_sol, curve_pct_depleted, source`.

## Our own trades: `full_trades.pkl` (→ REDUNDANT, subsumed by slinky21)

Location: `D:/mev_bot-artifacts/rust-data/full_trades.pkl` — 4,636,777,631 bytes (4.6 GB).
Deserialized (isolated process) → `full_trades.parquet`: **27,457,292 rows × 10 cols**
(`mint, seconds_since_launch, is_buy, sol_amount, market_cap_sol, price_sol,
v_sol_bonding_curve, curve_pct_depleted, token_amount, user_wallet`).

**Finding:** schema is a strict subset of slinky21 trades (missing `event_time`,
`tx_signature`, `v_tokens_bonding_curve`, `source`, `id`). Distinct-mint check:
602,038 mints, **all 602,038 present in slinky21** (which has 622,870). So `full_trades.pkl`
is a **legacy reduced copy** of the slinky21 capture — adds zero new data and lacks
absolute timestamps/on-chain tx identity. It is **deprioritized**: slinky21 is the single
authoritative numerical source. (Kept as an untouched legacy copy; never deleted.)

## LaserStream raw capture (canonical source truth)

Location: `D:/mev_bot-artifacts/raw/` — **70 GB, 369 `.ndjson.zst` + 1 `.ndjson`**.
Schema (decoded, verified): Geyser stream records with `record_type ∈ {account,
transaction}`, `slot`, `recv_unix_ms`, `record_index`. Accounts carry `pubkey_b58`,
`lamports`, `owner_b58`, `executable`, `data_b64`, `write_version`, `txn_signature_b58`.
Transactions carry the full `message` (account_keys, instructions, versioned), `meta`
(`fee`, `compute_units_consumed`, `err`, **pre/post_balances**, **pre/post_token_balances**,
`inner_instructions`, `log_messages`), `signature_b58`.

Maps to **D01** (source truth) + **D03** (exact execution) + **D12** (exact fills via
balance deltas — no modeling). This is the **canonical verification substrate** the derived
`slinky21_data` can be checked against. Note: this capture window is **2026-08-23 → 08-24**,
a *separate/later* window than slinky21's 06-05 → 07-14 — complementary, not the same period.

## Narrative (thin — the remaining gap)

`narrative_gold_v1.1`: 1,471 content, 1,471 claims, 1,183 states, 615 validations,
**97 strategy cards**, 490 trajectories. Per master spec §4: only **33 EX_ANTE**, 1,424
AMBIGUOUS, **zero GOLD strategy cards**. Maps to **D07/D08/D09** — this is where capture
must continue (the 270 fresh events from this session's crawl begin to close it).

### Narrative gold rebuild (this session)

Rebuilt the full 5-layer chain from raw (14,080 events / 23 files) through
`content → claim → state → validation → strategy_card`:

| Layer | Count | Note |
|---|---|---|
| creator_content | 1,346 | from 14,080 raw (8,213 rejected, 4,521 dedup) |
| creator_claim | 1,060 | **EX_ANTE 33 → 253**, EX_POST 66, AMBIGUOUS 741 |
| narrative_state | 1,060 | 395 unique mints |
| narrative_validation | 363 | ⚠️ **0/317 mint overlap** (see break below) |
| strategy_card | 65 | **GOLD 0 → 23**, 12 setup types |

**Break (fixed):** the validation builder's slinky lookup was split by the artifact-store move
— resolved by consolidating `pump_outcome_v3` to the store and repointing `SLINKY_PATH`
(2,717 → **622,870 mints loadable**).

**Deeper finding — temporal mismatch:** even with the full 622,870 slinky mints accessible,
narrative↔slinky overlap is **1/317**. The narrative mints are *current* tokens (this week's
crawls); the slinky capture is a *past* window (Jun–Jul). Claim↔outcome validation therefore
cannot match across these corpora. Implication for the dataset: **narrative and on-chain
capture must be co-temporal** — for each narrative claim, capture the on-chain outcome of the
*same token over the same window* — or BUY/SELL validation stays unmatched. This shapes the
Stage 4 labeling design (episodes must span both timelines).

### Social/narrative lanes (validated this session)

| Lane | Status | Result |
|---|---|---|
| Telegram | ● live, free | 8 public channels, 2h cron (`be9ab5a0ed96`) |
| Reddit | ● live, free | anonymous `.rss` feed works (r/solana pull returned live threads); throttled ~1 req/min/IP |
| Wayback CDX (X archive) | ◐ historical | 14 handles, ~150 public snapshots (blknoiz06=37, notthreadguy=22) |
| On-chain (LaserStream + Helius/DexScreener/Birdeye) | ● live, free | the real memecoin signal feed |
| X/Twitter live text | ✗ no legit free route | login-walled; only paid API/licensed reseller |

Reddit is the live narrative lane (free, no account, no wall). Relevant subs: r/solana
(general), r/memecoins, r/SolanaMemeCoins, r/pumpfun (memecoin-specific — higher signal).
Parser integration into narrative_gold is the next step; sustained pull benefits from the
free Reddit "script" app OAuth (100 req/min vs 1) — a 1-min user registration, not a login.

### Elite-wallet behavioral substitute (D06/D11 via on-chain, not X)

The X/Twitter narrative lane is login-walled (13+ handles mapped, unscrapable per policy).
Substitute: the seed registry maps **34 elite wallets** (Orangie/Cented/Cupsey/Megga/Potion/
Ansem), and **12 are active in slinky21** (~30K trades, `elite_wallet_tracker.py` →
`elite_wallet_profiles.json`). Profiles are revealing — e.g. Cented 79% buy (accumulating),
Cupsey ~80% buy across 4 wallets, **Cented's dev wallet 12% buy / 88% sell (dumping)**.
This is higher-authority than their X posts (on-chain truth > creator claims) and directly
feeds D06 (wallet behavior) + D11 (decision-shaped events). 22/34 wallets inactive in this
window — either dormant, wrong address, or outside the 39-day span.

---

## D-category coverage status (post-inventory)

| Cat | Status | Backing |
|---|---|---|
| D01 source truth | ● strong | laserstream raw + slinky21 manifests |
| D02 identity | ● strong | `tokens.parquet` (798K, creator risk fields) |
| D03 raw trades | ●● strong | 33.5M slinky21 trades + `full_trades.pkl` |
| D04 capacity | ● strong | `postgard_snapshots` (liquidity/dex) + snapshots; size-specific depth still to derive |
| D05 chart/flow | ●● strong | 27M snapshots + 33.5M trades |
| D06 holders/wallets | ● strong | `wallet_stats` (1M) + snapshots; connected-wallet *linking* still to build |
| D07 narrative | ◐ thin | 1.5K claims, mostly ambiguous |
| D08 propagation | ◐ thin | repost/echo graph not yet built |
| D09 meta/rotation | ◐ thin | 1,183 states, no rotation model |
| D10 account state | ● sourced | `rust/data/tape.jsonl` (520 rec, ~260 trades) + `live_status.json`/`held_coins.json`/`cumulative_pnl.json` — hot wallet `7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC` |
| D11 decisions | ○ gap | derive from on-chain counterfactual economics (L3) — no human history |
| D12 outcomes | ● strong | trades + postgard + full_trades |
| D13 protocol/numeracy | ● sourced | `STAGE2_D13_PROTOCOL_NUMERACY.md` — curve math, 1% fee, graduation 85.005 SOL, program IDs, error codes (from Rust source) |
| D14 rust | — dropped | frontier model (Astra) |

## Immediate next actions (aggregation continues)

1. ~~Resolve slinky21 license~~ — **RESOLVED** (operator authorization from slinky).
2. **Decode LaserStream raw** zst → validate against its two manifests (D01–D05 lineage).
3. **Continue narrative capture** to lift D07/D08/D09 (strategy cards → GOLD, EX_ANTE rate up).
4. **Build connected-wallet linking** (D06) and **size-specific depth** (D04) from the trades + snapshots.
