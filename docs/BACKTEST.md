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
| **masonmarker/memecoins-chart-data-low-mc** (HF) | Free but **GATED** (401) | 1.16 GB chart data, explicitly LOW market cap | **Best free lead — needs the operator to accept terms on HF, then re-check granularity** |
| **muhammetakkurt/pump-fun-meme-token-dataset** (HF) | **Fully free, no key** | 67 MB CSV, ONE ROW PER TOKEN: mint, created_timestamp, creator, `complete`, market_cap, **virtual_sol/token_reserves**, socials, reply_count | **Not a backtest corpus — but a real LAUNCH UNIVERSE (see §1.1)** |
| **blackhawkdragon/pumpfun-real-data** (HF) | **Fully free, no key** | **366 rows**, per-token labeled OUTCOMES: `reached_50pct`, `reached_100pct`, `migrated`, `peak_price`, `final_price`, `buy_ratio`, `dev_success_rate` | Too small to be a corpus; useful as a labeled sanity sample |
| **rincel/pumpfun** (HF) | Fully free | 1.18 GB, but a SINGLE `SIGNER` column | Useless for backtesting; possibly a known-degen wallet list |
| **Zenodo 10.5281/zenodo.20633486** | Free | 832,941 pump.fun token launches (survival-analysis paper) | **Largest launch universe found.** `robots.txt` blocks this sandbox; reachable from the server |

### 1.1 The survey was widened after the first pass, and it changed the answer

The first pass only checked the sources in the operator's screenshots. A systematic sweep of the
HuggingFace dataset API (`pump.fun`, `pumpfun`, `solana`, `memecoin`) surfaced four datasets that
pass had missed, and two of them matter:

* **`muhammetakkurt/pump-fun-meme-token-dataset` partially solves the survivorship problem.** It is
  one row per token *as created* — including the dead ones (the sampled row is a token sitting at a
  28.5 SOL cap, `is_currently_live: false`, `complete: false`) — and it carries
  `virtual_sol_reserves` / `virtual_token_reserves`, so the curve state is real rather than
  reconstructed. **That makes it a usable `--universe-manifest` source for its window**, and an
  honest read on the real market-cap distribution at launch. It is NOT trade data and cannot drive
  a backtest.
* **`masonmarker/memecoins-chart-data-low-mc` is the best free lead and is one click from usable.**
  1.16 GB, explicitly low-market-cap, but **gated behind HF terms acceptance (401 from here)**.
  Nobody can evaluate its granularity until someone with an HF account accepts the terms. **If it
  turns out to be per-trade rather than candles, it is the single best free corpus found.** That is
  a concrete operator action, not a research task.

**The conclusion is unchanged: no FREE source yet gives per-swap pump.fun trades with side,
reserves, trader and timestamp at scale.** Helius (already paid for) and Dune's free
`pumpdotfun.trades` remain the two routes that do. What changed is that we now have a real launch
universe to audit survivorship against, and one gated candidate worth ten minutes of the operator's
time.

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

   ### Ready-made launch universes — and the window rule that governs all of them

   You do not always have to enumerate `create` instructions yourself. Three published
   universes exist, all free. **But every one of them covers WEEKS, not years, and that is the
   trap:** a universe is only valid for the window it was collected in.

   | Universe | Window | Size | License / access |
   |---|---|---|---|
   | **Zenodo `10.5281/zenodo.20633486`** (concept DOI) | **2026-05-08 → 2026-06-10** | **860,213 launches** | **CC-BY-4.0, open** |
   | `muhammetakkurt/pump-fun-meme-token-dataset` (HF) | ~Jan 2025 | 67 MB, one row per token | Free, no key |
   | Kaggle `dremovd/pump-fun-graduation-february-2025` | ~Feb 2025 | graduation cohort | Kaggle account |

   The Zenodo release is the best of the three — it is the dataset behind arXiv 2607.02823
   ("Survival Analysis of 832,941 Token Launches"), it is openly licensed, and it is a genuine
   launch-time census rather than a survivor list.

   **THE WINDOW RULE, which is binding: pull your trade data for the SAME window the universe
   covers.** Auditing a year of trades against the 33-day Zenodo universe produces a coverage
   figure that is not merely low, it is meaningless. If you need a different window, enumerate
   `create` instructions for that window via Helius instead — the ready-made sets do not
   generalise, and no amount of care downstream repairs a mismatched universe.

   **This is now machine-checked.** The converter counts corpus mints that are ABSENT from the
   universe. Any non-zero count means the universe does not cover the corpus window (or is the
   wrong universe), and the tool says so explicitly — on the console and stamped into the events
   header — stating that the coverage figure is not interpretable until the two windows match.

   **End-to-end, using the Zenodo universe:**

   ```bash
   # 1. Fetch the launch census (resolve the concept DOI to its latest version).
   #    CC-BY-4.0 — attribute arXiv 2607.02823 in any write-up.
   curl -L -o zenodo_launches.zip \
        "https://zenodo.org/api/records/20633486/files-archive"
   unzip zenodo_launches.zip -d zenodo_launches/

   # 2. Reduce it to the manifest format: one mint per line, nothing else.
   #    Adjust the column name to whatever the release actually ships.
   python3 - <<'EOF' > universe_2026-05-08_2026-06-10.txt
   import csv, glob, sys
   for path in glob.glob("zenodo_launches/*.csv"):
       with open(path, newline="", encoding="utf-8") as fh:
           for row in csv.DictReader(fh):
               m = row.get("mint") or row.get("token_address") or row.get("address")
               if m:
                   print(m.strip())
   EOF

   # 3. Pull trades for THE SAME WINDOW (Helius, 2026-05-08 .. 2026-06-10) -> swaps.jsonl
   #    Then convert, and the audit runs automatically.
   python3 tools/backtest/pump_replay_build.py \
       --in swaps.jsonl --out events.txt \
       --universe-manifest universe_2026-05-08_2026-06-10.txt \
       --min-mcap-sol 5 --max-mcap-sol 600

   # 4. Confirm the header says WINDOW MISMATCH: 0 before believing any net.
   head -8 events.txt
   ```

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

---

# §7. PRINCIPAL-QUANT SCRUTINY OF THE HARNESS (2026-07-27)

The harness was reviewed the way a desk would review a backtest before letting it size anything.
Findings are ordered by how much they would have flattered the result.

## FATAL — would have invalidated any net figure

### F1. Fills happen at the observed print, with ZERO own-impact **(now buildable)**

`engine.rs:2992` — `let entry_price = self.numeric.latest_price_fp(mint).unwrap_or(0);` — opens the
position at the **last observed print**, and exits do the same. `pump_quant_simulator::fill::
simulate_fill` exists but is **never called on the held-position path** (only in `scalp.rs`, a
separate single-shot evaluator).

pump.fun is a constant-product bonding curve. Executing size `S` does not fill at the marginal
price; it fills at the average along the curve, which is *strictly worse* — and because we hold the
reserves, that is **exactly computable, so approximating it is inexcusable**. Closed form:
`avg/spot = (vsol + sol_in)/vsol`, i.e. impact in bps is exactly `sol_in · 10_000 / vsol`. At a
fresh 30-SOL curve a 0.1 SOL clip pays **33 bps** we never charged ourselves — on both legs, on
every trade, against a ~700 bps round trip.

**Built this pass:** `crates/pump-quant-app/src/curve_fill.rs` — exact integer constant-product
execution (`buy_tokens_out`, `sell_sol_out`, `buy_avg_price_fp`, `sell_avg_price_fp`,
`spot_price_fp`, `buy_impact_bps`, `sell_impact_bps`), `u128` intermediates, `checked_*` throughout,
`None` rather than saturation on degenerate input, and **conservative rounding** (buy average rounds
UP, sell average rounds DOWN) so the model can never flatter a fill. 14 unit tests pin the
properties that matter: buy-average ≥ spot always, sell-average ≤ spot always, impact monotone in
size, convergence to spot as size → 0, `k` preserved across a round trip, and a pinned worked
example at real pump.fun reserves.

### F2. Same-slot fill is look-ahead, and it violates our own law

The replay observes a swap and fills at that same price in the same slot. Real landing is ≥ 1 slot
(~400 ms) later, and **criterion 103 already requires evaluating at expected LANDING state, never
observation state.** The harness was breaking a constitutional rule in the flattering direction.

**Built this pass:** `fill_landing_slots` config (default `0` = today's behaviour, so nothing moves
until deliberately enabled).

## SEVERE — biases magnitude

### S1. Price quantization swamps the edge at real reserves **(fixed)**

This one was found only because an independent hand-check of the curve math disagreed with the
module's own test — and chasing the disagreement was more valuable than the check.

`price_fp = vsol · 10_000_000 / vtok`. At real pump.fun reserves (`vsol` 30 SOL,
`vtok` ≈ 1.07e15) that lands at **279 — three significant digits.** The smallest representable
price move is therefore **~36 bps**, which is the same order as the entire per-trade edge. Measured
across the realistic band:

| curve SOL | `price_fp` | smallest representable move |
|---|---|---|
| 30 | 279 | **35.8 bps** |
| 45 | 629 | 15.9 bps |
| 85 | 2,236 | 4.5 bps |
| 115 | 4,107 | 2.4 bps |

A raw-scaled backtest would have been dominated by quantization noise, and the noise is *invisible*
in the output — it looks like strategy performance.

**Fixed:** the converter now re-bases each mint's series to its own first observation
(`--price-normalize per-mint`, default). The engine consumes `price_fp` **only through ratios**
(`price · 10_000 / entry_price`, verified in `position.rs`), so this is **exact, not an
approximation**, and resolution goes from ~36 bps to ~0.001 bps. The market-cap gate deliberately
keeps the RAW absolute price, since a per-mint ratio carries no absolute scale. The tool reports the
raw quantum it eliminated, and warns if `raw` mode is selected with a quantum ≥ 10 bps.

### S2. Our fills do not consume the liquidity they take

A simulated buy does not move the curve for subsequent tape events, so the historical path continues
as though we were never there — and that path partly reflects the very trades we would be
displacing. **This is not fully fixable in a single-agent replay.** The honest mitigation is to keep
our size small relative to contemporaneous flow and to report the participation rate, so the
counterfactual damage is bounded and visible rather than assumed away. Not yet built; specified here
so it is not forgotten.

### S3. Logical time is tied to trade COUNT, not slots

`--tick-every N` advances the clock every N trades, so time-based exits and decay run fast in busy
markets and slow in quiet ones — exactly backwards. Time stops should key off slots. Not yet built.

## METHODOLOGICAL

* **M1. "First sighting" is not "launch"** unless the pull begins at creation. If the data starts
  mid-life the mcap gate admits mid-flight tokens, which is an easier and different game. Check
  `age_slots` at first sighting.
* **M2. Reserve semantics are unvalidated** — the converter documents reserves as AFTER the swap; a
  source that emits BEFORE would shift every price by one swap. Detectable via a constant-product
  consistency check against the swap amounts.
* **M3. No warm-up exclusion.** Lane weights and the brain adapt from cold, so early trades are
  measured under a model that has not converged.
* **M4. No purged/embargoed out-of-sample split and no multiple-testing correction**, while the
  parameters being evaluated have already been tuned elsewhere. The repo's §51 FDR/PBO machinery is
  built but unwired — wire it before believing a positive result.
* **M5. Cost model under-prices by ~150 bps** (`NET_SOL_SANITY_AUDIT_2026-07-25.md` §5). Set costs
  from measured server figures first.

## Status after this pass

`curve_fill.rs` and both config fields are built and tested; **defaults reproduce prior behaviour
exactly**, so re-pin #23 is SEED-ONLY (net 15,410,801 / 504 / 13 / 457 / 72 all unchanged — only the
§19 config-identity digest moved). **Wiring the curve fill into the decision path is deliberately
NOT done here**: it changes every historical number and must land as its own gated change with its
own A/B. Until it is wired, **no real-data net is admissible**, because F1 and F2 both remain live
in the fill path.

---

# §8. THE CURVE FILL IS NOW WIRED — and it found something bigger than itself

§7 left F1/F2 built but unwired, with the note that wiring "needs its own pass". That was
capacity reasoning dressed as methodology. It is now wired, A/B'd, and the result matters more
than the wiring did.

## The blocker that was not one

Wiring appeared to need the TOKEN reserve, which the events grammar has never carried. It does
not. For a constant-product curve the token reserve **cancels**:

```
    buy:   avg/spot      = (vsol + notional) / vsol
    sell:  realized/spot = vsol / (vsol + notional)
    both:  own-impact bps = notional · 10_000 / vsol
```

Only the SOL side appears — and the engine has always carried it as `liquidity_lamports`. Verified
against the full constant-product computation in
`curve_fill::reserve_free_tests::impact_bps_matches_the_full_curve_computation`. So the fill could
be priced **exactly**, not approximately, with data already in hand.

## Wired on both legs

* **Entry** (`engine.rs`): the admit price is `buy_fill_price_fp(spot, vsol, size)` — we pay up the
  curve. Unknown depth REFUSES the entry rather than guessing (§6).
* **Exit** (`position.rs::realize`): each sell adds `notional · 10_000 / vsol` bps to the existing
  §38 adversarial impairment — they are different frictions and both are charged. The freshest
  curve depth rides on the position, updated per swap, and **fails closed at 0**.
* `ScalpLifecycle::on_trade` and the §48 shadow tournament both take the depth, so challengers are
  scored under the same fill model as the incumbent.

## The A/B — and the finding

| tape | curve fill OFF | curve fill ON |
|---|---|---|
| GOLDEN | **+15,410,801** | **−332,289,498** |
| CONC-happy | +16,567,514 | −287,474,148 |
| CONC-mirror | +55,684,531 | −279,553,950 |

Every tape flips deeply negative. **That is not a verdict on the strategy — it is a verdict on the
TAPES, and it is the most consequential thing this work found.**

| pool depth (`vsol`) | own-impact per leg, 0.1 SOL clip |
|---|---|
| golden tape MIN, 0.12 SOL | **8,333 bps** |
| golden tape MAX, 0.47 SOL | **2,127 bps** |
| **REAL pump.fun at launch, 30 SOL** | **33 bps** |
| real, near graduation, 85 SOL | 11 bps |

The golden tape's pools are **0.12–0.47 SOL** while the operator's minimum clip is **0.1 SOL**. Our
own order is **21–83% of the entire pool**, and every measurement ever taken on that tape was taken
while charging ourselves nothing for it. Real pump.fun virtual reserves start at ~30 SOL — the tape
understates depth by roughly **250×** relative to our clip.

**What this does and does not invalidate, precisely:**

* **Relative A/B verdicts survive.** Both arms of every prior comparison paid the same (zero)
  impact, so armed-vs-disarmed conclusions — B3, B7, concentration, flow-persistence — are not
  overturned.
* **The absolute net does not survive.** "+15,410,801 on a cost-realistic tape" is not a coherent
  economic statement when the clip is a fifth of the pool. The tape is cost-realistic in **fees**
  and badly unrealistic in **depth**. That headline should not be quoted as an economic result
  again, including in the activation directive's "honest baseline" paragraph.

## Status and the next move

Default stays **OFF**: arming it against unrealistic depth produces nonsense rather than a verdict,
and `curve_fill_wiring.rs` pins that reasoning in arithmetic (3 tests) so it cannot quietly rot. The
golden digest is unchanged — the disarmed path is byte-identical.

**The real fix is to give the synthetic tapes pump.fun-realistic reserves (~30 SOL virtual at
launch, scaling toward ~85 at graduation).** Under real depth the charge is ~33 bps/leg, ~66 bps
round trip against a ~700 bps round trip: material, and survivable. That re-pins every tape and is
the correct next pass.

**For any real-data backtest, `curve_exact_fill_enable` MUST be armed** — there the depth is real,
and this is precisely the friction that decides whether the strategy clears its costs.
