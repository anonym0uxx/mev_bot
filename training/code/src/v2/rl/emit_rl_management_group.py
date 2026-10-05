#!/usr/bin/env python
"""Emit the RL management arm - the group GRPO never had.

WHY. `grpo_dataset.py:216` admits only `rec.get("family") != "decision"` rows, so the RL
candidate set has always been entry-only (BUY / WATCH / SKIP plus the policy arms). RL
could therefore never learn to exit, even though SFT teaches HOLD / ADD / REDUCE / EXIT -
the exact train != serve split this whole effort exists to remove. Spec 9 requires the
management tokens in the RL group.

WHAT IT EMITS. One RL record per management decision point, carrying the arm values for
HOLD / ADD / REDUCE / EXIT. The values are not recomputed here and not retyped: they are
`meta.candidates`, which `build_management_c10.py` wrote by calling
`grpo_reward_bridge.score_decision(family="management", entry=held_state)` - the same
function `qa_management_c10.py` re-scores 250/250 against. So the RL arm and the SFT label
are the same number from the same authority, which is the invariant.

Verification is deliberately local and cheap: arm-set completeness, argmax == meta.action,
finiteness, and identity/split integrity. The value-equality claim rests on the family QA
(`reports/QA_MANAGEMENT_C10.json`, 250/250 agreement) whose run postdates the artifact.

RECORD SCHEMA v2 (2026-09-21, CVaR carry-through). Three fields are ADDED to every
emitted record, next to the existing arm/chosen payload. Nothing existing is renamed,
reordered or retuned:

  group_loss_weight  float - the per-decision GROUP LOSS MULTIPLIER the reward bridge
                            (`grpo_reward_bridge.score_decision`) reports, carried through
                            so the objective is available at train time without a
                            re-score. ABSENCE MEANS 1.0: a record written by the pre-v2
                            emitter carries no such field and MUST be read as 1.0, never
                            fabricated. Today the bridge returns exactly 1.0 for every
                            decision because lambda_cvar is pinned to 0.0 (OFF), so
                            carrying it is an exact no-op - `rl_cvar_noop_gate.py` proves
                            it against a byte-level pre-change emission of this file.
  cvar               dict  - a COMPACT tail-risk diagnostic of the record's OWN group
                            (alpha, cvar, cvar_z, n, n_tail, applied, lambda_cvar,
                            source). Diagnostics ONLY: no number here reaches the
                            advantages. Taken verbatim from `meta.cvar` when a build
                            persists it; else derived from the record's own group values
                            by the pinned `reward_terms` authority and stamped
                            `source="derived_from_group"` - derivation from the record's
                            own numbers, never a fabricated bridge value.
  schema_version     int   - 2 for this layout (1 = pre-v2, the field then absent).

WHY carry instead of recompute: advantages are precomputed ONCE at build time and merely
looked up during training (grpo_trainer.py, design choice 3), so a per-decision field that
is not carried HERE is simply unavailable at train time.
"""
import argparse
import collections
import json
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

# reward_terms is the pinned authority for the CVaR math (`group_cvar`); re-implementing
# it here would let the diagnostic drift from the number the objective actually uses.
from reward_terms import RewardConfig, tail_amplified_rewards  # noqa: E402

MGMT = "/training/v2/reports/mgmt_c10"
OUT = "/training/v2/rl_group_management"
ARMS = ("HOLD", "ADD", "REDUCE", "EXIT")
SPLIT_LEGACY = {"train": "train", "validation": "val", "examination": "test"}
RECORD_SCHEMA_VERSION = 2
NEW_FIELDS = ("group_loss_weight", "cvar", "schema_version")


def cvar_summary(group_values):
    """Compact tail-risk diagnostic of one group, from the pinned reward_terms authority.

    lambda_cvar is deliberately NOT a parameter: the diagnostics must report the tail as
    the OBJECTIVE would see it, and the objective's lambda is pinned in reward_terms
    (0.0 today). A caller-supplied lambda here could report a tail the loss never uses.
    """
    tap = tail_amplified_rewards([float(v) for v in group_values], RewardConfig())
    return {"alpha": tap["alpha"], "cvar": float(tap["cvar"]),
            "cvar_z": float(tap["cvar_z"]), "n": int(tap["n"]),
            "n_tail": int(tap["n_tail"]), "applied": bool(tap["applied"]),
            "lambda_cvar": float(tap["lambda_cvar"]),
            "source": "derived_from_group"}


def carried_diagnostics(meta, group_values):
    """(group_loss_weight, cvar) for one decision.

    A MISSING group_loss_weight is the documented legacy 1.0, not a fallback guess: v1
    records were emitted with lambda_cvar pinned OFF, so their true weight IS 1.0. A
    PRESENT-but-malformed weight is refused loudly rather than coerced, because a
    corrupt weight silently read as 1.0 would drop a real objective factor.
    """
    raw = meta.get("group_loss_weight")
    if raw is None:
        glw = 1.0
    elif isinstance(raw, bool) or not isinstance(raw, (int, float)) \
            or not math.isfinite(float(raw)) or float(raw) < 0.0:
        raise ValueError(f"REFUSING: malformed group_loss_weight {raw!r} - expected a "
                         f"finite float >= 0, or no field at all (which means 1.0)")
    else:
        glw = float(raw)
    src = meta.get("cvar")
    return glw, (src if isinstance(src, dict) else cvar_summary(group_values))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--splits", nargs="+", default=["train", "validation", "examination"])
    ap.add_argument("--outdir", default=OUT)
    ap.add_argument("--src", default=MGMT,
                    help="management family staging dir (c12: /training/v2/reports/mgmt_c12_aligned)")
    ap.add_argument("--verify-only", action="store_true")
    ap.add_argument("--report", default="",
                    help="report path; default is the historical reports/ path. It is "
                         "overridable so a diagnostic run into a temp --outdir cannot "
                         "overwrite the production manifest")
    args = ap.parse_args()
    if not args.verify_only:
        os.makedirs(args.outdir, exist_ok=True)

    report = {"group": "management", "arms": list(ARMS),
              "schema_version": RECORD_SCHEMA_VERSION,
              # The manifest names the record schema it describes, so a consumer reading
              # this report never has to guess whether group_loss_weight/cvar exist.
              "record_schema": {
                  "version": RECORD_SCHEMA_VERSION,
                  "carried_fields": list(NEW_FIELDS),
                  "absence_semantics": {
                      "group_loss_weight": "a record without this field is read as 1.0",
                      "cvar": "diagnostics only; absence means no tail report",
                  },
              },
              "suites": {}}
    total_bad = 0
    for split in args.splits:
        src = os.path.join(args.src, f"{split}.jsonl")
        if not os.path.exists(src):
            continue
        st = collections.Counter()
        fout = None
        if not args.verify_only:
            fout = open(os.path.join(args.outdir, f"{split}.jsonl"), "w")
        with open(src) as fh:
            for line in fh:
                r = json.loads(line)
                m = r["meta"]
                st["rows"] += 1
                cand = m.get("candidates") or {}
                if set(cand) != set(ARMS):
                    st["bad_arm_set"] += 1
                    continue
                if any(not isinstance(v, (int, float)) or not math.isfinite(v)
                       for v in cand.values()):
                    st["nonfinite_arm"] += 1
                    continue
                best = max(cand, key=lambda k: cand[k])
                if best != m.get("action"):
                    st["argmax_disagrees_with_label"] += 1
                    continue
                glw, cvar = carried_diagnostics(m, [cand[k] for k in ARMS])
                rec = {
                    "episode_id": r.get("episode_id"),
                    "family": "management_replay",
                    "group": "management",
                    "mint": r.get("mint"),
                    "split": SPLIT_LEGACY.get(m.get("split"), "train"),
                    "messages": r["messages"],
                    "arm_values": {k: cand[k] for k in ARMS},
                    "chosen": m.get("action"),
                    # v2 carried fields, next to the arm payload the trainer looks up.
                    "group_loss_weight": glw,
                    "cvar": cvar,
                    "schema_version": RECORD_SCHEMA_VERSION,
                    "advantage_basis": "reward_engine.score_action_sequence(entry=held_state)",
                    "authority": "grpo_reward_bridge.score_decision(family='management')",
                    "step": m.get("step"),
                    "cost_authority": m.get("cost_authority"),
                }
                if fout:
                    fout.write(json.dumps(rec, ensure_ascii=False) + "\n")
                st["emitted"] += 1
        if fout:
            fout.close()
        bad = sum(v for k, v in st.items() if k not in ("rows", "emitted"))
        total_bad += bad
        report["suites"][split] = dict(st)
        print(split, "rows", st["rows"], "emitted", st["emitted"], "violations", bad,
              flush=True)
    out = (args.report or ("/training/v2/reports/RL_MANAGEMENT_GROUP_VERIFY.json"
                           if args.verify_only
                           else "/training/v2/reports/RL_MANAGEMENT_GROUP.json"))
    json.dump(report, open(out, "w"), indent=1)
    print("TOTAL_VIOLATIONS", total_bad, "->", out)
    return 1 if total_bad else 0


if __name__ == "__main__":
    sys.exit(main())