# NORTH STAR DATA-MIX AMENDMENT V6 — Empirical BUY/WATCH/SKIP Class-Balance Lesson

Date: 2026-09-07 · Source: Linux Hermes (qwen27b), SFT eval post-mortem
Supersedes: nothing (addendum to `NORTH_STAR_WINDOWS_MASTER_V5.md`)
Applies to: BOTH Linux and Windows agents. READ THIS BEFORE aggregating/labeling
any North Star training dataset.

---

## 1. The verified failure we must not repeat

The just-finished SFT run (Qwen 27B, `qwen_sft_v2.jsonl`, 2,162 records) was
evaluated against the frozen held-out `qwen_eval_v1.2` corpus. Result on the
buy-decision task: **the model emitted ZERO BUY actions across 36 evaluated
panels** (367 WATCH / 83 SKIP), while the reference expected ~249 BUYs across
234/250 panels.

### Root cause (source-verified, not hypothesis)
The training set's supervised action distribution was catastrophically skewed:

- **WATCH: 6,197**
- **SKIP: 3,262**
- **BUY: 22  ← 0.2% of all action labels**

The model was effectively **never taught what a BUY decision looks like**. It is
not "too conservative", not a decode/threshold bug, and not fixable by post-hoc
calibration — the BUY class had negligible supervised signal, so the policy
collapsed to the dominant WATCH/SKIP prior. (Most pump.fun tokens ARE garbage, so
the caution is partly correct — but it suppressed genuine buy opportunities
entirely.)

### Lesson (binding)
A decision-class model MUST have a meaningful, balanced representation of every
action it is expected to emit. A class present at <1% of labels is effectively
absent from the learned policy.

---

## 2. Binding requirement for the North Star dataset

The North Star training data aggregation (Windows side) MUST produce a dataset
whose action distribution includes a robust, economically-justified mix of ALL
decision classes:

- **BUY (long/enter)** — a meaningful minority, target order-of-magnitude
  **5–15% of action tokens** (not 0.2%), reflecting realistic profitable-entry
  frequency on qualified panels.
- **WATCH (hold / monitor / no-new-position)** — the majority, as in real markets.
- **SKIP / SELL (exit, take-profit, cut-loss)** — must include BOTH:
  - **SELL as an active take-profit / runner-management action** (North Star
    specifically needs partial-profit-taking, runner-holding, and re-entry
    decisions, not just a flat "exit").
  - **SKIP as deliberate no-trade** (incl. missed-trade / idle-period honesty).

### How to decide the right mix (do not guess)
1. Compute the **reference action distribution** from the underlying labeled
   gold corpus FIRST (the eval/training sources, wallet-derived ground truth).
2. Ensure every class that appears in the reference appears in TRAINING with
   enough count that it can be learned — as a rule of thumb **≥100–300 examples
   per class** and **≥2–5% label share** per decision class the model must emit.
3. If real-world buy frequency is genuinely rare in the raw data, DO NOT pad or
   fabricate to hit a target (binding: no duplication/padding) — instead **over-
   sample/upweight the genuine buy-labeled records in the loss** (the trainer
   supports task weights), and expand collection toward qualified/graduation
   candidates where buys are more common.
4. Mirror the same balance in the eval split, so the eval can actually measure
   buy-decision quality (precision, recall, false-buy rate).

### Validation gate (must pass before training)
Before any North Star SFT, run a class-balance audit on the training export:
count BUY / WATCH / SKIP / SELL labels. FAIL the export if any action class the
model must emit falls below the thresholds above. The current SFT shipped with a
BUY class at 0.2% and a model that literally cannot say BUY — that is the failure
mode this gate exists to prevent.

---

## 3. Implication for the model pipeline

- The current SFT model is **not ready to judge buy decisions** and will be
  **superseded** by the North Star lineage (one model: reasoning + executable
  decisions + Rust developer, per v5). Do not invest further in shipping this SFT
  checkpoint as a buy-capable executor until re-trained on balanced data.
- North Star model training therefore does NOT start from this degenerate SFT
  lineage for the decision layer. Per v5 architecture it trains on its own
  balanced North Star dataset (base/CPT-untrained seed — see v5 seed decision).
- The ranking/ordering backbone (full_rank_aux ordering showed correct top-candidate
  selection with spearman ≈0.49 on a sample) was NOT the failure — the action-label
  calibration was. Keep the ordering data; fix the action mix.

---

## 4. Files
- This amendment: `NORTH_STAR_DATA_MIX_AMENDMENT_V6.md` (new)
- Master it amends: `2026-09-06-expert-v5/NORTH_STAR_WINDOWS_MASTER_V5.md`
- Superseded empirical reference (Linux-local): `/home/alon/sft-native/` eval scripts,
  `/training/runs/qwen27b_eval_v1_2/`
- Skill (Linux): `mev-bot-qwen-training`, `serving-llms-vllm`
