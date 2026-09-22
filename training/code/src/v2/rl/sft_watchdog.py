"""sft_watchdog — hourly reliability engineer for the SFT-006 run.

Design rules (these are the point of the script; a watchdog that guesses is worse
than none):
  * DETECT from evidence only: unit state, log growth, step counters, checkpoint
    presence, GPU/CPU/disk pressure. No inference from hope.
  * REMEDIATE ONLY WHEN A CHECKPOINT EXISTS: resume-not-restart. A crash before the
    first save is restarted from the original init_from, never from a checkpoint
    that does not exist.
  * NEVER delete or overwrite a checkpoint. Never launch a second trainer.
  * CAP the blast radius: at most one remediation per hour and at most 3
    consecutive remediations before it escalates and STOPS acting.
  * ESCALATE with a clear reason instead of acting when the situation is not one
    of the defined, safe repairs.
  * Once final/ exists the run is COMPLETE: the watchdog stops acting so it cannot
    interfere with the RL chain trigger that is waiting on the same artifact.

Exit codes: 0 = healthy/completed, 2 = needs human attention.
stdout is the hourly report (injected into the cron agent's prompt).
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
import time

UNIT = os.environ.get("SFT_WATCHDOG_UNIT", "qwen-sft-006")
RUN_ROOT = os.environ.get("SFT_WATCHDOG_RUN_ROOT", "/training/runs/sft-006")
OUT_DIR = RUN_ROOT + "/astra_north_star_v3_linux_sft"
FINAL = OUT_DIR + "/final"
LOG = RUN_ROOT + "/train.log"
TRAINER = "/training/code/qwen27b/train_qwen27b.py"
# The relaunch MUST use the venv that has torch/accelerate/bitsandbytes. Using
# sys.executable would inherit whatever interpreter the cron happened to invoke,
# and a repair that launches a python without torch is worse than no repair.
PY = "/home/alon/qwen27b-venv/bin/python"
ACCEL = [PY, "-B", "-m", "accelerate.commands.launch",
         "--num_processes", "3", "--num_machines", "1", "--machine_rank", "0"]
RELEASE = "/training/rel/CANDIDATE_RELEASE_LINUX.json"
CONTRACT = os.environ.get("SFT_WATCHDOG_CONTRACT", "/training/code/qwen27b/LAUNCH_CONTRACT_SFT_V4.json")
ACCEPTANCE = os.environ.get("SFT_WATCHDOG_ACCEPTANCE", "/training/rel/ACCEPTANCE_SFT_006.json")
INIT_FROM = "/training/runs/cpt-001/cpt_final_bf16_hf"
STATE = os.environ.get("SFT_WATCHDOG_STATE", "/training/v2/reports/sft_watchdog_state.json")
DIAG = os.environ.get("SFT_WATCHDOG_DIAG", "/training/v2/reports/sft_diagnostics")
REPORT = os.environ.get("SFT_WATCHDOG_REPORT", "/training/v2/reports/SFT_WATCHDOG.json")

STALL_MIN = int(os.environ.get("SFT_WATCHDOG_STALL_MIN", "25"))  # no progress => STALLED
COOLDOWN_S = 3600         # at most one remediation per hour
MAX_CONSEC = 3            # then escalate and stop acting
# Cosmetic (% complete in the report). Not a gate: the trainer's own step plan is
# authoritative and this is only what the report divides by.
TOTAL_STEPS = int(os.environ.get("SFT_WATCHDOG_TOTAL_STEPS", "8999"))


def sh(cmd, timeout=60):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout.strip(), r.stderr.strip()
    except Exception as e:                                   # noqa: BLE001
        return 1, "", str(e)


def unit_active():
    _rc, out, _e = sh(["systemctl", "is-active", UNIT])
    if out in ("active", "activating", "reloading"):
        return True
    # The trainer may run as a USER-manager unit/scope (e.g. launched via
    # `systemd-run --user --scope`), which the system manager reports as
    # "inactive". Checking only the system bus produced a false CRASHED on a
    # healthy run (SFT-013, 2026-09-20). Probe the cgroup tree directly: it
    # needs no dbus session env and works from a system-timer context.
    import glob
    for suffix in (".scope", ".service"):
        pat = "/sys/fs/cgroup/user.slice/*/user@*.service/*/" + UNIT + suffix
        for d in glob.glob(pat) + glob.glob(pat.replace("/*/" + UNIT, "/**/" + UNIT)):
            procs = os.path.join(d, "cgroup.procs")
            try:
                with open(procs) as f:
                    if f.read().strip():
                        return True
            except OSError:
                continue
    return False


def sha256_file(p, chunk=1 << 20):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for c in iter(lambda: f.read(chunk), b""):
            h.update(c)
    return h.hexdigest()


def last_step():
    """Latest 'N/8999' counter and its wall-clock age, read off the log."""
    if not os.path.isfile(LOG):
        return None, None
    mtime = os.path.getmtime(LOG)
    last = None
    with open(LOG, "rb") as f:
        try:
            f.seek(max(0, os.path.getsize(LOG) - 200000))
        except Exception:
            pass
        blob = f.read().decode("utf-8", "replace")
    blob = blob.replace("\r", "\n")
    for m in re.finditer(r"(\d+)/(\d+) \[", blob):
        last = int(m.group(1))
    return last, mtime


def tfevents_mtime():
    """Newest TensorBoard event file mtime under the run's output dir.

    WHY THIS EXISTS: the trainer's stdout/stderr is redirected to a file, so Python
    block-buffers it. tqdm writes '\\r'-terminated bars that never trigger a line
    flush, so train.log can sit unchanged for 1-2 h on a perfectly HEALTHY run while
    the tb writer appends a record every logging step. A watchdog that treats log
    mtime as the heartbeat declares a false STALLED on a healthy run and trains the
    operator to ignore it. mtime of the events file is a real trainer heartbeat.
    """
    runs = os.path.join(OUT_DIR, "runs")
    best = 0.0
    if not os.path.isdir(runs):
        return None
    for d in os.listdir(runs):
        sub = os.path.join(runs, d)
        if not os.path.isdir(sub):
            continue
        for f in os.listdir(sub):
            if "tfevents" in f:
                try:
                    best = max(best, os.path.getmtime(os.path.join(sub, f)))
                except OSError:
                    pass
    return best or None


def newest_checkpoint():
    if not os.path.isdir(OUT_DIR):
        return None
    cks = []
    for d in os.listdir(OUT_DIR):
        if d.startswith("checkpoint-"):
            try:
                cks.append((int(d.split("-")[1]), os.path.join(OUT_DIR, d)))
            except ValueError:
                continue
    return max(cks)[1] if cks else None


def checkpoint_step(ckpt):
    """The newest checkpoint's own `global_step` — the OPTIMIZER step, not a bar position.

    WHY THIS EXISTS. `last_step()` reads the newest `N/M` pair in the log, which during an
    evaluation is the EVAL loop's counter (one pass over the validation set, ~6k iterations),
    not the training step. Reporting that beside `total_steps=8889` reads as "training is
    stuck" on a perfectly healthy run, and it did exactly that during the 1,777-step eval of
    sft-013. The checkpoint's `trainer_state.json` carries the number that cannot be faked.
    """
    if not ckpt:
        return None
    p = os.path.join(ckpt, "trainer_state.json")
    try:
        with open(p, encoding="utf-8") as fh:
            d = json.load(fh)
        return {"global_step": d.get("global_step"), "epoch": d.get("epoch"),
                "best_metric": d.get("best_metric")}
    except Exception as exc:  # a missing/incomplete state file is evidence, not a crash
        return {"error": str(exc)[:120]}


def gpu_state():
    _rc, out, _e = sh(["nvidia-smi", "--query-gpu=index,memory.used,utilization.gpu",
                       "--format=csv,noheader"])
    return out


def ram_free_gb():
    try:
        with open("/proc/meminfo") as f:
            d = dict(l.split(":", 1) for l in f if ":" in l)
        return round(int(d["MemAvailable"].split()[0]) / 1048576, 1)
    except Exception:
        return None


def disk_free_gb(path="/training"):
    _rc, out, _e = sh(["df", "-BG", "--output=avail", path])
    try:
        return int(out.splitlines()[-1].strip().rstrip("G"))
    except Exception:
        return None


def capture_diagnostics(tag):
    os.makedirs(DIAG, exist_ok=True)
    stamp = time.strftime("%Y%m%dT%H%M%S")
    base = os.path.join(DIAG, f"{tag}_{stamp}")
    _rc, pids, _e = sh(["pgrep", "-f", "train_qwen27b.py"])
    d = {"at": stamp, "pids": pids.split(), "gpu": gpu_state(),
         "ram_free_gb": ram_free_gb(), "disk_free_gb": disk_free_gb()}
    for pid in d["pids"][:3]:
        _rc, dump, _e = sh(["py-spy", "dump", "--pid", pid], timeout=90)
        d[f"pyspy_{pid}"] = dump[-4000:]
    _rc, tail, _e = sh(["bash", "-lc", f"tail -c 3000 {LOG}"])
    d["log_tail"] = tail
    with open(base + ".json", "w", encoding="utf-8") as fh:
        json.dump(d, fh, indent=1)
    return base + ".json"


def load_state():
    try:
        return json.load(open(STATE, encoding="utf-8"))
    except Exception:
        return {"last_change_ts": 0, "last_step": 0, "remediations": [],
                "consecutive": 0}


def save_state(s):
    os.makedirs(os.path.dirname(STATE), exist_ok=True)
    json.dump(s, open(STATE, "w", encoding="utf-8"), indent=1)


def relaunch(resume_ckpt):
    """Relaunch the SAME run: same contract, same sha chain, same acceptance."""
    csha = sha256_file(CONTRACT)
    asha = sha256_file(ACCEPTANCE)
    cmd = ["sudo", "systemd-run", "--unit=" + UNIT, "--collect",
           "--property=StandardOutput=append:" + LOG,
           "--property=StandardError=append:" + LOG,
           "--setenv=QWEN_RELEASE_ACCEPTANCE_SHA256=" + asha,
           "--working-directory=/training/code/qwen27b",
           *ACCEL, TRAINER, "--phase", "sft",
           "--release_manifest", RELEASE,
           "--init_from", INIT_FROM,
           "--out_root", RUN_ROOT,
           "--launch_contract", CONTRACT,
           "--contract_sha256", csha]
    if resume_ckpt:
        cmd += ["--resume_from_checkpoint", resume_ckpt]
    _rc, out, err = sh(cmd, timeout=120)
    return {"rc": _rc, "resume_from": resume_ckpt, "out": out[-300:],
            "err": err[-300:], "contract_sha256": csha}


def main(argv=None):
    import argparse
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--remediate", action="store_true",
                    help="apply the resume-repair AFTER a human/agent assessment; "
                         "without this the script only detects and gathers evidence")
    ap.add_argument("--reason", default="",
                    help="the assessment that justifies remediation (recorded)")
    ap.add_argument("--diagnostics", action="store_true",
                    help="capture py-spy/gpu/log-tail evidence even when healthy")
    ap.add_argument("--force", action="store_true",
                    help="bypass the hourly cooldown (still capped by MAX_CONSEC)")
    a = ap.parse_args(argv)
    now = time.time()
    st = load_state()
    step, log_mtime = last_step()
    active = unit_active()
    ckpt = newest_checkpoint()
    tb_mtime = tfevents_mtime()
    hb_mtime = max([m for m in (log_mtime, tb_mtime,
                                os.path.getmtime(ckpt) if ckpt else None) if m] or [0])
    done = os.path.isfile(FINAL) and any(
        f.endswith((".safetensors", ".bin")) for f in os.listdir(FINAL)
    ) if os.path.isdir(FINAL) else False

    rep = {"at": time.strftime("%Y-%m-%dT%H:%M:%S"),
           "unit_active": active, "step": step, "total_steps": TOTAL_STEPS,
           "checkpoint_step": checkpoint_step(ckpt),
           "newest_checkpoint": ckpt, "final_present": done,
           "gpu": gpu_state(), "ram_free_gb": ram_free_gb(),
           "disk_free_gb": disk_free_gb(), "consecutive": st.get("consecutive", 0)}

    # progress bookkeeping
    if step is not None and step > (st.get("last_step") or 0):
        st["last_step"] = step
    if hb_mtime > (st.get("last_hb_mtime") or 0):
        st["last_hb_mtime"] = hb_mtime
        st["last_change_ts"] = now
    idle_min = (now - max(st.get("last_change_ts") or now, hb_mtime)) / 60.0
    rep["idle_minutes"] = round(idle_min, 1)
    rep["tfevents_mtime"] = tb_mtime
    rep["heartbeat_age_min"] = (round((now - hb_mtime) / 60.0, 1) if hb_mtime else None)

    # ---------------- classify
    if done:
        rep["status"] = "COMPLETE"
        rep["action"] = "none (final/ present; RL chain trigger owns the handoff)"
        st["consecutive"] = 0
    elif active and idle_min <= STALL_MIN:
        rep["status"] = "HEALTHY"
        rep["action"] = "none"
        st["consecutive"] = 0
        if a.diagnostics:
            rep["diagnostics"] = capture_diagnostics("healthy")
    elif active and idle_min > STALL_MIN:
        rep["status"] = "STALLED"
        rep["action"] = "remediate(hang)"
        st["consecutive"] = st.get("consecutive", 0) + 1
    elif not active:
        rep["status"] = "CRASHED"
        rep["action"] = "remediate(crash)"
        st["consecutive"] = st.get("consecutive", 0) + 1
    else:
        rep["status"] = "UNKNOWN"
        rep["action"] = "escalate"

    # ---------------- act
    # ASSESSMENT FIRST. This script never repairs on its own: the monitoring agent
    # reads the evidence below, decides what the fault actually is, and only then
    # calls back with --remediate (recording its reasoning). A blanket restart hides
    # the root cause and can destroy the only checkpoint, so it is not the default.
    if rep["status"] in ("STALLED", "CRASHED"):
        if a.diagnostics or rep["status"] == "CRASHED":
            rep["diagnostics"] = capture_diagnostics(rep["status"].lower())
        last_rem = st.get("last_remediation_ts") or 0
        rep["cooldown_remaining_min"] = max(0, int((COOLDOWN_S - (now - last_rem)) / 60))
        if not a.remediate:
            rep["action"] = ("ASSESS REQUIRED: %s detected - evidence captured, no "
                             "automatic action taken" % rep["status"])
        elif st.get("consecutive", 0) > MAX_CONSEC:
            rep["action"] = ("ESCALATED: %d consecutive remediations without recovery "
                             "- refusing further automatic action" % st["consecutive"])
            rep["escalate"] = True
        elif not a.force and now - last_rem < COOLDOWN_S:
            rep["action"] = ("deferred: within the %ds cooldown (last remediation %d "
                             "min ago)" % (COOLDOWN_S, int((now - last_rem) / 60)))
        else:
            rep["assessment_reason"] = a.reason
            rep["diagnostics"] = rep.get("diagnostics") or capture_diagnostics(
                rep["status"].lower())
            sh(["sudo", "systemctl", "stop", UNIT], timeout=120)
            time.sleep(5)
            rep["remediation"] = relaunch(ckpt)
            st["last_remediation_ts"] = now
            st["remediations"] = (st.get("remediations") or [])[-9:] + [
                {"at": rep["at"], "status": rep["status"], "step": step,
                 "resume_from": ckpt, "reason": a.reason[:300],
                 "rc": rep["remediation"]["rc"]}]
            if rep["remediation"]["rc"] != 0:
                rep["escalate"] = True
                rep["action"] = ("REMEDIATION FAILED to relaunch - ESCALATE "
                                 "(resume_from=%s)" % ckpt)
            else:
                rep["action"] = ("remediated: relaunched (resume_from=%s) - reason: %s"
                                 % (ckpt or "init_from", a.reason[:200]))

    save_state(st)
    json.dump(rep, open(REPORT, "w", encoding="utf-8"), indent=1)

    # ---------------- report (stdout goes into the cron agent's prompt)
    print(json.dumps(rep, indent=1))
    if rep.get("escalate"):
        print("NEEDS HUMAN ATTENTION: %s" % rep["action"])
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())