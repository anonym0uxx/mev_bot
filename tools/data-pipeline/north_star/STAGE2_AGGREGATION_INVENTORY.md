# North Star — Stage 2: Aggregation Inventory (verified)

Status: **numerical foundation inventoried at full scale; permissively-licensed core
located.** Date: 2026-09-07. All counts measured via DuckDB footer scans, not estimates.

Purpose: the source-registry ledger for the D-categories. This is what Astra reviews to
see the actual data we have to build the trading brain from.

---

## Crown jewel: `slinky21_data` (permissively licensed)

Location: `D:/mev_bot-artifacts/rust-data/slinky21_data/`
License: README declares MIT in YAML, CC BY 4.0 in prose — **conflict to resolve**
before admission (both permissive, but the actual grant/revision must be pinned).
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

## Our own trades: `full_trades.pkl`

Location: `D:/mev_bot-artifacts/rust-data/full_trades.pkl` — **4,636,777,631 bytes (4.6 GB)**.
Maps to **D03 + D12 + D10** (our actual fills + inventory). NOT yet deserialized — per spec
this requires an **isolated, no-credential, memory-budgeted process → Parquet streaming**.
This is our authoritative D10 (account state) source, which slinky21 lacks.

## LaserStream raw capture

Location: `D:/mev_bot-artifacts/raw/` — 24 × `.ndjson.zst` parts + 1 `.ndjson` events file
(from `tools/stream-capture-rs/grpc-server-only/training-data/`). Maps to **D01–D05** raw
capture lineage. Not yet decoded/validated against manifests.

## Narrative (thin — the remaining gap)

`narrative_gold_v1.1`: 1,471 content, 1,471 claims, 1,183 states, 615 validations,
**97 strategy cards**, 490 trajectories. Per master spec §4: only **33 EX_ANTE**, 1,424
AMBIGUOUS, **zero GOLD strategy cards**. Maps to **D07/D08/D09** — this is where capture
must continue (the 270 fresh events from this session's crawl begin to close it).

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
| D10 account state | ○ gap | `full_trades.pkl` (undeserialized) is the source |
| D11 decisions | ○ gap | derive from on-chain counterfactual economics (L3) — no human history |
| D12 outcomes | ● strong | trades + postgard + full_trades |
| D13 protocol/numeracy | ◐ gap | build from versioned source docs |
| D14 rust | — dropped | frontier model (Astra) |

## Immediate next actions (aggregation continues)

1. **Resolve slinky21 license** (MIT vs CC BY 4.0) against publisher evidence — blocks admission.
2. **Deserialize `full_trades.pkl`** in an isolated, memory-budgeted process → Parquet (D10 + our D12).
3. **Decode LaserStream raw** zst → validate against its two manifests.
4. **Continue narrative capture** to lift D07/D08/D09 (strategy cards → GOLD, EX_ANTE rate up).
5. **Build connected-wallet linking** (D06) and **size-specific depth** (D04) from the trades + snapshots.
