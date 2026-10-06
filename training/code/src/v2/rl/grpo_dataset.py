#!/usr/bin/env python
"""grpo_dataset — build RL prompts + counterfactual groups over the wall-aware split.

SPLIT POLICY (recovered decision, session 20260913_125331_f95ddf). The immutable
forward wall is 8 h: `wall_ms = 1788984256517`, 31,350 episodes / 479 mints
(`reports/FORWARD_WALL_V1.json`, ids sha256 4a503bbf...). The 24 h alternative
would have cost 39% of the corpus (31% of train, 54,997 rows); the 2 h / 4 h / 8 h
widths reserve the SAME 31,350 rows, so 8 h is free insurance and 12 h+ is not
(83,669 rows). Therefore:

  RL TRAIN  = corpus train split, t_dec < wall_ms, not in wall ids   (173,441 eps)
  RL EVAL   = the wall itself, NEVER trained on                      (31,350 eps)

ACTION SPACE is the model's own SFT vocabulary, not a private one: {BUY, WATCH,
SKIP}, because the SFT prompts say exactly that and the RL mask parses the
`DECISION:` field. Each action gets a V3 counterfactual value:

  SKIP  = 0.0                       (no position; the value to beat)
  WATCH = 0.0                       (deferral is FREE; see the WATCH note below)
  BUY   = max over exit policies of the V3 risk-adjusted reward

WATCH NOTE (honest limitation, do not paper over): with WATCH at 0.0 it is
value-identical to SKIP, so the learnable signal is "act or don't". WATCH's true
value is the option value of re-deciding at t+delta, which needs a re-decision
model. A v4 refinement, not a blocker.

PROMPT FIDELITY: the prompt is `messages[:-1]` verbatim from the corpus, so the
RL prompt is byte-identical to the SFT prompt and the ref-logprob cache keys match.

RECORD SCHEMA v2 (2026-09-21, CVaR carry-through). Three fields are ADDED to every
emitted record, next to the existing `advantages` payload. Nothing existing is renamed,
reordered or retuned:

  group_loss_weight  float - the per-decision GROUP LOSS MULTIPLIER reported by
                            `grpo_reward_bridge.score_decision`, carried through so the
                            objective is available at train time without a re-score.
                            ABSENCE MEANS 1.0: a pre-v2 record carries no such field and
                            MUST be read as 1.0, never fabricated. The bridge returns
                            exactly 1.0 for every decision today (lambda_cvar pinned to
                            0.0, OFF), so carrying it is an exact no-op - proven by
                            `rl_cvar_noop_gate.py` against a pre-change emission.
  cvar               dict  - a COMPACT tail-risk diagnostic of the record's OWN group
                            (alpha, cvar, cvar_z, n, n_tail, applied, lambda_cvar,
                            source). Diagnostics ONLY: nothing here reaches
                            `advantages`. Taken verbatim from the source `meta.cvar` when
                            a build persists it, else derived from the record's own group
                            values by the pinned `reward_terms` authority and stamped
                            `source="derived_from_group"`.
  schema_version     int   - 2 for this layout (1 = pre-v2, the field then absent).

`advantage_mode` is CARRIED when the source record carries it and left absent otherwise -
inventing one here would let the builder's guess diverge from the bridge's own label.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import numpy as np  # noqa: E402

from rl_reward_v3 import (  # noqa: E402
    CORPUS_PARTS, RL_WALL_JSON, SKIP_VALUE, V3Engine, ReserveRegistry,
    STALE_ANY, score_entry, load_canonical_tapes, counterfactual_stats,
)

# reward_terms is the pinned authority for the CVaR math; re-implementing `group_cvar`
# here would let the diagnostic drift from the number the objective actually uses.
from reward_terms import RewardConfig, tail_amplified_rewards  # noqa: E402

from size_dimension import SIZE_FRACTIONS, SIZE_NOTIONAL_SOL  # noqa: E402

ACTIONS = ("SKIP", "WATCH", "BUY_SMALL", "BUY_MID", "BUY_FULL")

RECORD_SCHEMA_VERSION = 2
NEW_FIELDS = ("group_loss_weight", "cvar", "schema_version")


def cvar_summary(group_values):
    """Compact tail-risk diagnostic of one group, from the pinned reward_terms authority.

    lambda_cvar is deliberately NOT a parameter: the diagnostic must report the tail as
    the OBJECTIVE would see it, and that lambda is pinned in reward_terms (0.0 today).
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
    PRESENT-but-malformed weight is refused loudly rather than coerced - a corrupt weight
    silently read as 1.0 would drop a real objective factor.
    """
    raw = (meta or {}).get("group_loss_weight")
    if raw is None:
        glw = 1.0
    elif isinstance(raw, bool) or not isinstance(raw, (int, float)) \
            or not math.isfinite(float(raw)) or float(raw) < 0.0:
        raise ValueError(f"REFUSING: malformed group_loss_weight {raw!r} - expected a "
                         f"finite float >= 0, or no field at all (which means 1.0)")
    else:
        glw = float(raw)
    src = (meta or {}).get("cvar")
    return glw, (src if isinstance(src, dict) else cvar_summary(group_values))


def carried_advantage_mode(meta):
    """`advantage_mode` is carried only when the source carries it (see docstring)."""
    return {"advantage_mode": (meta or {})["advantage_mode"]} \
        if (meta or {}).get("advantage_mode") is not None else {}

# RE-ARM (KELLY_AUDIT_C12 §b, 2026-09-19): entry grading is venue-conditional
# 2-arm. 5-arm per-tier BUY grading trains mass onto arms the Option-A policy
# can no longer emit (MID retired; sizing is ev_gated_binary). The scored BUY
# arm is the venue's ONLY live size: AMM -> FULL, curve -> SMALL satellite.
# SKIP/WATCH grading unchanged. A record's action space is therefore 3 arms:
# {SKIP, WATCH, BUY_<venue tier>}. Records with unresolvable venue are MASKED
# (refused), never defaulted onto a tier.
#
# v9 RE-SCOPE (operator ruling 2026-09-22, from the refusal-edge audit
# reports/MIXED_VENUE_EDGE_AUDIT_20260922.txt): the group scores EVERY size the PROMPT
# offers the venue, not just the venue's single live tier.
#
# WHY. Every prompt prints all three sizes WITH their depth-derived round-trip costs
# ("SMALL = 0.25 SOL - 288 bp round trip (pool depth 51.3 SOL)" / MID / FULL), and the
# live entry path VETOES a clip whose own impact exceeds 90 bp. On a thin AMM row the
# FULL clip is therefore unexecutable while SMALL executes - and v8 gave that row a
# FULL-only BUY arm, so the group could not express the size the prompt advertised.
# Measured with the real engine on 8 thin AMM rows: FULL -2600/-2331/-3042/-2494/
# -2590/-1734/-4646/-3140 bp vs SMALL -476/-456/-610/-462/-542/-49/-1094/-481 bp net.
# 21,316 of 95,868 v8 rows carried that FULL-only arm. MID stays RETIRED
# (KELLY_AUDIT_C12 §b): the added tier is the one the venue can actually execute.
ENTRY_TIERS_BY_REGIME = {"amm": ("FULL", "SMALL"), "bonding_curve": ("SMALL",)}
# v8 line kept for provenance of the pre-v9 build:
#   ENTRY_TIER_BY_REGIME = {"amm": "FULL", "bonding_curve": "SMALL"}

# --- the management arm -------------------------------------------------------------
# The entry group is a 5-arm counterfactual over sizes (above). The management group is a
# 4-arm choice at a position you already hold: HOLD / ADD / REDUCE / EXIT. It is built from
# `reports/mgmt_c10`, where each row's `meta.candidates` were produced by
# grpo_reward_bridge.score_decision(family="management", entry=held_state) - the SAME
# function that produced the SFT label. Emitting them here is what lets RL learn to exit;
# without it the policy vocabulary SFT teaches at 39,180 rows cold-starts in RL.
MANAGEMENT_ACTIONS = ("HOLD", "ADD", "REDUCE", "EXIT")
MGMT_DIR = "/training/v2/reports/mgmt_c12_aligned"  # RE-ARM: barrier-aligned c12 rebuild
BUY_POLICIES = ("MOONSHOT_TAIL", "TRAIL_ONLY", "HOLD_TO_HORIZON")
DEFAULT_OUT = "/training/v2/rl_targets_v3"


def wall_state(path: str = RL_WALL_JSON) -> dict:
    """Load the immutable forward wall. REFUSES if absent: silently training on
    the forward test would destroy the only honest evaluation we have."""
    if not os.path.isfile(path):
        raise SystemExit(f"REFUSING: forward wall not found: {path}")
    w = json.load(open(path, encoding="utf-8"))
    for k in ("wall_ms", "reserved_rows", "wall_episode_ids_sha256"):
        if k not in w:
            raise SystemExit(f"REFUSING: wall missing {k}")
    ids_path = w.get("files", {}).get("ids", "/training/v2/forward_wall_v1/"
                                                  "wall_episode_ids.txt")
    if not os.path.isfile(ids_path):
        raise SystemExit(f"REFUSING: wall ids not found: {ids_path}")
    h = hashlib.sha256(open(ids_path, "rb").read()).hexdigest()
    if h != w["wall_episode_ids_sha256"]:
        raise SystemExit(f"REFUSING: wall ids sha256 {h} != recorded "
                         f"{w['wall_episode_ids_sha256']} - the wall CHANGED")
    eps = set()
    for line in open(ids_path, encoding="utf-8"):
        line = line.strip()
        if not line:
            continue
        try:
            ms, mint, fam, split = json.loads(line)
        except Exception:
            continue
        eps.add((int(ms), mint))
    w["wall_episodes"] = eps
    w["ids_sha256_verified"] = h
    return w


def in_wall(w: dict, t_dec_ms: int, mint: str) -> bool:
    if int(t_dec_ms) >= int(w["wall_ms"]):
        return True
    return (int(t_dec_ms), mint) in w["wall_episodes"]


def prompt_of(rec: dict) -> str:
    """The exact SFT prompt: every message except the assistant turn."""
    ms = rec.get("messages") or []
    keep = [m for m in ms if m.get("role") != "assistant"]
    return json.dumps(keep, ensure_ascii=False, sort_keys=True)


def prompt_sha(prompt: str) -> str:
    return hashlib.sha256(prompt.encode("utf-8")).hexdigest()


def build_records(rec: dict, tape, engine, horizon_ms: int) -> dict | None:
    """One decision -> the SFT-faithful prompt plus a 3-arm counterfactual group."""
    eid = rec.get("episode_id") or ""
    parts = eid.split(":")
    if len(parts) < 3:
        return None
    try:
        t_dec = int(parts[2])
    except ValueError:
        return None
    prompt = prompt_of(rec)
    group = {"SKIP": SKIP_VALUE, "WATCH": SKIP_VALUE}
    detail = {"SKIP": {"reward": SKIP_VALUE, "pure": 0.0, "kind": "no_position"},
              "WATCH": {"reward": SKIP_VALUE, "pure": 0.0, "kind": "deferral_free"}}
    # SIZE IS AN ARM. v9: every tier the PROMPT offers that this venue can execute is
    # scored (AMM -> FULL + SMALL, curve -> SMALL); MID stays retired. Resolve the
    # venue at the decision instant from the tape (same resolution the engine uses
    # for regime), then score EACH tier through the best exit policy at that tier's
    # pinned clip. Unresolvable venue -> masked.
    venue, _t_v = engine.tape_venue_at(tape, t_dec)
    regime = engine._regime_of_venue(venue)
    tiers = ENTRY_TIERS_BY_REGIME.get(regime.value) if regime is not None else None
    if not tiers:
        return {"__none__": True, "statuses": ["venue_unresolved"],
                "reason": f"tape venue={venue!r} at t_dec resolves to no live tier"}
    arm_space = ("SKIP", "WATCH") + tuple("BUY_" + t for t in tiers)
    for tier in tiers:
        frac = SIZE_FRACTIONS[tier]
        arm = "BUY_" + tier
        best = None
        for pol in BUY_POLICIES:
            res = score_entry(tape, t_dec, engine, policy_name=pol,
                              horizon_ms=horizon_ms, deploy_sol=frac * SIZE_NOTIONAL_SOL)
            if res.get("refused") or res.get("penalised") is None:
                st = res.get("status") or "refused"
                detail[f"_refused:{arm}:{pol}"] = {"status": st,
                                                   "reason": (res.get("reason") or "")[:120]}
                continue
            cand = {"reward": float(res["penalised"]), "pure": float(res["pure"]),
                    "held_ms": res.get("held_ms"), "exit_reason": res.get("exit_reason"),
                    "regime": res.get("regime"), "deploy_sol": frac * SIZE_NOTIONAL_SOL,
                    "size": tier, "policy": pol}
            if best is None or cand["reward"] > best["reward"]:
                best = cand
        if best is not None:
            group[arm] = float(best["reward"])
            detail[arm] = {**best, "kind": "entry_at_size_through_best_exit_policy"}
    buys = [v for k, v in detail.items() if k.startswith("BUY_") and "reward" in v]
    if not buys:
        # no priceable BUY arm -> MASKED, not zeroed. The refusal STATUS is carried
        # out so the yield is diagnosable instead of looking like "no signal".
        refs = [v["status"] for k, v in detail.items() if k.startswith("_refused:")]
        return {"__none__": True, "statuses": refs[:6],
                "reason": next((v["reason"] for k, v in detail.items()
                                if k.startswith("_refused:")), "")}
    best = max(buys, key=lambda d: d["reward"])
    adv, st = counterfactual_stats(group)
    glw, cvar = carried_diagnostics(rec.get("meta"), group.values())
    return {
        "episode_id": eid, "mint": rec.get("mint"), "t_dec_ms": t_dec,
        "split": rec.get("split"),
        "prompt": prompt, "prompt_sha256": prompt_sha(prompt),
        "actions": list(arm_space), "group": group,
        # NOT rounded: the zero-sum identity is load-bearing for the KL/policy
        # balance, and rounding to 6dp broke it by ~1.5e-6 (reproduced).
        "advantages": {k: float(v) for k, v in adv.items()},
        # v2 carried fields, next to the advantages the trainer looks up.
        "group_loss_weight": glw,
        "cvar": cvar,
        "schema_version": RECORD_SCHEMA_VERSION,
        "best_action": st["best_action"], "n_arms": st["n_arms"],
        "group_usable": st["usable"],
        **carried_advantage_mode(rec.get("meta")),
        "buy_policies": {k: v for k, v in detail.items() if k.startswith("BUY_")},
        "sft_action": (rec.get("meta") or {}).get("action"),
        "regime": best.get("regime"),
        "provenance": engine.provenance(),
    }


EXPECTATION_SCHEMA = "rl_targets_expectation/2"


def expectation_path(out_path: str) -> str:
    """The completeness pin lives NEXT TO the file it describes."""
    return os.path.join(os.path.dirname(out_path) or ".", "EXPECTED.json")


def _builder_fingerprint() -> dict:
    try:
        sha = hashlib.sha256(open(__file__, "rb").read()).hexdigest()
    except OSError:
        sha = ""
    return {"builder": os.path.basename(__file__), "sha256": sha}


def write_expectation(out_path: str, name: str, *, status: str,
                      count: int | None = None, extra: dict | None = None) -> dict:
    """Merge this build's state into `<dir>/EXPECTED.json`, atomically (tmp + replace).

    WHY THE BUILDER OWNS ITS OWN PIN. The RL chain's stage-0 completeness gate reads
    EXPECTED.json and refuses to launch on a partial target set - after an eight-day wait.
    When that file was maintained by hand, an ABSENT pin was indistinguishable from a
    finished build: the gate logged "PARTIAL builds are NOT detectable" and PROCEEDED, so
    a crashed rebuild could be trained on, and a policy trained on a half-scored corpus
    would look exactly like a healthy run (and the memorization veto would judge it
    against the same partial evidence).

    So the builder states its own state, and the chain believes only a stated COMPLETE:
      * RUNNING   written BEFORE any row is scored. A crash leaves this in place, which
                  is what makes an interrupted build VISIBLE instead of merely possible.
      * COMPLETE  written only after the FULL candidate set has been processed (and any
                  management stage returned normally), with the exact row count read back
                  from the file itself.
    `expected_rows` is MERGED, never replaced, so the train pin and the wall pin live in
    one file without clobbering each other.
    """
    p = expectation_path(out_path)
    try:
        cur = json.load(open(p, encoding="utf-8"))
        if not isinstance(cur, dict):
            cur = {}
    except Exception:                                            # noqa: BLE001
        cur = {}
    rows = dict(cur.get("expected_rows") or {})
    if count is not None:
        rows[name] = {
            "exact": int(count),
            "note": ("PINNED BY THE BUILDER at %s; exact count read back from the file "
                     "after the full candidate set was processed." % status),
        }
    doc = {**cur, "schema": EXPECTATION_SCHEMA, "status": status,
           "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
           "builder": _builder_fingerprint(),
           "entry_tiers_by_regime": {k: list(v)
                                     for k, v in ENTRY_TIERS_BY_REGIME.items()},
           "expected_rows": rows}
    if extra:
        doc.update(extra)
    tmp = p + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(doc, fh, indent=1, sort_keys=True, default=str)
    os.replace(tmp, p)
    return doc


def count_rows(path: str) -> int:
    with open(path, encoding="utf-8") as fh:
        return sum(1 for line in fh if line.strip())


def _done_shas(out_path: str) -> set:
    """Resume support: prompts already scored are skipped, so a restart is a
    resume, not a restart."""
    done = set()
    if os.path.isfile(out_path):
        for line in open(out_path, encoding="utf-8"):
            try:
                done.add(json.loads(line)["prompt_sha256"])
            except Exception:
                continue
    return done


def build_split(split: str = "train", *, out_path: str = "",
                max_records: int = 0, horizon_ms: int = 1_800_000,
                verbose: bool = True, with_management: bool = False) -> dict:
    """Score one corpus split into RL records. Never touches a wall episode."""
    w = wall_state()
    parts = [p for p in CORPUS_PARTS if f"/{split}.jsonl" in p]
    if not parts:
        raise SystemExit(f"unknown split {split!r}")
    out_path = out_path or os.path.join(DEFAULT_OUT, f"rl_{split}.jsonl")
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    done = _done_shas(out_path)
    reg = ReserveRegistry.build(verbose=verbose)
    eng = V3Engine(reg, max_reserve_stale_ms=STALE_ANY, deploy_sol=1.0)
    tapes = load_canonical_tapes("/training/v2/canonical/renorm_corpus_mints/"
                                 "trades.jsonl",
                                 min_notional_lamports=100_000, max_px_ratio=50)
    stats = {"split": split, "wall_ms": w["wall_ms"], "scanned": 0,
             "skipped_in_wall": 0, "skipped_already_done": 0, "skipped_no_tape": 0,
             "skipped_outside_tape_window": 0,
             "scored": 0, "none": 0, "refusals": {}, "out": out_path}
    # COLLECT FIRST, THEN ORDER BY MINT. Reading the corpus in FILE ORDER clusters
    # on a handful of early graduated mints: the first 1,731 decisions were 99%
    # refused because they were all pumpswap-venue with no pool reserves, which
    # says nothing about the corpus. A deterministic mint-stratified order makes
    # any partial build representative and every resume unbiased.
    cand = []
    for p in parts:
        if not os.path.isfile(p):
            continue
        for line in open(p, encoding="utf-8"):
            if '"decision"' not in line:
                continue
            try:
                rec = json.loads(line)
            except Exception:
                continue
            if rec.get("family") != "decision":
                continue
            eid = rec.get("episode_id") or ""
            parts_e = eid.split(":")
            if len(parts_e) < 3:
                continue
            try:
                cand.append((int(parts_e[2]), parts_e[1], rec))
            except ValueError:
                continue
    cand.sort(key=lambda x: x[1])                     # group by mint
    rng = np.random.default_rng(20260913)
    order = list(range(len(cand)))
    rng.shuffle(order)                                # deterministic mint strata
    cand = [cand[i] for i in order]
    stats["candidates"] = len(cand)
    # RUNNING FIRST, before a single row is scored: an interrupted build must be
    # VISIBLE to the chain's completeness gate, not merely possible (see
    # write_expectation).
    write_expectation(out_path, os.path.basename(out_path), status="RUNNING",
                      extra={"split": split, "candidates": len(cand),
                             "max_records": int(max_records or 0)})
    n = 0
    with open(out_path, "a", encoding="utf-8") as fh:
        for t_dec, mint, rec in cand:
            stats["scanned"] += 1
            if in_wall(w, t_dec, mint):
                stats["skipped_in_wall"] += 1
                continue
            pr = prompt_of(rec)
            if prompt_sha(pr) in done:
                stats["skipped_already_done"] += 1
                continue
            eid = rec.get("episode_id") or ""
            tape = tapes.get(mint)
            if tape is None:
                stats["skipped_no_tape"] += 1
                continue
            # THE COVERAGE BUG (reproduced: 1,711 of 1,731 early train
            # decisions scored nothing): a decision whose t_dec sits OUTSIDE
            # the recorded tape window cannot be priced at all. Without this
            # filter it silently became "no priceable BUY arm" and looked
            # like an engine refusal instead of a window mismatch.
            if not (int(tape.tt[0]) <= t_dec <= int(tape.tt[-1])):
                stats["skipped_outside_tape_window"] += 1
                continue
            try:
                out = build_records(rec, tape, eng, horizon_ms)
            except Exception as e:                    # noqa: BLE001
                k = type(e).__name__
                stats["refusals"][k] = stats["refusals"].get(k, 0) + 1
                continue
            if out is None:
                stats["none"] += 1
                continue
            if out.get("__none__"):
                k = out["statuses"][0] if out.get("statuses") else "no_arm"
                stats["refusals"][k] = stats["refusals"].get(k, 0) + 1
                if stats["none"] == 0 and stats["scored"] == 0:
                    stats["first_none_reason"] = out.get("reason")
                stats["none"] += 1
                continue
            fh.write(json.dumps(out, default=str) + "\n")
            stats["scored"] += 1
            n += 1
            if n % 200 == 0:
                fh.flush()
                if verbose:
                    print(json.dumps({k: stats[k] for k in
                                      ("scored", "skipped_in_wall",
                                       "skipped_no_tape")}), flush=True)
            if max_records and n >= max_records:
                break
    stats["regime_decisions"] = dict(eng.regime_decisions)
    stats["registry"] = {k: v for k, v in reg.stats.items() if k != "sources"}
    # The management arm is appended to the SAME group file: one RL dataset, two action
    # spaces. It needs no tape or oracle - its values are the reward engine's own outputs.
    stats["management_group"] = (build_management_records(split, out_path, verbose=verbose)
                                 if with_management else {"status": "not_requested"})
    # ---- the pin: COMPLETE only when the WHOLE candidate set was processed ----------
    # A resume run writes few (or zero) rows while still processing every candidate, so
    # completeness is about the CANDIDATE SET, and the count is read back from the file
    # itself rather than from this run's append count.
    processed_all = (not max_records) and stats["scanned"] >= stats["candidates"]
    total = count_rows(out_path)
    stats["expectation"] = write_expectation(
        out_path, os.path.basename(out_path),
        status="COMPLETE" if processed_all else "INCOMPLETE",
        count=total if processed_all else None,
        extra={"split": split, "candidates": stats["candidates"],
               "scanned": stats["scanned"], "scored_this_run": stats["scored"],
               "skipped_already_done": stats["skipped_already_done"],
               "refusals": dict(stats["refusals"]), "none": stats["none"],
               "total_rows_in_file": total})
    if verbose:
        print(json.dumps(stats, indent=1, default=str))
    return stats



def build_management_records(split: str, out_path: str, mgmt_dir: str = MGMT_DIR,
                             max_records: int = 0, verbose: bool = False) -> dict:
    """Append the management arm to the RL group file.

    Values are READ from `meta.candidates`, never recomputed here: they are the reward
    engine's own outputs, so the RL advantage and the SFT label cannot drift apart.
    """
    st = {"split": split, "family": "management_replay", "rows": 0, "emitted": 0,
          "skipped_bad_arm_set": 0, "skipped_nonfinite": 0, "out": out_path}
    path = os.path.join(mgmt_dir, f"{split}.jsonl")
    if not os.path.isfile(path):
        st["status"] = "absent"
        return st
    with open(path, encoding="utf-8") as fh, open(out_path, "a", encoding="utf-8") as out:
        for line in fh:
            rec = json.loads(line)
            m = rec.get("meta") or {}
            if m.get("family") != "management_replay":
                continue
            st["rows"] += 1
            cand = m.get("candidates") or {}
            if set(cand) != set(MANAGEMENT_ACTIONS):
                st["skipped_bad_arm_set"] += 1
                continue
            if any(not isinstance(v, (int, float)) or not math.isfinite(float(v))
                   for v in cand.values()):
                st["skipped_nonfinite"] += 1
                continue
            group = {k: float(cand[k]) for k in MANAGEMENT_ACTIONS}
            prompt = prompt_of(rec)
            adv, s = counterfactual_stats(group)
            glw, cvar = carried_diagnostics(m, group.values())
            out.write(json.dumps({
                "episode_id": rec.get("episode_id"),
                "mint": rec.get("mint"), "t_dec_ms": m.get("decision_time_unix_ms"),
                "split": rec.get("split"), "family": "management_replay",
                "prompt": prompt, "prompt_sha256": prompt_sha(prompt),
                "actions": list(MANAGEMENT_ACTIONS), "group": group,
                "advantages": {k: float(v) for k, v in adv.items()},
                # v2 carried fields, next to the advantages the trainer looks up.
                "group_loss_weight": glw,
                "cvar": cvar,
                "schema_version": RECORD_SCHEMA_VERSION,
                "best_action": s["best_action"], "n_arms": s["n_arms"],
                "group_usable": s["usable"],
                **carried_advantage_mode(m),
                "sft_action": m.get("action"),
                "step": m.get("step"),
                "regime": (m.get("cost_authority") or {}).get("regime"),
                "provenance": {
                    "engine": "reward_engine.score_action_sequence(entry=held_state)",
                    "bridge": "grpo_reward_bridge.score_decision(family='management')",
                    "source": "reports/mgmt_c11 meta.candidates",
                },
            }, default=str) + "\n")
            st["emitted"] += 1
            if max_records and st["emitted"] >= max_records:
                break
    if verbose:
        print(json.dumps(st, indent=1), flush=True)
    return st


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--split", default="train",
                    choices=["train", "validation", "examination"])
    ap.add_argument("--out", default="")
    ap.add_argument("--max-records", type=int, default=0)
    ap.add_argument("--with-management", action="store_true",
                    help="append the 4-arm management group after the entry group")
    ap.add_argument("--management-smoke", action="store_true",
                    help="build 20 management records into a temp file and assert shape")
    ap.add_argument("--smoke", action="store_true",
                    help="20 records into a temp path; proves the wiring")
    a = ap.parse_args(argv)
    if a.smoke:
        st = build_split(a.split, out_path=a.out or "/tmp/rl_smoke.jsonl",
                         max_records=20, verbose=True)
        recs = [json.loads(l) for l in open(st["out"], encoding="utf-8")]
        bad = []
        if not recs:
            bad.append("no_records")
        for r in recs:
            # v9: venue-conditional arm space - AMM {SKIP,WATCH,BUY_FULL,BUY_SMALL},
            # curve {SKIP,WATCH,BUY_SMALL}. MID retired.
            acts = set(r["actions"])
            if acts not in ({"SKIP", "WATCH", "BUY_FULL", "BUY_SMALL"},
                            {"SKIP", "WATCH", "BUY_SMALL"}):
                bad.append("action_space_wrong")
            if abs(sum(r["advantages"].values())) > 1e-6:
                bad.append("advantages_not_zero_sum")
            if r["group"]["SKIP"] != 0.0 or r["group"]["WATCH"] != 0.0:
                bad.append("skip_watch_not_zero")
            if not r["group_usable"]:
                bad.append("group_unusable")
            if not r["prompt"].startswith("[{"):
                bad.append("prompt_not_chat_list")
            # v2 carry-through: weight carried, at 1.0 because lambda_cvar is OFF
            if r.get("group_loss_weight") != 1.0:
                bad.append("group_loss_weight_not_carried")
            if r.get("schema_version") != RECORD_SCHEMA_VERSION:
                bad.append("schema_version_not_stamped")
            cv = r.get("cvar") or {}
            if cv.get("applied") is not False or cv.get("lambda_cvar") != 0.0:
                bad.append("cvar_summary_not_identity_under_off_lambda")
            if not math.isfinite(float(cv.get("cvar_z", float("nan")))):
                bad.append("cvar_summary_nonfinite")
        # the legacy-record contract: absent field reads as 1.0, and a corrupt field is
        # refused loudly instead of being coerced into a silent 1.0
        if carried_diagnostics({}, [0.0, 0.0])[0] != 1.0:
            bad.append("legacy_absent_group_loss_weight_not_one")
        for junk in ("1.0", True, -0.5, float("nan")):
            try:
                carried_diagnostics({"group_loss_weight": junk}, [0.0, 0.0])
                bad.append(f"malformed_group_loss_weight_not_refused:{junk!r}")
            except ValueError:
                pass
        print(json.dumps({"smoke_records": len(recs), "failed": sorted(set(bad)),
                          "verdict": "PASS" if not bad else "FAIL"}, indent=1))
        return 1 if bad else 0
    if a.management_smoke:
        tmp = "/tmp/rl_mgmt_smoke.jsonl"
        if os.path.exists(tmp):
            os.remove(tmp)
        st = build_management_records(a.split, tmp, max_records=20, verbose=True)
        recs = [json.loads(x) for x in open(tmp, encoding="utf-8")] if os.path.exists(tmp) else []
        bad = []
        if not recs:
            bad.append("no_records")
        for r in recs:
            if set(r["actions"]) != set(MANAGEMENT_ACTIONS):
                bad.append("action_space_wrong")
            if abs(sum(r["advantages"].values())) > 1e-6:
                bad.append("advantages_not_zero_sum")
            if r["best_action"] != r["sft_action"]:
                bad.append("best_action_disagrees_with_sft_label")
            if not r["group_usable"]:
                bad.append("group_unusable")
            if not r["prompt"].startswith("[{"):
                bad.append("prompt_not_chat_list")
            if r.get("family") != "management_replay":
                bad.append("family_not_tagged")
            # v2 carry-through: weight carried, at 1.0 because lambda_cvar is OFF
            if r.get("group_loss_weight") != 1.0:
                bad.append("group_loss_weight_not_carried")
            if r.get("schema_version") != RECORD_SCHEMA_VERSION:
                bad.append("schema_version_not_stamped")
            cv = r.get("cvar") or {}
            if cv.get("applied") is not False or cv.get("lambda_cvar") != 0.0:
                bad.append("cvar_summary_not_identity_under_off_lambda")
            if not math.isfinite(float(cv.get("cvar_z", float("nan")))):
                bad.append("cvar_summary_nonfinite")
            if carried_diagnostics({}, r["group"].values())[0] != 1.0:
                bad.append("legacy_absent_group_loss_weight_not_one")
        print(json.dumps({"management_smoke_records": len(recs),
                          "failed": sorted(set(bad)),
                          "verdict": "PASS" if not bad else "FAIL"}, indent=1))
        return 1 if bad else 0
    build_split(a.split, out_path=a.out, max_records=a.max_records,
                with_management=a.with_management)
    return 0


if __name__ == "__main__":
    sys.exit(main())

