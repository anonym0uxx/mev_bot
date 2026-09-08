# North Star — Stage 1: Protect Evaluation

Status: **Stage 1 framework established; exact held-out ID freeze deferred to schema
import (Stage 1b).** Date: 2026-09-07.

Goal: freeze the evaluation contract and the leakage-safety rules BEFORE any feature
engineering, ranking, or execution-model fitting, so the eventual balanced dataset and
its eval are measured on a partition the model never trained on, with no hindsight.

---

## 1. Evaluation boundary strategy

- **Held-out partition is frozen by token identity + time**, not by random row split.
  A mint that appears anywhere in training (or any temporally-overlapping window) must
  not appear in eval.
- **Eval mirrors the class balance** (V6): BUY/WATCH/SKIP/SELL represented at the same
  target shares as training, so BUY quality (precision/recall/false-buy rate) is
  actually measurable — the prior `qwen_eval_v1.2` could only observe "0 buys" from a
  model that never learned BUY; the North Star eval must be able to detect real BUY
  capability.
- **The prior `qwen_eval_v1.2` is retained only as a regression reference** for the
  superseded lineage, not as the North Star eval. North Star defines its own frozen
  partition.
- Freeze order (binding): eval IDs → features → rankings → execution models. Nothing
  earlier may be re-derived from anything later.

## 2. Leakage-safe export contracts (binding)

1. **Ex-ante only.** Every causal input at decision time `t` uses only information
   available strictly before `t`. No post-`t` fields (outcome, final PnL, "rugged
   later", completion status) may enter any input feature.
2. **No future-looking fields, ever.** "Did this token graduate", "final multiple",
   "exit price at +24h" are labels/outcomes only, never inputs.
3. **No AI-generated rationales.** Training reasoning text comes from human/on-chain
   source only (the original-source spans); the model must learn from real human
   judgments, not from another model's prose.
4. **Group-disjoint splits.** train / validation / eval are disjoint at the mint level
   (and, where mints overlap a theme, at the theme/wallet level), not row level.
5. **Temporal coherence.** Decision episodes are ordered; a later decision on the same
   token may not leak into an earlier episode's input.
6. **Freeze before fit.** Eval partition and export schema are frozen before any
   feature engineering, teacher/ranking fitting, or execution calibration.

## 3. Canonical accounting (binding numerics)

- **SOL ↔ lamports: 1 SOL = 1e9 lamports.** (Prior 1000× bug: 0.0178 SOL = 17,800,000
  lamports, not 17,800.) Every conversion is cross-checked.
- **Real fills vs modeled exits are distinct.** Balance-delta ground truth (from
  `rust-data/` `full_trades.pkl` / `event_stream.jsonl`) is the only "actual fill"
  signal. LaserStream counterfactual/modeled exits are labeled `modeled`, never treated
  as executed.
- **Fees, tips, and priority are explicit** in every PnL (Jito tip, AMM fee, slippage),
  so net PnL reflects our real cost/latency.
- **Counterfactual economics ≠ executed trades.** Feasible-entry economics are used to
  *derive* BUY labels where human decisions are absent, but they are marked as derived
  ground truth, not realized trades.

## 4. Next (Stage 1b)

Import the gold-layer schemas (laserstream/slinky/narrative) to pin the exact field
names and IDs, then:
- freeze the held-out mint/time partition (hash-pinned ID list),
- write the export schema that enforces §2 programmatically (assert no post-`t` field,
  group-disjoint), and
- produce the canonical accounting function with lamport-exact rounding.

Stage 2 follows: market/narrative capture toward filling the D11 (decisions) and
D04/D06/D08/D09 gaps.
