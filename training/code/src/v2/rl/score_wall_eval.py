"""score_wall_eval — attach counterfactual groups to the annotated wall set.

WHY THIS EXISTS
---------------
The forward wall (D4) is the RL eval set and is NEVER trained on. Two builders
produce it, and they must not be conflated:

  1. build_sft_c6.py --wall-eval-out   -> the ANNOTATED PROMPTS, in exactly the
     training format (same market block, same unit names, same staleness_ms and
     pricing_eligible fields). Verified: the corpus splits stay byte-identical
     when this runs, so the eval cannot drift from the training distribution.
  2. this script -> the SCORED rows: the 3-arm counterfactual group
     (SKIP/WATCH/BUY) plus reserve/staleness provenance that grpo_loop's
     load_wall_eval() requires before it will apply the gate.

Without step 2 the driver refuses at pre-flight, by design: it will not
fabricate a prompt or a return.

Reuses grpo_dataset.build_records - the ONE scoring path - so a wall record and
a training record are scored by identical code.
"""
import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from grpo_dataset import (ACTIONS, build_records,                  # noqa: E402
                          prompt_of, prompt_sha, wall_state)
from rl_reward_v3 import (HORIZON_MS_DEFAULT, ReserveRegistry,     # noqa: E402
                          STALE_ANY, V3Engine, load_canonical_tapes)

TAPES = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--in", dest="src", required=True,
                    help="annotated wall prompts from build_sft_c6 --wall-eval-out")
    ap.add_argument("--out", required=True)
    ap.add_argument("--horizon-ms", type=int, default=HORIZON_MS_DEFAULT)
    ap.add_argument("--max-records", type=int, default=0)
    a = ap.parse_args(argv)

    w = wall_state()
    wall_ms = int(w["wall_ms"])
    if not os.path.isfile(a.src):
        raise SystemExit(f"REFUSING: annotated wall set not found: {a.src}")
    os.makedirs(os.path.dirname(a.out), exist_ok=True)

    # resume: same key the corpus builder uses, so a restart never double-writes
    done = set()
    if os.path.isfile(a.out):
        for line in open(a.out, encoding="utf-8"):
            try:
                done.add(json.loads(line)["prompt_sha256"])
            except Exception:                                       # noqa: BLE001
                pass

    reg = ReserveRegistry.build(verbose=True)
    eng = V3Engine(reg, max_reserve_stale_ms=STALE_ANY, deploy_sol=1.0)
    tapes = load_canonical_tapes(TAPES, min_notional_lamports=100_000,
                                 max_px_ratio=50)

    st = {"src": a.src, "out": a.out, "wall_ms": wall_ms, "scanned": 0,
          "decision_rows": 0, "not_decision": 0, "skipped_done": 0,
          "skipped_no_tape": 0, "skipped_outside_tape_window": 0,
          "scored": 0, "none": 0, "refusals": {}, "leak_refused": 0}
    cand = []
    with open(a.src, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            st["scanned"] += 1
            try:
                rec = json.loads(line)
            except Exception:                                       # noqa: BLE001
                continue
            if rec.get("family") != "decision":
                st["not_decision"] += 1
                continue
            st["decision_rows"] += 1
            eid = rec.get("episode_id") or ""
            parts = eid.split(":")
            if len(parts) < 3:
                continue
            try:
                t_dec = int(parts[2])
            except ValueError:
                continue
            # THE EVAL SET MUST BE THE WALL. A row from before wall_ms here would
            # silently turn the gate into a measurement on trained data.
            if t_dec < wall_ms:
                st["leak_refused"] += 1
                continue
            cand.append((t_dec, parts[1], rec))
    cand.sort(key=lambda x: x[1])                     # mint-stratified, as corpus

    n_written = 0
    with open(a.out, "a", encoding="utf-8") as fo:
        for t_dec, mint, rec in cand:
            if a.max_records and n_written >= a.max_records:
                break
            pr = prompt_of(rec)
            psha = prompt_sha(pr)
            if psha in done:
                st["skipped_done"] += 1
                continue
            tape = tapes.get(mint)
            if tape is None:
                st["skipped_no_tape"] += 1
                continue
            if not (int(tape.tt[0]) <= t_dec <= int(tape.tt[-1])):
                st["skipped_outside_tape_window"] += 1
                continue
            try:
                out = build_records(rec, tape, eng, a.horizon_ms)
            except Exception as e:                                   # noqa: BLE001
                k = type(e).__name__
                st["refusals"][k] = st["refusals"].get(k, 0) + 1
                continue
            if out is None:
                st["none"] += 1
                continue
            if out.get("__none__"):
                k = out["statuses"][0] if out.get("statuses") else "no_arm"
                st["refusals"][k] = st["refusals"].get(k, 0) + 1
                if st["scored"] == 0 and "first_none_reason" not in st:
                    st["first_none_reason"] = out.get("reason")
                continue
            out["wall_eval"] = True
            out["never_trained_on"] = True
            out["wall_ms"] = wall_ms
            fo.write(json.dumps(out) + "\n")
            fo.flush()
            n_written += 1
            st["scored"] += 1
            if n_written % 200 == 0:
                print(json.dumps({"scored": n_written}), flush=True)

    st["written_this_run"] = n_written
    st["total_lines"] = sum(1 for _ in open(a.out, encoding="utf-8"))
    # RESUME-AWARE VERDICT: what matters is how many scored rows EXIST, not how
    # many this invocation appended. A resume over a complete file writes 0 new
    # rows — that is success, not failure. (The old `scored >= 2000` rule made
    # every resume of a complete build exit 1, and the builds watchdog then
    # relaunched a finished build hourly, forever.)
    done_total = st["total_lines"]
    st["verdict"] = ("PASS" if done_total >= 2000
                     else f"INSUFFICIENT: {done_total} < gate_min_episodes 2000")
    with open("/training/v2/reports/RL_WALL_EVAL_SCORE.json", "w",
              encoding="utf-8") as fh:
        json.dump(st, fh, indent=1)
    print(json.dumps(st, indent=1), flush=True)
    return 0 if done_total >= 2000 else 1


if __name__ == "__main__":
    t0 = time.time()
    rc = main()
    print("elapsed %.1fs" % (time.time() - t0), flush=True)
    raise SystemExit(rc)
