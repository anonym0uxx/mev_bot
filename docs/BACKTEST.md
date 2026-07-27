# BACKTESTING ON REAL DATA — source assessment, harness, and method (2026-07-27)

**The operator is right, and this is the most important open gap in the project: every
measurement made to date came from SYNTHETIC tapes.** The golden tape, the hazard tapes, the
two-sided A/B corpora — all of them are generators we authored. They are internally rigorous and
they have caught real defects, but not one of them is evidence about the actual market.

This document does three things: names which historical source is genuinely free and what each can
and cannot support; records precisely why the backtest could not be *executed* from the authoring
sandbox; and ships the harness so it runs the moment real per-swap data is in hand.

---

## 1. Source assessment — verified, not assumed

Every claim below was checked directly against the source, not inferred from the discussion.

| Source | Free? | What it actually has | Verdict |
|---|---|---|---|
| **Dune** (`pumpdotfun.trades`) | Free tier, account + API key | **Pre-decoded pump.fun trades**: side, amounts, reserves, trader | **BEST free option.** 50k rows/export, compose multiple queries |
| **Helius** (we hold Business) | Already paid for | Enhanced/decoded transactions, DAS metadata, LaserStream backfill | **BEST overall for us** — no new vendor, and it is the live ingest path |
| **Flipside Crypto** | Free tier, account | Decoded Solana tx over SQL | Viable; needs own pump.fun instruction decoding |
| **PumpAPI.io** replay | Account | Raw + decoded buys/sells/transfers since 18 Apr | Viable |
| **solarchive** (HuggingFace) | **Fully free, no key** | Parquet, `accounts`/`tokens`/`txs` | **NOT viable today.** `index.json` shows `txs` = **5 daily partitions**, `updated_at` 2025-12-19. Five days, not a year |
| **horenresearch/solana-pairs-history** (HF) | **Fully free, no key** | Per-pair JSONL OHLCV, includes pump.fun mints (they end `pump`) | **Wrong granularity — see §2** |
| **BigQuery** Solana public dataset | Scan costs money | Full chain | Rejected on cost |
| **jetstreamer** + Old Faithful | Free software, needs a big machine | Full chain replay | Viable on the server; heavy |

## 2. The free OHLCV set cannot backtest THIS strategy — and the reason matters

`horenresearch/solana-pairs-history` is real, free, and does contain pump.fun tokens. I pulled it
and confirmed the shape directly. Example, a real pump.fun mint (`23ENcg…J1pump`):

```
{"o":0.0000000343,"h":0.000018317,"l":0.0000000341,"c":0.0000053604,"v":7489283891.23,"t":1732856400}
{"o":0.0000053604,"h":0.0000285188,"l":0.0000030344,"c":0.0000146602,"v":2440682329.05,"t":1732860000}
```

`1732860000 − 1732856400 = 3600`. **These are HOURLY candles**, and that is disqualifying:

* Our bot's whole trade lifecycle — entry, ladder, exit — happens **inside a single hourly bar**.
  `lc_max_hold_ticks` is 300 ticks; the binding exit fires per swap.
* The binding exit is the **§32 order-flow sign flip**. Candles carry no `signed_base`, so the rule
  that actually ends every one of our positions is unrepresentable.
* No `liquidity_lamports` (no reserves), so the economic gate cannot price impact or exit cost.
* No `buyer_entity`, so entity-dedup, wash screening, holder accounting, and concentration are all
  blind.

A "backtest" on hourly candles would measure an **hourly swing strategy that we do not run**, and
reporting it as validation of this bot would be precisely the fabrication the constitution forbids.
It is rejected for that reason, not for lack of effort.

*(The dataset remains useful for one thing: measuring the real pump.fun outcome distribution to
check whether the golden tape's assumed mix is plausible. That is future work and is NOT a
strategy backtest — see §5.)*

## 3. Why it could not be executed from the authoring sandbox

Recorded so nobody repeats the attempt and concludes the data is unobtainable — it is obtainable,
just not from here.

* **No network egress.** `curl` to huggingface.co, datasets-server, api.dune.com, solscan,
  api.mainnet-beta.solana.com and frontend-api.pump.fun all return `000`. Only the package-registry
  route answers.
* **No package installs.** `pip download requests` → "No matching distribution". There is no
  `duckdb`, `pandas`, or `pyarrow`, so parquet could not be read even if it could be fetched.
* **`WebFetch` is LLM-summarized, and its aggregation is provably unreliable.** Verbatim line
  extraction is faithful — the candles above came through exactly. But asked for a computed
  statistic it returned `MAX_H = 0.000000292` for a file whose *first line* has `h = 0.000000317`.
  A maximum below a value present in the data is impossible, so **any statistic derived through it
  is inadmissible.** Verbatim extraction only.
* **The device bridge has no network either**, so the operator's machine is not a workaround from
  this surface.

**Conclusion: the backtest must run where the data is reachable — the deployment server, which
already holds Helius Business credentials. It is a Phase-B task, and the harness below is ready.**

## 4. The harness (shipped, tested end to end)

`tools/backtest/pump_replay_build.py` converts real decoded pump.fun swaps into the replay events
grammar the engine already reads. Pipeline, verified working against the release binary:

```
decoded swaps (JSONL)  →  pump_replay_build.py  →  events.txt  →  pump-quant-app replay
```

**Input** is the canonical pump.fun `TradeEvent` — `mint`, `user`, `isBuy`, `solAmount`,
`tokenAmount`, `virtualSolReserves`, `virtualTokenReserves`, `slot` — which is exactly what a
Helius decode, a PumpPortal stream, a PumpAPI replay, or a Dune `pumpdotfun.trades` export yields.
Field names are matched case- and style-insensitively, so most sources need no reshaping.

**It refuses rather than guesses.** A record missing reserves, side, amounts, or slot is **dropped
and counted**, never defaulted or interpolated (§6). The run prints a drop ledger and a coverage
percentage, so how much of the input actually informed the result is auditable rather than assumed.
Reserves are required precisely because they *are* the price and the depth — inventing either would
manufacture the very edge we are trying to measure.

**Market-cap gating** (`--min-mcap-sol` / `--max-mcap-sol`) is evaluated at **first sighting only**,
so selection carries no look-ahead. Defaults bracket the pre/just-post-graduation band the strategy
is calibrated for; replaying graduated large-caps would measure a different game.

Run:

```bash
python3 tools/backtest/pump_replay_build.py \
    --in swaps.jsonl --out events.txt \
    --min-mcap-sol 5 --max-mcap-sol 600 --max-mints 512

printf 'gate_expected_move_bps = 1800\ngate_protocol_bps = 450\ngate_margin_bps = 150\n' \
    'gate_base_fixed_lamports = 200000\ngate_impact_den = 250000\n' > bt.cfg

./target/release/pump-quant-app replay bt.cfg events.txt
```

## 5. Method — what makes this backtest honest

Binding on whoever runs it. Most of these are ways a memecoin backtest lies.

1. **Survivorship bias is the dominant risk — and it is now ENFORCED, not advised.** Any dataset
   assembled from *indexed pairs* is conditioned on the token having mattered enough to be indexed.
   Tokens that died in minutes — the majority, and our modal case — are systematically absent, and
   a backtest over the survivors will look profitable when the strategy is not.

   The converter therefore **REFUSES TO RUN** without `--universe-manifest`: the launch-time
   universe, i.e. every mint created in the window. Passing `--unaudited-survivorship` overrides it
   but stamps the events file `*** UNAUDITED ***`, and a net produced from an unaudited corpus is
   **not admissible evidence** under A-11.

   **How to build the launch universe (do this first, before pulling any trades).** Enumerate the
   pump.fun program's `create` instructions over the slot range — that set IS the universe, because
   every token that ever existed was created by one, including the ones that died in ninety seconds
   and were never indexed anywhere:

   * Program id `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`.
   * Page `getSignaturesForAddress` over the program by slot range (or use Helius enhanced
     transactions / LaserStream backfill, which we already pay for), keep transactions containing a
     `create`, and take the mint from the emitted `CreateEvent`.
   * Write one mint per line. That file is `--universe-manifest`.
   * **Never** build it from a list of pairs that exist today, from a DEX-screener export, or from
     "tokens with at least N trades" — each of those is the bias wearing a different hat.

   The tool then reports `corpus covers X/Y launched mints (Z%)`, warns loudly below 50%, and
   independently flags a **pre-filtered input**: if the minimum trades-per-mint is implausibly high,
   the corpus was filtered to active tokens somewhere upstream, which is the same bias entering
   before the tool ever sees the data. Both warnings are written into the events-file header so they
   travel with the artifact and cannot be lost between the run and the write-up.

   Finally, subsampling with `--max-mints` is **hash-ordered, not first-N**. Taking the first N
   after a slot sort — which this tool originally did — silently truncates to the earliest mints,
   i.e. a single market regime. That was a real defect, found and fixed in review.
2. **No look-ahead anywhere.** Selection, market-cap gating, and every feature must be computable
   from data at or before the decision slot. The gate already captures the brain fingerprint at
   admit for this reason.
3. **Costs must be the measured ones, not the defaults.** `dev_portable`'s round trip is a
   laptop-profile number and is known to under-price by ~150 bps (the first-sell penalty is charged
   at exit but absent from the gate model — see `NET_SOL_SANITY_AUDIT_2026-07-25.md` §5). Set
   `gate_protocol_bps` and the fixed term from *measured* server costs before believing any net.
4. **Fills are optimistic by default.** `OptimisticCeiling` assumes we get the printed price.
   Re-run under Mode-C adversarial before trusting anything, and treat the gap between the two as
   the honest error bar.
5. **Hold out.** Fit nothing on the full corpus. Split by time, and per Amendment A-11 the
   pre-existing corpora remain the arbiter — a parameter that wins only on the backtest and loses
   on the hazard tapes is fitted, exactly as flow-persistence was.
6. **A negative result is the expected outcome and is publishable.** The external literature
   (`STRATEGY_PERMUTATION_STUDY_2026-07-25.md` §D) finds **no published memecoin strategy with
   positive out-of-sample expectancy**; the best classifier reduces losses rather than producing
   profit. If the backtest says we do not clear the ~7% round trip, that is the finding, it goes in
   the artifact, and it is worth far more than a flattering number.

## 6. Status

* Harness: **built and verified end to end** against the release binary.
* Real-data run: **NOT DONE — blocked on data reachability, not on code.** No net-SOL claim from
  real data exists yet, and none should be cited until this runs on the server.
* Owner: Hermes, Phase-B. See the activation directive's action items.
