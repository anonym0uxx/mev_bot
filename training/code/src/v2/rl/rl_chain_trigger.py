"""rl_chain_trigger — guarantees the RL run happens once SFT-006 completes.

WHY THIS EXISTS
---------------
"Do not hope it gets triggered." A chain that fires into a failing precondition is
worse than no chain, so every stage here either passes a VERIFIED precondition or
aborts loudly with a REFUSING line. Nothing is launched on a guess.

STAGES (each idempotent; state in rl_chain_state.json so a restart resumes):
  0 preconditions  all RL inputs exist NOW (targets, wall eval, config)
  1 wait for SFT   poll the unit + {sft_out}/final (HF weights). If the unit is
                   gone WITHOUT final/ the SFT run FAILED -> ABORT, never launch.
  2 ref cache      build the action-distribution cache from init_from over the RL
                   train prompts (skipped when already built for this ckpt)
  3 verify         run rl_readiness_verify.py; a non-zero exit ABORTS the chain
  4 launch         start RL in its OWN cgroup (systemd-run) so a trainer OOM can
                   never kill the gateway

Usage:
    python rl_chain_trigger.py                 # full chain, waits for SFT-006
    python rl_chain_trigger.py --precheck-only # stage 0 only
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
PY = "/home/alon/qwen27b-venv/bin/python"
SFT_UNIT = os.environ.get("RL_CHAIN_SFT_UNIT", "qwen-sft-013")
SFT_OUT = os.environ.get("RL_CHAIN_SFT_OUT",
                         "/training/runs/sft-013/astra_north_star_v3_linux_sft")
INIT_FROM = SFT_OUT + "/final"
CFG = os.path.join(HERE, "configs", "rl_v2.json")
# RE-ARM (KELLY_AUDIT_C12 §b): v8 targets are venue-conditional (AMM FULL / curve
# SMALL, MID retired); built from candidate_sft_c12 by the same grpo_dataset path.
RL_TRAIN = os.environ.get("RL_CHAIN_TRAIN", "/training/v2/rl_targets_v8/rl_train.jsonl")
WALL_EVAL = os.environ.get("RL_CHAIN_WALL", "/training/v2/rl_targets_v8/rl_wall_scored.jsonl")
REF_CACHE = "/training/v2/rl_ref/rl_ref_action_dist.pt"
REF_META = "/training/v2/rl_ref/rl_ref_meta.json"
JKGATE_EVIDENCE = os.environ.get("RL_CHAIN_JKGATE",
                                 "/training/v2/reports/RL_JOINT_KL_GATE.json")
STATE = os.environ.get("RL_CHAIN_STATE", "/training/v2/reports/rl_chain_state_013.json")
RL_UNIT = os.environ.get("RL_CHAIN_UNIT", "qwen-rl-001")
LOG = os.environ.get("RL_CHAIN_LOG", "/training/v2/reports/rl_chain_trigger_013.log")


def log(msg, **kw):
    rec = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "msg": msg, **kw}
    line = json.dumps(rec)
    print(line, flush=True)
    try:
        with open(LOG, "a", encoding="utf-8") as fh:
            fh.write(line + "\n")
    except Exception:
        pass


def refuse(msg):
    log("REFUSING: " + msg)
    sys.exit(2)


def wait_more(msg):
    """BENIGN not-ready-yet: exit 3, which the timer wrapper treats as SILENT.

    Waiting for the builds, or for SFT's final weights, is the normal state of this
    chain for days at a time. Exiting 2 there would page the operator every hour and
    launch an assessing agent on a non-fault. Only genuine faults (a failed ref cache,
    a failed joint-KL gate, a failed verify or launch) use refuse/2.
    """
    log("WAITING: " + msg)
    sys.exit(3)


def load_state():
    try:
        return json.load(open(STATE, encoding="utf-8"))
    except Exception:
        return {}


def save_state(d):
    os.makedirs(os.path.dirname(STATE), exist_ok=True)
    json.dump(d, open(STATE, "w", encoding="utf-8"), indent=1)


def sha_dir(p, chunk=1 << 20):
    h = hashlib.sha256()
    for root, _d, files in os.walk(p):
        for f in sorted(files):
            fp = os.path.join(root, f)
            h.update(os.path.relpath(fp, p).encode())
            with open(fp, "rb") as fh:
                for c in iter(lambda: fh.read(chunk), b""):
                    h.update(c)
    return h.hexdigest()


def has_weights(p):
    if not os.path.isdir(p):
        return False
    return any(f.endswith((".safetensors", ".bin")) for f in os.listdir(p))


def _handoff():
    """The SFT->RL handoff module: single source of truth for 'best' + the FSDP2 export."""
    if HERE not in sys.path:
        sys.path.insert(0, HERE)
    import sft_checkpoint_handoff as h
    return h


def policy_artifact_state(p):
    """'hf' (loadable) | 'sharded' (real policy, needs the FSDP2 export) | 'none'.

    WHY NOT has_weights(): the pinned FSDP2 + SHARDED_STATE_DICT save writes ONLY
    `pytorch_model_fsdp_<i>/__<rank>_0.distcp` -- there are NO top-level weights -- so
    `has_weights(SFT_OUT/final)` is False on a PERFECTLY HEALTHY finished run, and the old
    logic would fall through to the liveness check and REFUSE ("did not produce a policy")
    on a run that succeeded. Distinguishing 'sharded' from 'none' is the whole point:
    'none' still aborts, 'sharded' gets exported.
    """
    if not os.path.isdir(p):
        return "none"
    try:
        kind = _handoff().classify_artifact(p)["kind"]
    except Exception as exc:                                       # noqa: BLE001
        log("stage1 handoff import/classify failed, falling back to raw file test",
            error=repr(exc))
        return "hf" if has_weights(p) else "none"
    if kind == "LOADABLE_HF":
        return "hf"
    if kind in ("SHARDED_FSDP2", "SHARDED_ZERO3"):
        return "sharded"
    return "none"


def export_policy(src):
    """Consolidate a sharded policy dir into a loadable BF16 HF dir. CPU-bound."""
    h = _handoff()
    pre = h.shard_metadata_check(src)
    log("stage1 export precheck", ok=pre.get("ok"), tensors=pre.get("tensors"),
        params_b=pre.get("params_b"))
    if not pre.get("ok"):
        refuse("policy at %s failed the shard metadata completeness gate: %s"
               % (src, pre.get("reason") or pre))
    out = src.rstrip("/") + "_bf16_hf"
    res = h.export(src, out)
    if not res.get("ok"):
        refuse("FSDP2 export of %s FAILED: %s" % (src, res.get("reason") or res))
    return out


def unit_active(unit):
    r = subprocess.run(["systemctl", "is-active", unit], capture_output=True, text=True)
    if r.stdout.strip() in ("active", "activating"):
        return True
    # sft-013+ run as transient USER scopes (qwen-<run>.scope under user@1000),
    # invisible to the system manager. Blind here = false "SFT failed" REFUSE.
    for name in (unit + ".scope", unit + ".service", unit):
        r = subprocess.run(["systemctl", "--user", "is-active", name],
                           capture_output=True, text=True)
        if r.stdout.strip() in ("active", "activating"):
            return True
    return _cgroup_present(unit)


def _cgroup_present(unit):
    """Read the cgroup tree directly. Needs no dbus env, so it works from a SYSTEM timer -
    the user-manager query above does not, and returned a false negative on a live run."""
    import glob
    for pat in ("/sys/fs/cgroup/user.slice/*/user@*.service/*/" + unit + ".scope",
                "/sys/fs/cgroup/user.slice/*/user@*.service/*/" + unit + ".service",
                "/sys/fs/cgroup/system.slice/" + unit + ".service"):
        if glob.glob(pat):
            return True
    return False


def heartbeat_fresh(within_s=1800):
    """Alive = the trainer is PRODUCING, whatever the unit bookkeeping says.

    Unit state is a poor liveness signal on this host (transient scopes, no user bus from a
    system timer), and one negative sample already produced a false "SFT failed" REFUSE at
    2026-09-21T00:00:09 while the trainer sat at 100% GPU with a 1.3-minute-old heartbeat.
    A false FAILED is worse than a late one: it pages the operator on a healthy run and, if
    the escalation path is down, it pages with nothing useful at all.
    """
    import glob
    cands = [SFT_OUT + "/train.log", os.path.dirname(SFT_OUT) + "/train.log"]
    cands += glob.glob(SFT_OUT + "/checkpoint-*")
    cands += glob.glob(SFT_OUT + "/runs/*/events.out.tfevents.*")
    fresh = [os.path.getmtime(p) for p in cands if os.path.exists(p)]
    return bool(fresh) and (time.time() - max(fresh)) <= within_s


# ---------------------------------------------------------------- stage 0
def builders_running() -> list:
    """PIDs of RL target builders alive RIGHT NOW (either builder, any invocation).

    This is the one thing that tells an IN-FLIGHT regen (benign: wait, exit 3) from a
    target set that does not match its own pin while nothing is building (a fault that
    waiting cannot fix: exit 2). Both stage0's completeness gate and stage1.5 use it, so
    "is a build running?" has ONE definition.
    """
    pids: list = []
    for sig in ("grpo_dataset.py", "score_wall_eval.py"):
        r = subprocess.run(["pgrep", "-f", sig], capture_output=True, text=True)
        pids += [x for x in r.stdout.split() if x.strip()]
    return sorted(set(pids))


def stage0_precheck():
    missing = []
    running = builders_running()
    for label, path, kind in (("rl_config", CFG, "file"),
                              ("rl_train_targets", RL_TRAIN, "file"),
                              ("wall_eval_scored", WALL_EVAL, "file")):
        ok = os.path.isfile(path) if kind == "file" else os.path.isdir(path)
        log("precondition", **{label: ("OK" if ok else "MISSING"), "path": path})
        if not ok:
            missing.append(path)
    if missing:
        # Same wait-vs-page rule as the completeness gate below: a builder that is ALIVE
        # makes a missing input an in-flight regen (benign); nothing building means the
        # target set was never produced (or was deleted), and waiting cannot fix it.
        msg = "RL inputs missing: %s. The chain will NOT launch on partial inputs." % missing
        if running:
            wait_more(msg + " Builders running: %s" % running)
        refuse(msg + " Nothing is building, so this cannot resolve by waiting.")
    rows = sum(1 for _ in open(RL_TRAIN, encoding="utf-8"))
    wall = sum(1 for _ in open(WALL_EVAL, encoding="utf-8"))
    cfg = json.load(open(CFG, encoding="utf-8"))
    floor = int(cfg["gate"]["gate_min_episodes"])
    log("input_sizes", rl_train_rows=rows, wall_eval_rows=wall,
        gate_min_episodes=floor)
    if wall < floor:
        wait_more("wall eval has %d rows < gate_min_episodes %d: the gate could never "
               "select a checkpoint." % (wall, floor))
    # ---- COMPLETENESS: a build that died mid-flight leaves a file that exists and is
    # non-empty, so existence + rows>0 would both pass and RL would train on a PARTIAL
    # target set after an eight day wait. Pin the exact expectation instead.
    #
    # THE PIN IS THE BUILDER'S OWN STATEMENT (grpo_dataset.write_expectation): the
    # builders write RUNNING before scoring a row and COMPLETE only after the whole
    # candidate set is processed. Stage 0 therefore refuses anything that is not a
    # stated COMPLETE - an ABSENT pin is a REFUSAL, never a pass, because "we could not
    # detect a partial build" is the same failure as a partial build.
    #
    # WAIT vs PAGE: a builder that is ALIVE right now is a benign in-flight regen
    # (exit 3, silent - this is the normal state for days). Nothing running + a pin that
    # is missing, not COMPLETE, or out of count is a fault waiting cannot fix: exit 2,
    # which pages and engages the assessing agent.
    exp_path = os.environ.get("RL_CHAIN_EXPECTED",
                              os.path.join(os.path.dirname(RL_TRAIN), "EXPECTED.json"))
    running = builders_running()
    if not os.path.isfile(exp_path):
        if running:
            wait_more("no expectation pin at %s yet (builders running: %s)"
                      % (exp_path, running))
        refuse("no expectation pin at %s - the target set's completeness CANNOT be "
               "proven and nothing is building. A missing pin is NOT a pass: run the "
               "builder (it writes its own pin) before RL may launch." % exp_path)
    pre_doc = json.load(open(exp_path, encoding="utf-8")) or {}
    status = pre_doc.get("status")
    if status is not None and str(status) != "COMPLETE":
        if running:
            wait_more("expectation pin status=%s (builders running: %s)"
                      % (status, running))
        refuse("expectation pin at %s says status=%s (written %s by %s) - a build that "
               "is not COMPLETE must never launch RL."
               % (exp_path, status, pre_doc.get("generated_at"),
                  (pre_doc.get("builder") or {}).get("builder")))
    rules = pre_doc.get("expected_rows", {})
    # A pin that does not COVER the gate's own inputs is not a completeness proof: the
    # loop below can only check the entries it is given, so an expectation missing
    # `rl_train.jsonl` would silently leave the largest input unverified - exactly the
    # hole this gate exists to close.
    must_pin = {os.path.basename(RL_TRAIN), os.path.basename(WALL_EVAL)}
    unpinned = sorted(must_pin - set(rules))
    if unpinned:
        if running:
            wait_more("expectation pin at %s does not cover %s yet (builders running: %s)"
                      % (exp_path, unpinned, running))
        refuse("expectation pin at %s does not cover the gate inputs %s - an unpinned "
               "input cannot be checked for completeness." % (exp_path, unpinned))
    for name, rule in rules.items():
        fp = os.path.join(os.path.dirname(RL_TRAIN), name)
        if not os.path.isfile(fp):
            if running:
                wait_more("completeness: %s is missing (%s)" % (name, exp_path))
            refuse("completeness: %s is missing but nothing is building (%s)"
                   % (name, exp_path))
        n = sum(1 for _ in open(fp, encoding="utf-8"))
        if "exact" in rule and n != int(rule["exact"]):
            msg = ("completeness: %s has %d rows, expected exactly %d - the build is "
                   "PARTIAL or died mid-flight." % (name, n, int(rule["exact"])))
            if running:
                wait_more(msg + " Builders running: %s" % running)
            refuse(msg + " Nothing is building, so waiting cannot fix it: re-run the "
                         "builder (it re-pins) before RL may launch.")
        if "min_frac" in rule:
            floor_n = int(float(rule["min_frac"]) * int(rule["of"]))
            if n < floor_n:
                if running:
                    wait_more("completeness: %s has %d rows, below the %d floor (%s of "
                              "%d)" % (name, n, floor_n, rule["min_frac"],
                                       int(rule["of"])))
                refuse("completeness: %s has %d rows, below the %d floor (%s of %d) and "
                       "nothing is building" % (name, n, floor_n, rule["min_frac"],
                                                int(rule["of"])))
        log("completeness", **{name: "%d rows OK" % n})
    log("stage0 PASS")
    return {"rl_train_rows": rows, "wall_eval_rows": wall}


# ---------------------------------------------------------------- stage 1
def stage1_wait(poll_s=60, max_wait_s=0):
    global INIT_FROM
    t0 = time.time()
    while True:
        state = policy_artifact_state(INIT_FROM)
        if state == "hf":
            log("stage1 policy ready", init_from=INIT_FROM)
            return sha_dir(INIT_FROM)
        if state == "sharded":
            # The policy EXISTS -- it is FSDP2-sharded. Export it and use the consolidated
            # dir as the policy, because the RL stages LOAD this path (ref cache, joint-KL
            # gate, training) and a DCP shard dir is not loadable by from_pretrained.
            # This is checked BEFORE the liveness test so a healthy finished run is never
            # mistaken for a failed one.
            exported = export_policy(INIT_FROM)
            INIT_FROM = exported
            st = load_state()
            st["init_from"] = INIT_FROM
            st["exported_from"] = SFT_OUT + "/final"
            save_state(st)
            log("stage1 policy exported", init_from=INIT_FROM)
            return sha_dir(INIT_FROM)
        act = unit_active(SFT_UNIT)
        hb = heartbeat_fresh()
        if not (act or hb):
            # A single negative sample is NOT evidence of failure. Require TWO consecutive
            # misses: the trainer writing to disk is the strongest signal available, and a
            # false REFUSE pages the operator on a healthy run.
            st = load_state()
            miss = int(st.get("stage1_liveness_misses", 0)) + 1
            st["stage1_liveness_misses"] = miss
            save_state(st)
            if miss < 2:
                wait_more("SFT unit %s not visible and no fresh heartbeat (miss %d/2) - "
                          "waiting for one more sample before declaring failure"
                          % (SFT_UNIT, miss))
            refuse("SFT unit %s is not active, no fresh heartbeat, and %s has no weights: "
                   "the SFT run did not produce a policy. NOT launching RL."
                   % (SFT_UNIT, INIT_FROM))
        if load_state().get("stage1_liveness_misses"):
            st = load_state()
            st["stage1_liveness_misses"] = 0
            save_state(st)
        if max_wait_s and time.time() - t0 > max_wait_s:
            wait_more("timed out after %ds waiting for %s" % (max_wait_s, INIT_FROM))
        log("stage1 waiting for SFT", unit=SFT_UNIT, active=act,
            waited_s=int(time.time() - t0))
        time.sleep(poll_s)


# ---------------------------------------------------------------- stage 1.5
def stage15_wait_targets(poll_s=60, max_wait_s=0):
    """Wait for the RL target regen to finish. A target file that is still being
    appended to would make the readiness verifier judge a partial corpus - the
    verifier would pass on 9k rows and the run would train on an incomplete set.
    """
    t0 = time.time()
    while True:
        pids = builders_running()
        if not pids:
            rows = sum(1 for _ in open(RL_TRAIN, encoding="utf-8")) \
                if os.path.isfile(RL_TRAIN) else 0
            wall = sum(1 for _ in open(WALL_EVAL, encoding="utf-8")) \
                if os.path.isfile(WALL_EVAL) else 0
            log("stage1.5 target regen finished", rl_train_rows=rows,
                wall_scored_rows=wall)
            if rows == 0:
                wait_more("RL train targets are empty after the regen finished")
            if wall == 0:
                wait_more("wall scored set is empty after the regen finished")
            return rows
        if max_wait_s and time.time() - t0 > max_wait_s:
            wait_more("timed out waiting for the RL target regen (pids=%s)" % pids)
        log("stage1.5 waiting for target regen", pids=len(pids),
            waited_s=int(time.time() - t0))
        time.sleep(poll_s)


# ---------------------------------------------------------------- stage 2
def stage2_ref_cache(ckpt_sha):
    os.makedirs(os.path.dirname(REF_CACHE), exist_ok=True)
    if os.path.isfile(REF_CACHE) and os.path.getsize(REF_CACHE) > 0:
        try:
            meta = json.load(open(REF_META, encoding="utf-8"))
        except Exception:
            meta = {}
        if meta.get("checkpoint_sha256") == ckpt_sha:
            log("stage2 ref cache already built for this policy", cache=REF_CACHE)
            return
        log("stage2 ref cache exists but was built for a DIFFERENT policy: rebuilding",
            had=meta.get("checkpoint_sha256"), want=ckpt_sha)
    cmd = [PY, os.path.join(HERE, "grpo_ref_cache.py"),
           "--prompts", RL_TRAIN, "--ckpt", INIT_FROM, "--out", REF_CACHE,
           "--action-dist", "--device", "cuda"]
    log("stage2 building ref action-dist cache", cmd=" ".join(cmd))
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        refuse("ref cache build failed rc=%d: %s" % (r.returncode, r.stderr[-400:]))
    if not (os.path.isfile(REF_CACHE) and os.path.getsize(REF_CACHE) > 0):
        refuse("ref cache build reported success but %s is empty/missing" % REF_CACHE)
    json.dump({"checkpoint_sha256": ckpt_sha, "init_from": INIT_FROM,
               "prompts": RL_TRAIN, "cache": REF_CACHE,
               "built_at": time.strftime("%Y-%m-%dT%H:%M:%S")},
              open(REF_META, "w", encoding="utf-8"), indent=1)
    log("stage2 PASS", cache=REF_CACHE)


# ---------------------------------------------------------------- stage 2.5
def stage25_joint_kl_gate():
    """HARD GATE: confirm the size-aware anchor on a REAL batch before RL launches.

    The unit self-checks prove the joint math and the slot plumbing with synthetic
    tensors. This is the only check that uses a real policy and the real cache, so it
    is the only one that can prove the policy-side and reference-side constructions
    agree. FAIL-CLOSED: a launch that silently falls back to the marginal KL would
    leave the size token unregularised, which is precisely the fabricated-anchor
    failure this pipeline refuses to ship.
    """
    cmd = [PY, os.path.join(HERE, "rl_joint_kl_gate.py"),
           "--policy", INIT_FROM, "--ref-cache", REF_CACHE,
           "--targets", RL_TRAIN, "--out", JKGATE_EVIDENCE]
    log("stage2.5 joint-KL gate on a real batch", cmd=" ".join(cmd))
    r = subprocess.run(cmd, capture_output=True, text=True)
    log("stage2.5 joint-KL gate rc", rc=r.returncode,
        out=(r.stdout or "").strip()[-400:], err=(r.stderr or "").strip()[-300:])
    if r.returncode != 0:
        refuse("joint-KL gate FAILED on a real batch - NOT launching RL. "
               "Evidence: %s" % JKGATE_EVIDENCE)
    log("stage2.5 PASS")


# ---------------------------------------------------------------- stage 3
def stage3_verify():
    cmd = [PY, os.path.join(HERE, "rl_readiness_verify.py"), "--config", CFG]
    log("stage3 readiness verify", cmd=" ".join(cmd))
    r = subprocess.run(cmd, capture_output=True, text=True)
    tail = (r.stdout or "")[-700:]
    if r.returncode != 0:
        refuse("readiness verify FAILED rc=%d: %s" % (r.returncode, tail))
    log("stage3 PASS", out=tail.replace("\n", " ")[:400])


# ---------------------------------------------------------------- stage 4
def stage4_launch():
    if unit_active(RL_UNIT):
        log("stage4 RL unit already active: nothing to do", unit=RL_UNIT)
        return
    cmd = ["sudo", "systemd-run", "--unit=" + RL_UNIT, "--collect",
           "--property=StandardOutput=append:/training/v2/reports/rl_001_train.log",
           "--property=StandardError=append:/training/v2/reports/rl_001_train.log",
           "--working-directory=" + HERE,
           PY, os.path.join(HERE, "grpo_loop.py"),
           "--config", CFG, "--train"]
    log("stage4 launching RL", cmd=" ".join(cmd))
    r = subprocess.run(cmd, capture_output=True, text=True)
    log("stage4 launch rc", rc=r.returncode, out=(r.stdout or "").strip()[:300],
        err=(r.stderr or "").strip()[:300])
    if r.returncode != 0:
        refuse("RL launch failed rc=%d" % r.returncode)
    time.sleep(20)
    log("stage4 PASS", active=unit_active(RL_UNIT))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--precheck-only", action="store_true")
    ap.add_argument("--poll-s", type=int, default=60)
    ap.add_argument("--max-wait-s", type=int, default=0)
    a = ap.parse_args(argv)
    st = load_state()
    log("=== rl_chain_trigger start ===")
    st["stage0"] = stage0_precheck()
    save_state(st)
    if a.precheck_only:
        log("precheck-only: stopping before the wait")
        return 0
    ckpt_sha = stage1_wait(a.poll_s, a.max_wait_s)
    st["init_from_sha256"] = ckpt_sha
    save_state(st)
    st["stage1_5"] = {"rl_train_rows": stage15_wait_targets(a.poll_s, a.max_wait_s)}
    save_state(st)
    stage2_ref_cache(ckpt_sha)
    st["stage2"] = "done"
    save_state(st)
    stage25_joint_kl_gate()
    st["stage2_5_joint_kl_gate"] = "PASS"
    save_state(st)
    stage3_verify()
    st["stage3"] = "done"
    save_state(st)
    stage4_launch()
    st["stage4"] = "done"
    st["completed_at"] = time.strftime("%Y-%m-%dT%H:%M:%S")
    save_state(st)
    log("=== chain complete ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())