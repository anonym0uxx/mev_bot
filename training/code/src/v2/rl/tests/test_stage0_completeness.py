"""Test the stage-0 completeness gate: missing pin / RUNNING pin / mismatch / COMPLETE,
in BOTH states (a builder alive -> wait/3, nothing building -> refuse/2).

Monkeypatches builders_running so the test does not depend on whether the real v9 build
happens to be running.
"""
import json
import os
import shutil
import sys
import tempfile

sys.path.insert(0, "/training/v2/code/src/v2/rl")
import rl_chain_trigger as T

fails = []


def chk(tag, cond, extra=""):
    if not cond:
        fails.append("%s %s" % (tag, extra))
    print("  %-42s %s" % (tag, "ok" if cond else "FAIL " + str(extra)))


def run_case(d, running, label):
    """Return (rc, msgs) with rc None meaning it returned normally (stage0 PASS)."""
    T.RL_TRAIN = os.path.join(d, "rl_train.jsonl")
    T.WALL_EVAL = os.path.join(d, "rl_wall_scored.jsonl")
    T.builders_running = lambda: list(running)
    msgs = []
    T.log = lambda m, **kw: msgs.append(m)
    try:
        out = T.stage0_precheck()
        return None, out, msgs
    except SystemExit as e:
        return int(e.code), None, msgs


root = tempfile.mkdtemp(prefix="gate_")
FLOOR = int(json.load(open(T.CFG, encoding="utf-8"))["gate"]["gate_min_episodes"])
for name, n in (("rl_train.jsonl", FLOOR), ("rl_wall_scored.jsonl", FLOOR)):
    with open(os.path.join(root, name), "w") as f:
        for i in range(n):
            f.write("{}\n")

# 1. no pin at all
rc, _o, _m = run_case(root, [], "no pin / nothing building")
chk("no_pin_refuses", rc == 2, "rc=%s" % rc)
rc, _o, _m = run_case(root, ["123"], "no pin / builder alive")
chk("no_pin_waits_while_building", rc == 3, "rc=%s" % rc)

# 2. pin says RUNNING (a build that started and never finished)
pin = os.path.join(root, "EXPECTED.json")
json.dump({"schema": "rl_targets_expectation/2", "status": "RUNNING",
           "expected_rows": {"rl_train.jsonl": {"exact": FLOOR},
                             "rl_wall_scored.jsonl": {"exact": FLOOR}}}, open(pin, "w"))
rc, _o, _m = run_case(root, [], "RUNNING / nothing building")
chk("running_pin_refuses", rc == 2, "rc=%s" % rc)
rc, _o, _m = run_case(root, ["123"], "RUNNING / builder alive")
chk("running_pin_waits_while_building", rc == 3, "rc=%s" % rc)

# 3. COMPLETE but a count does not match (partial file / died mid-flight)
json.dump({"status": "COMPLETE", "expected_rows": {"rl_train.jsonl": {"exact": FLOOR + 1},
                                                  "rl_wall_scored.jsonl": {"exact": FLOOR}}},
          open(pin, "w"))
rc, _o, _m = run_case(root, [], "count mismatch / nothing building")
chk("mismatch_refuses", rc == 2, "rc=%s" % rc)
rc, _o, _m = run_case(root, ["123"], "count mismatch / builder alive")
chk("mismatch_waits_while_building", rc == 3, "rc=%s" % rc)

# 4. COMPLETE and exact -> stage0 PASSES
json.dump({"status": "COMPLETE", "expected_rows": {"rl_train.jsonl": {"exact": FLOOR},
                                                   "rl_wall_scored.jsonl": {"exact": FLOOR}}},
          open(pin, "w"))
rc, out, _m = run_case(root, [], "COMPLETE + exact")
chk("complete_passes", rc is None and out is not None, "rc=%s out=%s" % (rc, out))

# 5. LEGACY pin (v8 shape: no status field, exact counts) must still pass
json.dump({"expected_rows": {"rl_train.jsonl": {"exact": FLOOR},
                             "rl_wall_scored.jsonl": {"exact": FLOOR}}}, open(pin, "w"))
rc, out, _m = run_case(root, [], "legacy pin (no status)")
chk("legacy_pin_still_passes", rc is None and out is not None, "rc=%s" % rc)

# 6. a NAMED file (not one of the two gate inputs) missing from disk
json.dump({"status": "COMPLETE",
           "expected_rows": {"rl_train.jsonl": {"exact": FLOOR},
                             "rl_wall_scored.jsonl": {"exact": FLOOR},
                             "rl_validation.jsonl": {"exact": 7}}}, open(pin, "w"))
rc, _o, _m = run_case(root, [], "named file missing / nothing building")
chk("missing_named_file_refuses", rc == 2, "rc=%s" % rc)
rc, _o, _m = run_case(root, ["123"], "named file missing / builder alive")
chk("missing_named_file_waits_while_building", rc == 3, "rc=%s" % rc)

# 7. a GATE INPUT missing (the pre-loop's own check): same wait-vs-page rule
os.remove(os.path.join(root, "rl_wall_scored.jsonl"))
rc, _o, _m = run_case(root, [], "gate input missing / nothing building")
chk("missing_input_refuses", rc == 2, "rc=%s" % rc)
rc, _o, _m = run_case(root, ["123"], "gate input missing / builder alive")
chk("missing_input_waits_while_building", rc == 3, "rc=%s" % rc)

# 8. a pin that does not COVER the gate's own inputs is not a completeness proof
os_ok = os.path.join(root, "rl_wall_scored.jsonl")
if not os.path.isfile(os_ok):
    open(os_ok, "w").write("{}\n" * FLOOR)
json.dump({"status": "COMPLETE",
           "expected_rows": {"rl_validation.jsonl": {"exact": 7}}}, open(pin, "w"))
rc, _o, _m = run_case(root, [], "pin omits the gate inputs / nothing building")
chk("uncovered_pin_refuses", rc == 2, "rc=%s" % rc)
rc, _o, _m = run_case(root, ["123"], "pin omits the gate inputs / builder alive")
chk("uncovered_pin_waits_while_building", rc == 3, "rc=%s" % rc)

shutil.rmtree(root, ignore_errors=True)
print(json.dumps({"suite": "stage0_completeness_gate", "failed": len(fails),
                  "failures": fails}, indent=1))
sys.exit(1 if fails else 0)