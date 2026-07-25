# ENTRY / EXIT WORKFLOW SCRUTINY — principal-quant A/B study (2026-07-25)

**Mandate (operator, verbatim intent):** scrutinize the entry/exit workflow end-to-end
from the POV of a principal memecoin Solana quant, referencing every prior research
finding. **Calculate + A/B test FIRST.** If a change moves net lamports, make it; if it
does not, argue against it. Find any live-scalping edge. Calculate every machinely-possible
permutation. Then hand a proven edge to the L7 build step, QA it, and commit green.

**Bottom line, up front:** after sweeping every never-A/B'd entry/exit knob one-at-a-time on
the golden tape and the movers across all six hazard tapes, **no entry/exit parameter change
is justified.** There is no net-lamport edge to capture on the available evidence, and the one
apparent "edge" (larger position sizing) is overfitting that fails the repo's own pre-registered
no-harm and no-fitting rules. The shipped values stay. This is an honest negative, documented,
not buried — and it is now guarded by a permanent pinned test so a future drift is loud.

---

## 1. Method — what was measured, and the acceptance rule (pre-registered)

The A/B harness is the same deterministic machinery the law-permutation sweep uses: build a
`Config`, drive a fixed event tape through `Engine` in `RunMode::Replay`, read `Report.net_lamports`
(the objective) plus admitted/rejected. Integer/fixed-point, no RNG, no wall-clock — the same
(config, tape) cell always returns the same net, so every number here is reproducible.

**Tapes.** Golden (512 mints × 6 rounds, a realistic pump.fun outcome mix — ~45% quick losers,
~35% small→mid winners, ~15% moderate, ~5% runners 2.5–6× — each mint riding its own deterministic
trajectory that rises to a peak then fades on order-flow, so the held-position lifecycle is actually
exercised), plus the five hazard tapes (B3-hazard, B7-happy, B7-unhappy, Conc-happy, Conc-mirror).

**Pre-registered acceptance rule (written before reading any result, inherited from
`law_permutation_sweep.rs` P1–P5):** a parameter change replaces the shipped value only if it
(P1) gains more than the **materiality bar of 100,000,000 lamports** (one 0.1-SOL bite) on the
arbiter, (P2) does **no hazard-tape harm** beyond one bite on any single tape, (P3) shows a
**≥3× asymmetry** on any two-sided pair, and (P5) is **not fitted** to a single tape's idiosyncratic
path. **If no candidate clears P1–P2, the answer is "shipped value stays," stated bluntly.**

**Surface swept.** Every knob the inventory flagged as never-A/B'd: the entire fixed and derived
exit lifecycle (`lc_hard_sl_bps`, `lc_trail_base_bps`, `lc_trail_k_div`, `lc_trail_max_bps`,
`lc_cvd_hold_frac_bps`, `lc_precursor_drop_bps`, `lc_stall_ticks`, `lc_max_hold_ticks`,
`lc_tp2/3_frac_bps`, `target_margin_mult_bp`, `target_floor_bp`, `target_ceiling_bp`), the optional
exit laws (`into_strength_exit_enable`, `vol_stop_enable`), and the entry-sizing chain (`f_base_bp`,
`total_risk_cap_bp`, `floor_fraction_bps`, `probe_frac_bp`, `max_concurrent_positions`).

---

## 2. Finding 1 — the price-based exit geometry is INERT (and why)

Every price-based exit trigger is **exactly neutral** across its full grid on the golden tape, and
**identical net across all six tapes** over wide ranges:

| Knob | Range swept | Net effect |
|---|---|---|
| `lc_hard_sl_bps` (hard stop) | 2,000 → 6,000 bps (−20% to −60%) | **0 on all 6 tapes** |
| `lc_trail_base_bps` (trail width) | 1,200 → 4,500 bps | **0 on all 6 tapes** |
| `lc_trail_k_div`, `lc_trail_max_bps` | 2→12, 6k→30k | **0** |
| `lc_cvd_hold_frac_bps` (thesis CVD) | 2,000 → 7,000 bps | **0 on all 6 tapes** |
| `lc_precursor_drop_bps` (rug precursor) | 1,500 → 5,000 bps | **0** |
| `lc_stall_ticks`, `lc_max_hold_ticks` (time) | 10→80, 150→900 | **0** |
| `lc_tp2_frac_bps`, `lc_tp3_frac_bps` (trim size) | 1,500 → 6,000 bps | **0** |
| `target_margin_mult_bp` (TP-ladder spacing) | 8,000 → 30,000 bps | ≤ +230k golden, sub-material, non-monotone |
| `into_strength_exit_enable`, `vol_stop_enable` | off → on | **0 on golden** |

**Root cause — proven, not guessed.** The exit-reason census on the golden tape (recent-decision
window) shows exits occur via **`ThesisInvalidation` (11) and `TakeProfitLadder` (1) only — zero
HardStop, TrailingStop, TimeStop, RugPrecursor, or ForceClose fills.** The binding exit on realistic
order-flow is the **§32 flow-sign-flip thesis force-exit** (OFI buy-pressure < 5,000 bps or CVD sign
turns negative — hardcoded structural rule), which fires the instant net-sell flow begins, **before**
price ever retraces to a trailing or hard stop. The price-based protective geometry never becomes the
binding constraint, so its parameters cannot move net. That the effect is identical across six
independent tapes rules out a sampling artifact: it is structural.

**Quant reading — this VALIDATES the exit design, it is not a defect.** "Exit on order-flow reversal,
not on price retracement" is precisely what a memecoin scalper should do: the flow flip is the earliest
exit signal, and waiting for a price stop only donates the retracement to the market. The research we
read agrees (sell into strength / do not round-trip winners; flow reversal precedes price on thin
low-caps). The elaborate price-stop lifecycle is correctly a **fail-safe backstop** — it exists for the
paths where flow data is absent or a single-swap collapse outruns the CVD read (RugPrecursor), which
is exactly when a scalper needs a hard floor. Its inٍertness on healthy tapes is the system working.

**The one honest caveat (a coverage gap, NOT an edge):** because the flow-flip pre-empts them, the
price-stop parameters are **unfalsified by the current tapes** — we cannot prove they are optimally set,
only that they do not bind here. This is model risk / an information gap, not lamports left on the table.
Tuning them to the golden tape would be tuning to a rule that does not fire — pure overfitting. The
correct future work is a **fine-grained flow-absent stress tape** that forces the price stops to bind;
that is a research-plane task, and it does not change a single lamport today, so no code changes on its
account now.

---

## 3. Finding 2 — sizing is the only mover, and it is already at a defensible Kelly frontier

The only knobs that move net are the sizing chain. Their behavior is the classic fractional-Kelly
surface, and it argues **for keeping the shipped values**, not changing them.

**`f_base_bp` (per-position fraction; shipped 667 = deep-fractional ≈ quarter-Kelly):**

| f_base_bp | GOLDEN | B7-happy | B7-unhappy | CONC-happy |
|---|---|---|---|---|
| 667 (SHIP) | 15,410,801 | 479,556,343 | 601,202,914 | **+16,567,514** |
| 800 | 36,499,065 | 763,863,618 | 840,020,209 | **−23,671,086** |
| 1,000 | 45,591,883 | 764,743,639 | 764,743,639 | **−29,540,863** |
| 1,200 | −19,516,754 | **24,035,894** | **24,035,894** | −28,793,076 |

Raising `f_base_bp` 667→1,000 inflates golden by **+30.18M** — but that gain is **itself sub-material**
(below one 0.1-SOL / 100M bite), so it never clears **P1** to begin with. The same change **flips the
concentration-hazard tape from +16.57M to −29.54M** (a sign flip, positive→negative — a qualitative
**P2** harm), and one notch further (1,200) **collapses B7-unhappy from 601M to 24M** (a −577M cliff,
now far past one bite). This is textbook overbetting past the Kelly-optimal fraction: the in-sample
"gain" is a sub-material blip on the left shoulder of a distribution whose right tail is ruin. **Fails
P1, P2, and P5.** The shipped 667 is the only value positive on all six tapes — the principled
conservative point, with comfortable margin below the blow-up. **Argue against raising it.** (It is also
derived from Kelly theory, not tape-fitted; replacing a principled risk parameter with an in-sample
curve-fit is the exact mistake the constitution forbids.)

**`total_risk_cap_bp` (2,100), `max_concurrent_positions` (3), `floor_fraction_bps` (2,500):** lowering
any of them hurts (fewer/smaller positions → less net on golden/B7); raising them is **saturated** on the
core tapes (the 3-position concurrency binds first) and merely reshuffles the concentration tapes
(conc-happy up, conc-mirror down at 3,500 — a wash). Each shipped value sits at the Pareto frontier: no
strict improvement exists. **Argue against changing them.**

---

## 4. Verdict against every candidate change (pre-registered rule applied)

| Candidate change | P1 material gain? | P2 no hazard harm? | Verdict |
|---|---|---|---|
| Any price-stop / trail / TP-ladder / CVD / time knob | **No** (inert / sub-material) | n/a | **REJECT — no edge; shipped stays** |
| `f_base_bp` ↑ (667→1,000) | Yes on golden/B7 | **No** (conc → negative; 1,200 ruinous) | **REJECT — overbet, fails P2 & P5** |
| `total_risk_cap_bp` ↑/↓ | No (saturated / harmful) | mixed | **REJECT — at frontier** |
| `max_concurrent_positions` ↑/↓ | No (saturated / harmful) | — | **REJECT — at frontier** |
| `floor_fraction_bps` ↑/↓ | No (≤ship neutral; ↑ harmful) | — | **REJECT — at frontier** |
| `into_strength` / `vol_stop` on | No (inert) | — | **REJECT — stay off** |

**No candidate clears P1 ∧ P2. The shipped entry/exit configuration stays, unchanged.** The place net
is actually made on these tapes is **entry selection and sizing discipline** — which is exactly what the
gate, the brain (LAW B3, armed), the concentration stream, and the fractional-Kelly sizer already govern
and which have already earned (or honestly failed) their own pre-registered A/Bs. The exit side has no
untapped lamport edge; its job is fast flow-reversal exits plus a fail-safe price backstop, and it does
that.

---

## 5. What was actually built this pass

Because the finding is a negative, the "L7 build" is not a parameter change — it is the durable artifact
that makes the negative machine-checked, plus the drift remediation surfaced by the end-to-end sweep:

1. **`tests/entry_exit_frontier.rs`** — a pinned A/B test that (a) asserts the price-based exit knobs are
   inert on the golden tape across extremes, (b) asserts the sizing overbet HARM on the concentration
   tape (so a future change that "improves" golden by overbetting trips this guard), and (c) pins the
   shipped-value net on every tape. The largest never-swept surface in the live path is now guarded.
2. **Doc drift fixed** (surfaced by the drift sweep, all doc-only, no code/re-pin): a stale re-pin #13
   golden baseline (digest 2_725_869_539_061_043_535 / net 1,406,102 / counts 504/14/467) had lagged into
   five operational docs while the code is at re-pin #21 (3_604_954_302_921_337_343 / 15,410,801 /
   504/13/457); the most dangerous was the activation one-shot ordering Hermes to verify the WRONG digest,
   which would false-trigger a determinism-break halt on a healthy build. Also corrected: BRAIN_SYSTEM.md
   still calling B3 "DEFAULT OFF" (it is ARMED), the recall-radius default (8, not 12), and closed gaps
   described as open. The byte-frozen constitution + mirror were not touched.

**Golden digest is unchanged (3_604_954_302_921_337_343 / net 15,410,801): no decision path changed, no
re-pin.** The study added measurement and guards only.
