#!/usr/bin/env python
"""CVaR carry-through no-op gate: the v2 diagnostics must move NO existing number.

WHY this exists. `group_loss_weight` / `cvar` / `schema_version` are the only sanctioned
additions of the lambda_cvar carry-through, and lambda_cvar is pinned 0.0 (OFF) today
(operator decision: it is a later A/B arm). Therefore a dataset emitted by the v2 build
must be VALUE-IDENTICAL to the v1 build apart from the carried fields themselves. This
gate is that claim, executable, rather than an assertion in a commit message.

SAME STYLE AS rl_v7_regression_gate.py - deliberately, so there is one regression idiom
in this directory: two JSONL files, one record per line, same order, compared AFTER
removing named fields. The named fields here are EXACTLY the v2 additions; nothing else
is excused, so any accidental movement in `advantages`, `arm_values`, `group`, `chosen`
or the prompts FAILS.

TEETH. A gate that only excused fields would also pass on a file that never carried them
(vacuous). So this one FAILS when the new file does not actually carry the v2 fields:
the check has to see the thing it is excusing.

Usage: python rl_cvar_noop_gate.py <baseline_v1.jsonl> <new_v2.jsonl>
       (exit 0 = PASS, 1 = FAIL)
"""
import json
import sys

IGNORED = ("group_loss_weight", "cvar", "schema_version")


def strip(obj, seen):
    """Drop the v2 fields at any depth and count how many were actually present."""
    if isinstance(obj, dict):
        for k in IGNORED:
            if k in obj:
                seen[k] = seen.get(k, 0) + 1
        return {k: strip(v, seen) for k, v in obj.items() if k not in IGNORED}
    if isinstance(obj, list):
        return [strip(x, seen) for x in obj]
    return obj


def main():
    if len(sys.argv) != 3:
        print(json.dumps({"verdict": "FAIL", "reason": "usage: <baseline.jsonl> "
                                                       "<new.jsonl>"}))
        return 1
    a_path, b_path = sys.argv[1], sys.argv[2]
    seen = {}
    n = mism = 0
    with open(a_path) as A, open(b_path) as B:
        for la in A:
            lb = B.readline()
            if not lb:
                print(json.dumps({"verdict": "FAIL", "reason": "new file shorter",
                                  "at_row": n}))
                return 1
            n += 1
            sa = strip(json.loads(la), {})
            sb = strip(json.loads(lb), seen)
            if sa != sb:
                mism += 1
                if mism <= 3:
                    ka, kb = set(sa), set(sb)
                    print(f"MISMATCH row {n}: only_baseline={sorted(map(str, ka - kb))} "
                          f"only_new={sorted(map(str, kb - ka))}")
        if B.readline():
            print(json.dumps({"verdict": "FAIL", "reason": "new file longer"}))
            return 1
    carry = {k: seen.get(k, 0) for k in IGNORED}
    missing = [k for k in IGNORED if not carry[k]]
    v = "PASS" if (mism == 0 and not missing and n > 0) else "FAIL"
    print(json.dumps({"verdict": v, "rows": n, "mismatches": mism,
                      "ignored_fields_present": carry,
                      "v2_fields_absent_from_new_file": missing}))
    return 0 if v == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())