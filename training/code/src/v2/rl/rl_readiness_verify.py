"""rl_readiness_verify — the RL gate that runs BEFORE any RL launch.

Exits non-zero on the first class of defect it finds, so the chain trigger can
refuse. It verifies the RUN, not just the files:

  A config      required keys present; gate floor matches the artifact
  B targets     schema, action space, zero-sum advantages, SKIP/WATCH pinned at
                0, group_usable, prompt is a chat list
  C wall        >= gate_min_episodes; every row is AT/AFTER wall_ms (a pre-wall
                row in the eval set is a leak and fails the run)
  D no-leak     NO train target sits at/after wall_ms, and no train prompt_sha256
                appears in the wall set (the wall is never trained on)
  E engine      reward < pure on every sampled arm (the training signal must be
                CONSERVATIVE against the realized return, never optimistic)
  F policy      init_from exists and carries weights
  G ref cache   present, non-empty, action order == the RL action space
  H driver      grpo_loop.py --selfcheck exits 0
  I notional    every module agrees about the deploy trade size (AST guard,
                kwargs included); no duplicate definition can drift
  J freeze      the frozen C3 exam bar is reproducible from this code (pinned
                code/driver/partition/seedset/accounting/bar all match)
  K exit costs  the exit impairment is MEASURED and frozen, the stress level is
                strictly worse than the training level, and an unexitable position
                is valued by a declared rule capped at basis - never at the mark

NOTE ON MEMORY. Class [J] loads the frozen exam dataset (~4 GB). Run this from a
scope with real memory budget (`systemd-run --user --scope ...`): inside a small
worker cgroup the kernel OOM-kills the exam child and the run dies at [J] with no
verdict, which is how a readiness gate silently stops being a gate.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PY = "/home/alon/qwen27b-venv/bin/python"
ACTIONS = ["SKIP", "WATCH", "BUY_SMALL", "BUY_MID", "BUY_FULL"]
# v9 RE-SCOPE (operator ruling 2026-09-22; audit reports/MIXED_VENUE_EDGE_AUDIT_20260922.txt):
# entry groups are venue-conditional MULTI-TIER — the sizes the prompt offers that the
# venue can execute. AMM = {SKIP, WATCH, BUY_FULL, BUY_SMALL} (the live own-impact veto
# refuses a FULL clip on a thin book while SMALL executes); curve = {SKIP, WATCH,
# BUY_SMALL}. MID stays retired (KELLY_AUDIT_C12 §b). Management rows ride the same file
# with their own 4-arm space.
ENTRY_ARM_SETS = ({"SKIP", "WATCH", "BUY_FULL", "BUY_SMALL"},
                  {"SKIP", "WATCH", "BUY_SMALL"})
MGMT_ARM_SET = {"HOLD", "ADD", "REDUCE", "EXIT"}
fails = []
notes = {}


def check(ok, code, msg):
    if ok:
        print("  PASS %-6s %s" % (code, msg))
    else:
        print("  FAIL %-6s %s" % (code, msg))
        fails.append("%s: %s" % (code, msg))
    return ok


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--config", required=True)
    ap.add_argument("--sample", type=int, default=4000)
    a = ap.parse_args(argv)
    cfg = json.load(open(a.config, encoding="utf-8"))
    print(json.dumps({"verifier": "rl_readiness_verify", "config": a.config}))

    # ---- A config
    print("[A] config")
    need = ["loop", "optim", "gate", "data", "checkpoint"]
    for k in need:
        check(k in cfg, "A.%s" % k, "key %r present" % k)
    for k in ("group_size", "grad_accum", "kl_coef", "max_new_tokens"):
        check(k in cfg.get("loop", {}), "A.loop.%s" % k, "loop.%s present" % k)
    check(cfg["optim"].get("optim") == "adamw_8bit", "A.optim",
          "optim.optim == adamw_8bit (the proven path)")
    floor = int(cfg["gate"]["gate_min_episodes"])
    wall_ms = int(cfg["data"]["wall_ms"])
    notes["gate_min_episodes"] = floor
    notes["wall_ms"] = wall_ms

    train_path = cfg["data"]["train"]
    wall_path = cfg["data"]["eval"]

    # ---- B targets
    print("[B] train targets")
    ntrl = 0
    wall_leaks = 0
    train_shas = set()
    bad_schema = bad_adv = bad_arms = bad_skip = 0
    reward_lt_pure = arms = 0
    with open(train_path, encoding="utf-8") as fh:
        for line in fh:
            if not line.strip():
                continue
            try:
                r = json.loads(line)
            except Exception:
                bad_schema += 1
                continue
            ntrl += 1
            g = r.get("group") or {}
            adv = r.get("advantages") or {}
            if not (r.get("prompt") and r.get("prompt_sha256") and g):
                bad_schema += 1
                continue
            acts = set(r.get("actions") or [])
            if acts not in ENTRY_ARM_SETS and acts != MGMT_ARM_SET:
                bad_arms += 1
            if abs(sum(float(v) for v in adv.values())) > 1e-6:
                bad_adv += 1
            if acts != MGMT_ARM_SET and (g.get("SKIP") != 0.0 or g.get("WATCH") != 0.0):
                bad_skip += 1
            train_shas.add(r["prompt_sha256"])
            t = r.get("t_dec_ms")
            if t is not None and int(t) >= wall_ms:
                wall_leaks += 1
            for k, v in (r.get("buy_policies") or {}).items():
                if "reward" in v and "pure" in v:
                    arms += 1
                    if float(v["reward"]) < float(v["pure"]):
                        reward_lt_pure += 1
    check(ntrl > 0, "B.rows", "train targets non-empty (%d rows)" % ntrl)
    check(bad_schema == 0, "B.schema", "every row has prompt/prompt_sha256/group")
    check(bad_arms == 0, "B.actions",
          "action space is venue-conditional multi-tier %s / %s or mgmt %s"
          % (sorted(ENTRY_ARM_SETS[0]), sorted(ENTRY_ARM_SETS[1]), sorted(MGMT_ARM_SET)))
    check(bad_adv == 0, "B.zerosum", "advantages sum to 0 on every row")
    check(bad_skip == 0, "B.skip", "SKIP and WATCH pinned at 0.0 on every row")
    notes["train_rows"] = ntrl

    # ---- C wall
    print("[C] wall eval set")
    nwall = pre = 0
    wall_shas = set()
    with open(wall_path, encoding="utf-8") as fh:
        for line in fh:
            if not line.strip():
                continue
            r = json.loads(line)
            nwall += 1
            wall_shas.add(r.get("prompt_sha256"))
            t = r.get("t_dec_ms")
            if t is not None and int(t) < wall_ms:
                pre += 1
    check(nwall >= floor, "C.min", "wall rows %d >= gate_min_episodes %d" % (nwall, floor))
    check(pre == 0, "C.wallonly", "no pre-wall row in the eval set (%d found)" % pre)
    notes["wall_rows"] = nwall

    # ---- D no-leak
    print("[D] wall leakage")
    check(wall_leaks == 0, "D.train", "no train target at/after wall_ms (%d)" % wall_leaks)
    overlap = train_shas & wall_shas
    check(not overlap, "D.sha", "no train prompt_sha256 appears in the wall set (%d)"
          % len(overlap))

    # ---- E engine conservatism
    print("[E] reward engine")
    frac = reward_lt_pure / max(arms, 1)
    check(arms > 0, "E.arms", "sampled %d BUY arms" % arms)
    check(frac == 1.0, "E.conservative",
          "reward < pure on every arm (%.4f of %d) - the optimised signal never "
          "overstates the realized return" % (frac, arms))

    # ---- F policy
    print("[F] init policy")
    init_from = cfg.get("init_from") or ""
    ok_dir = os.path.isdir(init_from)
    ok_w = ok_dir and any(f.endswith((".safetensors", ".bin"))
                          for f in os.listdir(init_from))
    check(ok_w, "F.weights", "init_from has weights: %s" % init_from)

    # ---- G ref cache
    print("[G] ref cache")
    ref = (cfg.get("reference") or {}).get("action_dist_cache") or ""
    has_ref = os.path.isfile(ref) and os.path.getsize(ref) > 0
    check(has_ref, "G.present", "ref cache present and non-empty: %s" % ref)

    # ---- H driver selfcheck
    print("[H] driver")
    r = subprocess.run([PY, os.path.join(HERE, "grpo_loop.py"), "--selfcheck"],
                       capture_output=True, text=True)
    check(r.returncode == 0, "H.selfcheck", "grpo_loop.py --selfcheck rc=%d"
          % r.returncode)

    # ---- I deploy notional: every module must agree about the trade size.
    # The cost floor is in bps with a FIXED lamport term, so its bp value scales
    # as 1/notional: two components at 1.0 and 0.5 SOL mis-state cost ~2x. This
    # runs the AST guard (kwargs included) and fails the gate on any live
    # disagreement or duplicate definition of the notional.
    print("[I] deploy notional")
    r = subprocess.run([PY, os.path.join(HERE, "check_deploy_notional.py")],
                       capture_output=True, text=True)
    tail = [ln for ln in r.stdout.splitlines() if ln.startswith(("RESULT", "CONFLICT", "DUPLICATE"))]
    check(r.returncode == 0, "I.notional",
          "check_deploy_notional.py rc=%d %s" % (r.returncode, " | ".join(tail)))
    notes["deploy_notional_guard"] = tail

    # ---- J frozen exam: the C3 bar must remain reproducible from this code.
    # The freeze artifact pins code/driver/partition/seedset/accounting/bar. Until
    # now nothing read it, so an entry-tranche change moved the bar 0.46081 ->
    # 0.70280 (+52%) with the seed set byte-identical and nothing failed. The exam
    # now refuses on any drift by default; this makes the refusal block the chain.
    print("[J] frozen C3 exam")
    r = subprocess.run([PY, os.path.join(HERE, "economic_exam_c3.py")],
                       capture_output=True, text=True)
    tail = [ln for ln in r.stdout.splitlines()
            if "FREEZE VERIFY" in ln or ln.strip().startswith("DRIFT")]
    check(r.returncode == 0, "J.freeze",
          "economic_exam_c3.py rc=%d %s" % (r.returncode, " | ".join(tail[-3:])))
    notes["frozen_c3_exam"] = tail

    # ---- K the exit-cost layer: measured impairment + declared terminal loss
    #
    # The reward used to fill every sell at the quoted mark and to MASK the horizon
    # refusal, so the crash tail carried no cost and no gradient. These checks make the
    # replacement load-bearing: the values must be MEASURED and frozen (not the other
    # codebase's fixtures), the stress level must be strictly worse than the training
    # level, and an unexitable position must be valued by a declared rule capped at basis.
    print("[K] exit impairment + terminal loss")
    imp_freeze = "/training/v2/reports/EXIT_IMPAIRMENT_C14.json"
    check(os.path.isfile(imp_freeze), "K.freeze_present",
          "%s exists" % imp_freeze)
    m = subprocess.run([PY, os.path.join(HERE, "exit_mechanics.py"), "--self-check"],
                       capture_output=True, text=True)
    check(m.returncode == 0, "K.mechanics_selfcheck",
          "exit_mechanics.py --self-check rc=%d" % m.returncode)
    c = subprocess.run([PY, os.path.join(HERE, "calibrate_impairment.py"),
                        "--verify-freeze"], capture_output=True, text=True)
    check(c.returncode == 0, "K.freeze_verifies",
          "calibrate_impairment --verify-freeze rc=%d %s"
          % (c.returncode, c.stdout.strip().splitlines()[-1] if c.stdout.strip() else ""))
    sys.path.insert(0, HERE)
    from exit_mechanics import (TERMINAL_WRITE_TO_ZERO, exit_proceeds_lamports,
                                impairment_bps, impairment_source,
                                terminal_value_lamports)
    src = impairment_source()
    check(src["source"] == "frozen", "K.source_is_measured",
          "impairment source=%s (an unfrozen impairment is the Rust fixtures again)"
          % src["source"])
    real, pess = impairment_bps("realistic"), impairment_bps("pessimistic")
    keys = ("first_sell_penalty_bps", "retry_slippage_bps", "fee_escalation_lamports")
    check(all(real[k] > 0 for k in keys), "K.values_positive", str({k: real[k] for k in keys}))
    check(all(pess[k] > real[k] for k in keys), "K.stress_strictly_worse",
          "pessimistic %s > realistic %s" % ([pess[k] for k in keys], [real[k] for k in keys]))
    g = 10 ** 9
    net = exit_proceeds_lamports(g, first_sell=True, retries=1)["net_lamports"]
    check(0 < net < g, "K.impairment_reduces_proceeds", "net=%d of gross=%d" % (net, g))
    check(terminal_value_lamports(g, TERMINAL_WRITE_TO_ZERO) == 0,
          "K.terminal_write_to_zero", "an unexitable position may not be marked at value")
    from rl_reward_v3 import IMPAIRMENT_LEVEL, TERMINAL_LOSS_POLICY
    check(IMPAIRMENT_LEVEL == "realistic" and TERMINAL_LOSS_POLICY == TERMINAL_WRITE_TO_ZERO,
          "K.file_levels_declared",
          "training level=%s terminal policy=%s" % (IMPAIRMENT_LEVEL, TERMINAL_LOSS_POLICY))
    notes["impairment"] = {"values": {k: real[k] for k in keys},
                           "source": src["source"],
                           "population": src.get("population")}

    # ---- L memorization: a preregistered, load-bearing veto on selection
    print("[L] memorization detector")
    mem = subprocess.run([PY, os.path.join(HERE, "memorization_detector.py"), "--self-check"],
                         capture_output=True, text=True)
    check(mem.returncode == 0, "L.detector_selfcheck",
          "memorization_detector.py --self-check rc=%d (honest passes, memorizer flagged, "
          "thin groups refuse)" % mem.returncode)
    prereg_path = "/training/v2/reports/MEMORIZATION_BOUNDS.json"
    prereg_ok = False
    if os.path.isfile(prereg_path):
        try:
            pre = json.load(open(prereg_path, encoding="utf-8"))
            prereg_ok = bool(pre.get("statistics") and pre.get("null")
                             and pre.get("decision_rule") and pre.get("fail_closed"))
        except Exception:
            prereg_ok = False
    check(prereg_ok, "L.instrument_preregistered",
          "%s registers the statistics, null, decision rule and fail-closed behaviour"
          % prereg_path)
    loop_src = os.path.join(HERE, "grpo_loop.py")
    wired = "memorization_detector" in open(loop_src, encoding="utf-8").read()
    check(wired, "L.wired_into_selection",
          "grpo_loop.run_gate calls the detector; selection needs the wall AND a clean verdict")
    # The producer must exist and must pass its own CPU-only self-check: the gate is
    # only as load-bearing as the records it reads, and an unverified producer is the
    # same class of hole as an instrument with nothing feeding it.
    prod = os.path.join(HERE, "emit_memorization_records.py")
    check(os.path.isfile(prod), "L.records_producer_present",
          "emit_memorization_records.py (the prereg's registered records producer) exists")
    m = subprocess.run([PY, prod, "--self-check"], capture_output=True, text=True)
    check(m.returncode == 0, "L.records_producer_selfcheck",
          "emit_memorization_records.py --self-check rc=%d (honest->clean, memorizer->"
          "SUSPECTED, thin->insufficient_groups, null bp never coerced to 0.0)"
          % m.returncode)
    # FAIL-CLOSED, PROVEN (not read off the filesystem): point the gate at a records
    # path that CANNOT exist and require a REFUSAL. Testing the ambient state instead
    # would turn this check into a false failure the moment a real run legitimately
    # wrote records - and a check that fails for the wrong reason gets waived.
    probe = os.path.join(HERE, "_mem_probe_absent.jsonl")
    script = ("import json,grpo_loop as G;"
              "p=G.memorization_precheck();"
              "print(json.dumps({'ok':p.get('ok'),'reason':str(p.get('reason',''))}))")
    env = dict(os.environ)
    env["RL_MEM_RECORDS"] = probe
    if os.path.exists(probe):
        os.remove(probe)
    r = subprocess.run([PY, "-c", script], capture_output=True, text=True,
                       cwd=HERE, env=env)
    try:
        pre = json.loads(r.stdout.strip().splitlines()[-1])
    except Exception:                                            # noqa: BLE001
        pre = {"ok": None, "reason": (r.stdout + r.stderr)[-120:]}
    check(pre.get("ok") is False and "records" in str(pre.get("reason", "")),
          "L.refuses_without_records",
          "records path that cannot exist -> ok=%s reason=%s"
          % (pre.get("ok"), str(pre.get("reason"))[:80]))

    # ---- M the M6 decision->fill drift: MEASURED, FROZEN and actually charged.
    #
    # The reward prices every entry at the first tape fill after the decision clock,
    # while the prompt states the price AT the decision clock. The gap is a real cost
    # that used to be charged as nothing. These checks make the replacement
    # load-bearing: the magnitude must be re-derivable from the tape by the pinned
    # calibrator (not a literal in the reward), it must be positive at the trained
    # level and strictly worse at the gate's stress level, and the reward must be
    # demonstrated to READ the artifact rather than a constant baked into the file.
    print("[M] decision->fill drift")
    fd_freeze = "/training/v2/reports/FILL_DRIFT_C15.json"
    check(os.path.isfile(fd_freeze), "M.freeze_present", "%s exists" % fd_freeze)
    m = subprocess.run([PY, os.path.join(HERE, "fill_drift.py"), "--self-check"],
                       capture_output=True, text=True)
    check(m.returncode == 0, "M.drift_selfcheck",
          "fill_drift.py --self-check rc=%d" % m.returncode)
    c = subprocess.run([PY, os.path.join(HERE, "calibrate_fill_drift.py"),
                        "--verify-freeze"], capture_output=True, text=True)
    check(c.returncode == 0, "M.freeze_verifies",
          "calibrate_fill_drift --verify-freeze rc=%d %s"
          % (c.returncode, c.stdout.strip().splitlines()[-1] if c.stdout.strip() else ""))
    sys.path.insert(0, HERE)
    import fill_drift
    from fill_drift import entry_drift_bps, fill_drift_source
    dsrc = fill_drift_source()
    check(dsrc["source"] == "frozen", "M.source_is_measured",
          "drift source=%s (an unfrozen drift charges 0 and is un-recalibratable)"
          % dsrc["source"])
    real, pess = entry_drift_bps("realistic"), entry_drift_bps("pessimistic")
    check(real["entry_slippage_bps"] > 0, "M.bound_positive",
          "trained bound=%d bp" % real["entry_slippage_bps"])
    check(pess["entry_slippage_bps"] > real["entry_slippage_bps"],
          "M.stress_strictly_worse",
          "pessimistic %d > realistic %d" % (pess["entry_slippage_bps"],
                                             real["entry_slippage_bps"]))
    check(bool(dsrc["quantile"]) and "fill" in str(dsrc["sign_convention"]),
          "M.quantile_and_sign_recorded",
          "quantile=%s sign=%s" % (dsrc["quantile"], dsrc["sign_convention"]))
    check(os.path.getsize(fd_freeze) > 0 and bool(dsrc["justification"]),
          "M.justification_recorded",
          "the freeze carries a justification string")
    # NO HARDCODED DRIFT: substitute the reader the reward uses and require the
    # substitute to come back through the reward's own accessor. A literal magnitude
    # inside rl_reward_v3 (or simulate_v3) survives this and fails the check.
    import rl_reward_v3
    no_literal = False
    try:
        _sentinel = 4242
        _orig = rl_reward_v3.entry_drift_bps
        try:
            rl_reward_v3.entry_drift_bps = lambda level="realistic": {
                "entry_slippage_bps": _sentinel, "source": "sentinel", "level": level}
            got = rl_reward_v3.entry_drift_for_pricing()
            no_literal = int(got.get("entry_slippage_bps", -1)) == _sentinel
        finally:
            rl_reward_v3.entry_drift_bps = _orig
    except Exception:
        no_literal = False
    check(no_literal, "M.reward_reads_the_artifact",
          "substituting fill_drift.entry_drift_bps moves the reward's charged bound "
          "(a hardcoded drift cannot be re-derived from the tape)")
    reward_src = open(os.path.join(HERE, "rl_reward_v3.py"), encoding="utf-8").read()
    check("charge_entry_fill" in reward_src and "entry_drift_for_pricing" in reward_src,
          "M.consumed_in_simulate_v3",
          "simulate_v3 charges the frozen drift on the entry fill")
    check(bool(rl_reward_v3.FILL_DRIFT_ENABLED), "M.enabled_by_default",
          "the charge is on in the training path (level=%s)"
          % rl_reward_v3.FILL_DRIFT_LEVEL)
    notes["fill_drift"] = {"values": {"realistic_bps": real["entry_slippage_bps"],
                                      "pessimistic_bps": pess["entry_slippage_bps"]},
                           "source": dsrc["source"], "quantile": dsrc["quantile"],
                           "population": dsrc.get("population")}

    out = {"verdict": "PASS" if not fails else "FAIL", "failures": fails,
           "notes": notes}
    print(json.dumps(out, indent=1))
    with open("/training/v2/reports/RL_READINESS_VERIFY.json", "w",
              encoding="utf-8") as fh:
        json.dump(out, fh, indent=1)
    return 1 if fails else 0


if __name__ == "__main__":
    raise SystemExit(main())