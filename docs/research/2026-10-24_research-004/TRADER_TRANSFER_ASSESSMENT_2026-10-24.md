# Trader-setup transfer assessment — 2026-10-24 (offline; frozen panel outcomes NOT read)

Scope per the 2/2 directive: existing archives only (slinky21 Geyser corpus Jun 5–Jul 14 2026, 33.58 M curve trades;
operator seed registry wallets), one best-supported setup, one declared window, causal exits, our size and latency.
Scripts: `extract_trader_fills.py`, `trader_own_pnl.py`, `follower_replay.py`, `own_entries_markout.py`.

## 0. What exists / what is missing
- Latest trader research: `STAGE2_AGGREGATION_INVENTORY.md` (Sep 8) established slinky21 as the authoritative KOL
  source (RPC `getTransaction` is blind to bundled trades). `kol_decisions.jsonl` (Sep 8) aggregates match this pass.
- Narrative lane: `narrative_gold_v1.1` temporal ambiguity (Aug 29) is NOT resolved by any later artifact found; the
  X lane is login-walled. No timestamped narrative exists for the June window → the "information" question (narrative /
  wallet-inventory / buyer-retention beyond numeric fields) cannot be tested on this corpus. Missing evidence, not zero.
- Wallet attribution: operator seed labels (`narrative_seeds_v1.yaml`), unverified identity. Prosper/Pr6spr: NO wallet in
  any registry → no coverage. Orangie: 16 curve fills / 8 mints → unusable. Usable: Cupsey (6 labelled wallets, 15,672
  fills, 997 mints), Cented (8 wallets, 7,992 / 1,888), Megga (2 wallets, 2,763 / 800). Potion 427 fills (small).
- Tape limitation: slinky21 trades are BONDING-CURVE only. Post-graduation AMM sales are invisible. Any inventory
  still held at the last curve row is UNRESOLVED. `sol_amount` = curve leg excluding the 1 % fee (verified on reserves
  deltas); reserves are post-trade; the §3.1 "inconsistency" for these wallets is price impact of 1–4 SOL clips, not
  corruption (ratio outside [0.5, 2] in <1 % of rows). 18 duplicate signatures.

## 1. What the trader actually earned at their own fills (full window, curve only, fee-adjusted)
Accounting convention (corrected 2026-10-24): sale proceeds − purchases − fees over a window is NET CASH FLOW. It equals
realized trading P&L only for positions fully closed on the tape. Sold quantities were NOT matched to cost basis here.
Cupsey, four distinct components: (a) 317 mints fully round-tripped on the curve → realized trading P&L **+31.6 SOL**
(52 % win, median +0.073 SOL per mint ≈ +1.5 % of a ~5 SOL clip); (b) 472 mints with inventory remaining at the last
curve row → net cash flow −1,909 SOL, inventory estimate 2,773 SOL at last curve price (1,445 in 100 graduated mints,
1,328 in non-graduated) — P&L UNRESOLVED; (c) 20 sell-only mints (unknown basis) → +83 SOL of proceeds, P&L unknown;
(d) post-graduation AMM sales — absent from the tape. Whole-window curve net cash flow: buys 5,229, sells 3,736 →
**−1,584 SOL net cash flow (NOT realized P&L)**. His window P&L is therefore **UNRESOLVED on this tape**: it depends on
(b)+(d), which we cannot see. Median entry age **12.8 s after launch** (p90 106 s); first sell median
26 s after first buy; buys split across wallets in sub-second bursts of 4–8 fills.
Cented: net cash flow −331 (−838 ex sell-only mints); 95 fully-exited mints realized −6.5 SOL (4 % win); 322 "oversold"
mints (tokens from outside the tape, basis unknown). Megga: net cash flow +65 but −239 excluding sell-only mints
(inventory acquired outside the window). Conclusion (scoped): the observable curve-only part of their activity does not
demonstrate profitability, and this tape CANNOT establish that they are unprofitable — the decisive components (b)+(d)
are unobserved.

## 2. Delayed observation of their public buy at our first feasible landing (Δ = 2.5 s and 5 s), 0.15 SOL clip, 1 % fee
First BUY per mint, curve constant-product fill from the state at landing, declared exits:
E1 = mirror their first public SELL (+Δ); E2 = 120 s; E3 = 1,800 s barrier; M30 = 30 s markout. Unresolved = exit after
the last curve row (graduation/no trades) or no public sell.
| trader | entries | median fill vs their price | M30 resolved / mean / win | E1 resolved / Σ net / win | E2 resolved / Σ net / win |
|---|---|---|---|---|---|
| Cupsey Δ2.5 | 913 | **+5,231 bp** (87 % > +20 %) | 740 / −0.014 / 34 % | 639 / **−25.8 SOL** / 13 % | 656 / −21.7 / 26 % |
| Cupsey Δ5 | 913 | +4,869 bp | 740 / −0.011 / 35 % | 636 / −24.4 / 13 % | 655 / −18.9 / 25 % |
| Cented Δ2.5 | 1,533 | +3,770 bp | 1,145 / −0.018 / 30 % | 981 / −30.6 / 14 % | 1,008 / −33.2 / 20 % |
| Megga Δ2.5 | 668 | +1,826 bp | 507 / −0.018 / 28 % | 475 / −13.0 / 17 % | 437 / −14.6 / 19 % |
Per 0.15 SOL entry the E1 mean is ≈ −27 %. E3 (PROD_HOLD-like) is 91 % unresolved on the curve (graduation/death) and
negative where resolved. Including last-curve-price marks for unresolved rows does not change the sign. Row sums are
diagnostics, not portfolio P&L; a sequential book was not simulated because every exit policy loses at the row level.
Mechanism: by Δ = 2.5 s the price is already +37–52 % above their fill — their own split fills plus follower flow move a
30 SOL virtual curve; the public signal arrives after the move. Their edge, if any, is being first at the curve (≈ 10 s
after launch) at 1–4 SOL per clip, plus whatever happens after graduation — neither is available to a 2.5 s follower.

## 3. What a rule/model with our pre-decision information could reproduce
Nothing from this setup: our admission already refuses pumpfun age < 5 min / warm < 20 by design, and the entries that
matter here happen at 10–100 s of age on the curve. A copy rule is dominated by latency (§2). The question "do their
buys carry information beyond the price move" cannot be separated from follower-induced flow on this tape.

## 4. Our own 37 historical fills (cohort-003 15 + research-001 22; closed books)
Price-only markouts from the journaled AmmSwap tape vs fill price: 30 s < −5 % in **12/37** (immediately adverse);
**13/37** not adverse at 30 s, reached ≥ +15 % before our exit, closed at a loss (gains surrendered; the peak is a
hindsight classifier, not a benchmark); 8 closed positive. Declared causal fixed-horizon exits at 0.15 SOL net of fees:
30 s Σ −0.489 SOL (13/37 positive), 120 s Σ −0.935 SOL (15/37 positive) — both lose; actual closes Σ −1.585 SOL (row
view; ledgers −1.4847 / −0.2811). So: roughly a third of our entries were adverse immediately; a third saw a transient
gain that no declared causal exit captured; selection and exit both fail, and "sell faster" at 30/120 s does not fix it.

## 5. Edge specification (as requested) — result: NO TRANSFERABLE EDGE DEMONSTRATED by the tested delayed-following
and fixed-exit strategies on these datasets. This is not proof that the traders are unprofitable, nor that no
pre-decision predictor can work; negative aggregate row outcomes justify declining an expensive portfolio test here —
they do not establish that every subset or sequential policy must lose.
Central implication carried forward: our model must identify an opportunity BEFORE the public trade has consumed it;
trader fame, historical profitability and observing a BUY are not sufficient predictive inputs.
The 13 own positions with a positive excursion before a losing close identify a MANAGEMENT QUESTION, not an executable
profit opportunity (no hindsight exit times; step250 stays excluded).
- Eligible setup: none demonstrated. The only transferable observable (their public buy) is priced out within 2.5 s.
- Mechanism/competing explanation: early-curve priority + capital + exit on graduated inventory (unauditable) vs.
  follower-induced flow; indistinguishable here.
- Entry/size/exit: not specifiable with positive expectancy under our constraints.
- Net outcome vs cash and frozen benchmark: every declared policy < cash by 13–27 % per entry.
- LLM contribution: untestable until a setup with positive causal economics exists.
- Rejection evidence (already obtained): §2 at both Δ, all three traders, all exits.
- Smallest next prospective test: none warranted from this setup. Missing evidence that would change the picture:
  (a) post-graduation sales of the same wallets (AMM tape with trader identity) to settle §1; (b) a timestamped narrative
  feed aligned to a trade tape for the "information" question; (c) for our own entries, a continued-demand observable
  measured BEFORE the decision — the current R panel is the only registered test of that kind and stays frozen.
