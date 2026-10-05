"""Supervise the three RL target builds so an 8-day wait cannot silently lose one.

THE HOLE THIS CLOSES: rl-targets-v5 and rl-walleval-v5 are transient systemd units.
Nothing restarts them. If one dies at 60% the file still exists and is far past the
gate_min_episodes floor, so the RL chain's stage0 would pass. rl_chain_trigger.py now
refuses on an exact expectation (EXPECTED.json), which turns that into a STOP - but a
stop eight days later is a wasted wait. This restarts the dead stage instead.

RESUME-NOT-RESTART: grpo_dataset.build_split skips prompts already present in the
output, so re-running CONTINUES a partial build rather than redoing it. The stage is
only relaunched when its producer is genuinely NOT running (pgrep), so a live build is
never double-written.

    python rl_builds_watchdog.py [--apply]
"""
import json
import os
import subprocess
import sys
import time

PY = "/home/alon/qwen27b-venv/bin/python"
RL = "/training/v2/code/src/v2/rl"
OUT = os.environ.get("RL_BUILDS_OUT", "/training/v2/rl_targets_v5")
EXPECTED = os.path.join(OUT, "EXPECTED.json")
REPORT = "/training/v2/reports/RL_BUILDS_WATCHDOG.json"
LOG = "/training/v2/reports/rl_builds_watchdog.log"


def nrows(path):
    if not os.path.isfile(path):
        return 0
    n = 0
    with open(path, "rb") as fh:
        for _ in fh:
            n += 1
    return n


def log(msg, **kw):
    rec = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "msg": msg, **kw}
    print(json.dumps(rec), flush=True)
    try:
        with open(LOG, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(rec) + "\n")
    except Exception:                                   # noqa: BLE001
        pass


def running(sig):
    r = subprocess.run(["pgrep", "-f", sig], capture_output=True, text=True)
    pids = [p for p in r.stdout.split() if p.strip()]
    # pgrep -f also matches this script's own argv when it names the signature
    pids = [p for p in pids if p != str(os.getpid())]
    return pids


def stages():
    tr = os.path.join(OUT, "rl_train.jsonl")
    va = os.path.join(OUT, "rl_validation.jsonl")
    we = os.path.join(OUT, "rl_wall_eval.jsonl")
    ws = os.path.join(OUT, "rl_wall_scored.jsonl")
    return [
        ("rl_train.jsonl", tr, "grpo_dataset.py --split train",
         [PY, os.path.join(RL, "grpo_dataset.py"), "--split", "train",
          "--with-management", "--out", tr]),
        ("rl_validation.jsonl", va, "grpo_dataset.py --split validation",
         [PY, os.path.join(RL, "grpo_dataset.py"), "--split", "validation",
          "--with-management", "--out", va]),
        ("rl_wall_eval.jsonl", we, "build_sft_c6.py",
         [PY, os.path.join(RL, "build_sft_c6.py"), "--wall-eval-out", we]),
        ("rl_wall_scored.jsonl", ws, "score_wall_eval.py",
         [PY, os.path.join(RL, "score_wall_eval.py"), "--in", we, "--out", ws]),
    ]


def complete(name, n, rule):
    if "exact" in rule:
        return n == int(rule["exact"])
    if "min_frac" in rule:
        return n >= int(float(rule["min_frac"]) * int(rule["of"]))
    return n > 0


def notify(text):
    """Best-effort Telegram. A silent rebuild would defeat the point of this script."""
    try:
        tokfile = "/home/alon/.hermes/.env"
        tok = ""
        for line in open(tokfile, encoding="utf-8"):
            if line.startswith("TELEGRAM_BOT_TOKEN="):
                tok = line.split("=", 1)[1].strip()
                break
        if not tok:
            return False
        subprocess.run(["curl", "-s", "-m", "20", "-X", "POST",
                        f"https://api.telegram.org/bot{tok}/sendMessage",
                        "--data-urlencode", "chat_id=" +
                        os.environ.get("WATCHDOG_CHAT_ID", "5024153101"),
                        "--data-urlencode", "text=" + text],
                       capture_output=True, text=True)
        return True
    except Exception:                                   # noqa: BLE001
        return False


def main(argv=None):
    apply_it = "--apply" in (argv or sys.argv[1:])
    rules = (json.load(open(EXPECTED, encoding="utf-8")) or {}).get("expected_rows", {})
    rep = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "apply": apply_it, "stages": {}}
    for name, path, sig, cmd in stages():
        n = nrows(path)
        rule = rules.get(name, {})
        # COMPLETENESS = producer has EXITED *and* the row expectation is met. A row
        # floor that passes while the producer is still writing is NOT done: it is a
        # partial-but-large file, the exact defect this module exists to prevent.
        # Never declare complete while the producer process is alive.
        pids = running(sig)
        done = (not pids) and complete(name, n, rule)
        entry = {"rows": n, "expected": rule, "complete": done,
                 "producer_pids": len(pids)}
        if done:
            entry["action"] = "none (complete)"
        elif pids:
            entry["action"] = "none (build in progress)"
        else:
            entry["action"] = "RELAUNCH (producer not running and output incomplete)"
            if apply_it:
                unit = "rl-rebuild-" + name.replace(".jsonl", "").replace("_", "-")
                wrap = ["sudo", "systemd-run", "--unit=" + unit, "--collect",
                        "--property=MemoryMax=160G", "--property=Nice=5",
                        "--property=StandardOutput=append:" + LOG,
                        "--property=StandardError=append:" + LOG,
                        "--working-directory=" + RL, "--"] + cmd
                r = subprocess.run(wrap, capture_output=True, text=True)
                entry["relaunch_rc"] = r.returncode
                entry["relaunch_out"] = (r.stdout or r.stderr or "")[-200:]
                log("relaunched", stage=name, rc=r.returncode, rows=n)
        rep["stages"][name] = entry
        log("stage", stage=name, **{k: entry[k] for k in
                                    ("rows", "complete", "producer_pids", "action")})
    rep["needs_attention"] = [k for k, v in rep["stages"].items()
                              if v["action"].startswith("RELAUNCH")]
    if rep["needs_attention"] and apply_it:
        det = "; ".join("%s=%d rows" % (k, rep["stages"][k]["rows"])
                        for k in rep["needs_attention"])
        notify("RL target build had died and was restarted (resume-not-restart): %s"
               % det)
    os.makedirs(os.path.dirname(REPORT), exist_ok=True)
    json.dump(rep, open(REPORT, "w", encoding="utf-8"), indent=1)
    print(json.dumps({"needs_attention": rep["needs_attention"],
                      "apply": apply_it}, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
