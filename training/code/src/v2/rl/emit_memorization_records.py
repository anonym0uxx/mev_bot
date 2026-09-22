#!/usr/bin/env python
"""emit_memorization_records - the PRODUCER for reports/MEMORIZATION_RECORDS.jsonl.

WHY THIS EXISTS. `grpo_loop.memorization_precheck()` is a FAIL-CLOSED veto on RL
checkpoint SELECTION: a candidate is selectable only with a CLEAN verdict from the
preregistered instrument (`reports/MEMORIZATION_BOUNDS.json`). The instrument was
registered before this file existed and nothing fed it, so the gate could only ever
refuse. The prereg names this producer verbatim:

  "the RL evaluation step must score matched in-sample and unseen prompts through the
   SAME policy and write reports/MEMORIZATION_RECORDS.jsonl with {prompt_sha256,
   in_sample, chosen, realized_net_bp, repeat_count}"

That is exactly what this module writes. The prereg is AUTHORITATIVE: this file does
not edit MEMORIZATION_BOUNDS.json, does not edit memorization_detector.py, and does
not relax the gate. It CALLS the detector and propagates its verdict (exit non-zero
unless the verdict is 'clean').

WHAT A RECORD IS. One scored decision from ONE policy:

  prompt_sha256   str    the SFT-faithful prompt, hashed (the join key to the RL rows)
  in_sample       bool   True = the prompt's row comes from the in-sample training file
                         (rl_train.jsonl), False = from the unseen file
                         (rl_validation.jsonl)
  chosen          str    the ARM THE POLICY EMITTED (grpo_loss.parse_decision's arm
                         vocabulary: SKIP / WATCH / BUY_SMALL / BUY_MID / BUY_FULL).
                         It is read from the POLICY's own completion, NEVER from the
                         record's `best_action` - the whole point of the instrument is
                         what the policy does, not what the counterfactual says it
                         should have done.
  realized_net_bp float  see the DEFINITION below
  repeat_count    int    how many times that prompt_sha256 appears in the IN-SAMPLE
                         training file (the detector's optional dose-response input);
                         0 for unseen rows, by construction (unseen means it does not
                         appear in training).

REALIZED NET BP, DEFINITION (the engine's own identity - nothing is invented here).
An episode carries `net_sol_returned` and `final_equity_sol`, and
`reward_terms.episode_risk` derives the capital that produced the return as

    capital = final_equity_sol - net_sol_returned        ("exact, no assumption")

so, in basis points of deployed capital,

    realized_net_bp = 10000.0 * net_sol_returned / capital

`capital` is read from `reward_terms.episode_risk(episode)["capital_sol"]` - the pinned
authority, not a second derivation of the same arithmetic. An episode whose implied
capital is <= 0 is REFUSED and COUNTED (episode_risk raises), never coerced.

SKIP and WATCH deploy NO capital, so there is no episode and no capital to divide by.
Their mechanical return is the engine's own SKIP_VALUE (0.0): the record carries
realized_net_bp = 0.0 and price_source="no_position_zero". That is the engine's value
for the arm, NOT a fabricated reward and NOT a substitute for a refusal. A score_entry
REFUSAL (no reserves, order too large, min-hold violation, implausible path, ...) is
SKIPPED and COUNTED by status - it never becomes a 0.0 and never becomes a record.

ARM SIZING AND THE EXIT POLICY (whose size, whose exit). The arm is priced AS THE
POLICY EMITTED IT: BUY_FULL/BUY_MID/BUY_SMALL deploy SIZE_FRACTIONS[tier] *
SIZE_NOTIONAL_SOL, even when that tier is not the venue's live tier - the instrument
asks what the policy's OWN action realised, not what the record's scorable arm set
would have allowed. The exit is resolved exactly as grpo_dataset.build_records
resolves it: score_entry is run for each of BUY_POLICIES and the best non-refused
episode is kept. An arm whose every policy refuses produces NO record.

MATCHING. The two groups are matched on `regime` ('amm' | 'bonding_curve'), so the
detector's statistics cannot be driven by a population difference rather than by
exposure. Sampling is stratified: candidate rows are bucketed by regime and drawn
round-robin across regimes for BOTH groups, and the achieved regime mix per group is
reported. The comparison is still a measurement over the rows the engine can price -
the report carries the per-reason skip counts so the surviving population is visible
instead of implied.

POLICY. The chosen arm comes from the candidate policy through the LOOP's own path:
`grpo_loop.rollout` over a batch built by `grpo_loop.collate`, parsed by
`grpo_loss.parse_decision`. Nothing samples here that the loop does not sample. The
loop's TEST-ONLY seam (`--test-completions` + `--device cpu`) is mirrored here so the
producer is exercisable with no accelerator: it supplies fixed completions per
prompt_sha256 and is refused on any device other than cpu.

Usage:
    emit_memorization_records.py --ckpt <policy dir> [--limit N] [--device cpu]
    emit_memorization_records.py --ckpt <tiny> --device cpu --test-completions f.json
    emit_memorization_records.py --self-check
"""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
import math
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

HERE = os.path.dirname(os.path.abspath(__file__))

DEFAULT_TRAIN = "/training/v2/rl_targets_v9/rl_train.jsonl"
DEFAULT_VALIDATION = "/training/v2/rl_targets_v9/rl_validation.jsonl"
DEFAULT_OUT = "/training/v2/reports/MEMORIZATION_RECORDS.jsonl"
DEFAULT_REPORT = "/training/v2/reports/MEMORIZATION_RECORDS_REPORT.json"
DEFAULT_PREREG = "/training/v2/reports/MEMORIZATION_BOUNDS.json"
DEFAULT_SCORE = "/training/v2/reports/MEMORIZATION_SCORE.json"

# grpo_dataset's tape path and filters, reused verbatim (one canonical tape set).
TAPES = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
TAPE_MIN_NOTIONAL_LAMPORTS = 100_000
TAPE_MAX_PX_RATIO = 50

# The arms the policy can emit (grpo_loss.parse_decision) and their pinned sizes.
POLICY_ARMS = ("SKIP", "WATCH", "BUY_SMALL", "BUY_MID", "BUY_FULL")
BUY_POLICIES = ("MOONSHOT_TAIL", "TRAIL_ONLY", "HOLD_TO_HORIZON")
HORIZON_MS = 1_800_000

REALIZED_NET_BP_DEFINITION = ("realized_net_bp = 10000.0 * net_sol_returned / capital, "
                              "capital = final_equity_sol - net_sol_returned "
                              "(reward_terms.episode_risk's exact identity)")


# ---------------------------------------------------------------------------
# row selection: stratified on regime, matched between the two groups
# ---------------------------------------------------------------------------
def load_target_rows(path: str, in_sample: bool) -> list:
    """Read the RL target rows this producer scores. REFUSES an absent/empty file."""
    if not os.path.isfile(path):
        raise SystemExit("REFUSING: target rows not found: %s" % path)
    rows, bad = [], 0
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                r = json.loads(line)
            except Exception:                                    # noqa: BLE001
                bad += 1
                continue
            if not r.get("prompt_sha256") or not r.get("mint"):
                bad += 1
                continue
            r["in_sample"] = bool(in_sample)
            rows.append(r)
    if not rows:
        raise SystemExit("REFUSING: no usable rows in %s" % path)
    return rows


def _regime_of(row: dict) -> str:
    """The matching key. Absent regime is REFUSED, never defaulted onto a stratum."""
    reg = row.get("regime")
    if reg not in ("amm", "bonding_curve"):
        return ""
    return reg


def stratified_queues(rows: list, seed: int, cap: int) -> dict:
    """{regime: [rows]} in a seeded order, then interleaved round-robin across regimes.

    A seeded shuffle makes a partial run reproducible; the round-robin interleave makes
    it REGIME-BALANCED, so a run truncated by --limit cannot become a single-stratum
    sample. Rows with no resolvable regime are dropped here and counted by the caller.
    """
    import numpy as np
    per: dict = collections.defaultdict(list)
    unkeyable = 0
    for r in rows:
        reg = _regime_of(r)
        if not reg:
            unkeyable += 1
            continue
        per[reg].append(r)
    rng = np.random.default_rng(int(seed))
    for reg in per:
        idx = rng.permutation(len(per[reg]))
        per[reg] = [per[reg][int(i)] for i in idx]
    regimes = sorted(per)
    if cap:
        per = {reg: per[reg][:cap] for reg in regimes}
    out, depth = {}, max([len(per[reg]) for reg in regimes] or [0])
    for reg in regimes:
        out[reg] = per[reg]
    interleaved = []
    for i in range(depth):
        for reg in regimes:
            if i < len(per[reg]):
                interleaved.append((reg, per[reg][i]))
    return {"per_regime": out, "interleaved": interleaved, "unkeyable": unkeyable,
            "regimes": regimes}


# ---------------------------------------------------------------------------
# pricing one chosen arm through the engine
# ---------------------------------------------------------------------------
def realized_net_bp(episode: dict) -> float:
    """The pinned identity. Raises on a non-positive implied capital - never coerced."""
    from reward_terms import episode_risk
    net = episode.get("net_sol_returned")
    if net is None or not math.isfinite(float(net)):
        raise ValueError("episode carries no finite net_sol_returned")
    capital = float(episode_risk(episode)["capital_sol"])
    return 10000.0 * float(net) / capital


def price_arm(tape, t_dec_ms: int, engine, arm: str,
              horizon_ms: int = HORIZON_MS) -> dict:
    """Realized net bp of ONE arm the policy emitted, through score_entry.

    Returns {"ok": bool, ...}. A refusal is returned as a refusal (with its status):
    the caller SKIPS and COUNTS it; it is never turned into a reward.
    """
    from size_dimension import SIZE_FRACTIONS, SIZE_NOTIONAL_SOL
    from rl_reward_v3 import score_entry
    if arm in ("SKIP", "WATCH"):
        # no position taken: the engine's own SKIP_VALUE, not a fabricated reward
        return {"ok": True, "realized_net_bp": 0.0, "price_source": "no_position_zero",
                "policy": None, "status": "no_position"}
    if arm not in POLICY_ARMS:
        return {"ok": False, "status": "unparseable_arm:%s" % arm}
    tier = arm[4:]
    frac = SIZE_FRACTIONS.get(tier)
    if frac is None:
        return {"ok": False, "status": "no_size_for_tier:%s" % tier}
    best, refused = None, []
    for pol in BUY_POLICIES:
        try:
            res = score_entry(tape, t_dec_ms, engine, policy_name=pol,
                              horizon_ms=horizon_ms,
                              deploy_sol=frac * SIZE_NOTIONAL_SOL)
        except Exception as e:                                   # noqa: BLE001
            refused.append("exception:%s" % type(e).__name__)
            continue
        if res.get("refused") or res.get("penalised") is None:
            refused.append(res.get("status") or "refused")
            continue
        ep = res.get("episode")
        if not isinstance(ep, dict):
            refused.append("no_episode")
            continue
        try:
            bp = realized_net_bp(ep)
        except ValueError:
            refused.append("nonpositive_implied_capital")
            continue
        cand = {"reward": float(res["penalised"]), "realized_net_bp": bp, "policy": pol,
                "status": res.get("status"), "held_ms": res.get("held_ms"),
                "exit_reason": res.get("exit_reason")}
        if best is None or cand["reward"] > best["reward"]:
            best = cand
    if best is None:
        return {"ok": False, "status": (refused[0] if refused else "no_priceable_policy"),
                "refused_statuses": refused[:6]}
    return {"ok": True, "realized_net_bp": float(best["realized_net_bp"]),
            "price_source": "score_entry", "policy": best["policy"],
            "status": best["status"]}


# ---------------------------------------------------------------------------
# the policy: the LOOP's rollout + parse path, nothing new
# ---------------------------------------------------------------------------
def load_policy(ckpt: str, device: str):
    """Load the candidate policy. REFUSES a missing / non-loadable directory."""
    if not ckpt or not os.path.isdir(ckpt):
        raise SystemExit("REFUSING: --ckpt %r is not a directory" % ckpt)
    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer
    try:
        tok = AutoTokenizer.from_pretrained(ckpt, local_files_only=True)
    except Exception as e:                                       # noqa: BLE001
        raise SystemExit("REFUSING: tokenizer not loadable from %s: %s: %s"
                         % (ckpt, type(e).__name__, str(e)[:200]))
    try:
        model = AutoModelForCausalLM.from_pretrained(
            ckpt, local_files_only=True,
            torch_dtype=(torch.float32 if device == "cpu" else torch.bfloat16))
    except Exception as e:                                       # noqa: BLE001
        raise SystemExit("REFUSING: model not loadable from %s: %s: %s"
                         % (ckpt, type(e).__name__, str(e)[:200]))
    if tok.pad_token_id is None and tok.eos_token_id is None:
        raise SystemExit("REFUSING: tokenizer has neither a pad nor an eos token")
    tok.padding_side = "right"
    model.to(device)
    model.eval()
    return model, tok


def chosen_arms_from_policy(model, tok, rows: list, cfg: dict, device: str,
                            completions=None, batch_size: int = 4) -> dict:
    """prompt_sha256 -> the policy's arm (modal over the loop's own rollout).

    Reuses grpo_loop.RLDataset's item construction, grpo_loop.collate and
    grpo_loop.rollout, so the sampled completion path is the training loop's, not a
    second implementation of it. A prompt with NO parseable completion is absent from
    the result and is COUNTED by the caller (never defaulted to an arm).
    """
    import collections as _c
    import tempfile
    import torch                                                # noqa: F401
    import grpo_loop
    from grpo_loss import ARM_ORDER

    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False,
                                     encoding="utf-8") as fh:
        tmp = fh.name
        for r in rows:
            fh.write(json.dumps(r) + "\n")
    try:
        ds = grpo_loop.RLDataset(tmp, tok)
        if len(ds) != len(rows):
            # RLDataset drops rows without group_usable: that is a real population
            # change, so it is reported rather than silently absorbed.
            pass
        arms: dict = {}
        order = list(range(len(ds)))
        for i in range(0, len(order), batch_size):
            batch = grpo_loop.collate([ds[j] for j in order[i:i + batch_size]],
                                      pad_id=(tok.pad_token_id or tok.eos_token_id))
            out = grpo_loop.rollout(model, tok, batch, cfg, device,
                                    completions=completions)
            for item in out:
                sha = batch["meta"][item["prompt_index"]]["prompt_sha256"]
                good = [a for a in item["actions"] if a in POLICY_ARMS]
                if not good:
                    continue
                # deterministic mode: majority arm, ties broken by ARM_ORDER
                cnt = _c.Counter(good)
                top = max(cnt.values())
                arms[sha] = sorted([a for a, c in cnt.items() if c == top],
                                   key=ARM_ORDER.index)[0]
        return {"arms": arms, "n_rows": len(ds)}
    finally:
        try:
            os.remove(tmp)
        except OSError:
            pass


def load_test_completions(path: str):
    """TEST-ONLY seam (mirrors grpo_loop.load_test_completions). Refused off cpu."""
    if not os.path.isfile(path):
        raise SystemExit("REFUSING: --test-completions file not found: %s" % path)
    raw = json.load(open(path, encoding="utf-8"))
    comp = {}
    for k, v in raw.items():
        comp[k] = v if isinstance(v, list) else [v]
    if not comp:
        raise SystemExit("REFUSING: --test-completions file is empty: %s" % path)
    return comp, hashlib.sha256(open(path, "rb").read()).hexdigest()


# ---------------------------------------------------------------------------
# the producer
# ---------------------------------------------------------------------------
def write_records(records: list, out_path: str) -> str:
    """Write the records atomically (a torn file must never be scored)."""
    os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
    tmp = out_path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        for r in records:
            fh.write(json.dumps(r, default=str) + "\n")
    os.replace(tmp, out_path)
    return out_path


def make_record(row: dict, chosen: str, bp_result: dict, repeat_count: int,
                provenance: dict) -> dict:
    """One record. A missing/None/non-finite realized_net_bp is REFUSED here, so a
    refusal can never be silently scored by the detector as a 0.0 return."""
    bp = bp_result.get("realized_net_bp")
    if bp is None or isinstance(bp, bool) or not isinstance(bp, (int, float)) \
            or not math.isfinite(float(bp)):
        raise ValueError("REFUSING: record would carry realized_net_bp=%r - a missing "
                         "value must never become 0.0" % (bp,))
    if chosen not in POLICY_ARMS:
        raise ValueError("REFUSING: chosen arm %r is not in the policy's arm space"
                         % (chosen,))
    return {
        # --- the preregistered five -----------------------------------------
        "prompt_sha256": row["prompt_sha256"],
        "in_sample": bool(row["in_sample"]),
        "chosen": chosen,
        "realized_net_bp": float(bp),
        "repeat_count": int(repeat_count),
        # --- diagnostics (the detector ignores every field it does not name) --
        "episode_id": row.get("episode_id"), "mint": row.get("mint"),
        "t_dec_ms": row.get("t_dec_ms"), "split": row.get("split"),
        "regime": row.get("regime"), "repeat_count_meaning":
            ("occurrences of this prompt_sha256 in the in-sample training file"
             if row["in_sample"] else "unseen: 0 by construction"),
        "price_source": bp_result.get("price_source"),
        "exit_policy": bp_result.get("policy"),
        "price_status": bp_result.get("status"),
        "realized_net_bp_definition": REALIZED_NET_BP_DEFINITION,
        "provenance": provenance,
    }


def produce(*, ckpt: str = "", model=None, tokenizer=None, train_file: str = DEFAULT_TRAIN,
            validation_file: str = DEFAULT_VALIDATION, out: str = DEFAULT_OUT,
            limit: int = 0, device: str = "cpu", seed: int = 7, report_path: str = "",
            completions=None, test_seam_sha: str = "", batch_size: int = 4,
            group_size: int = 3, temperature: float = 0.7, top_p: float = 0.95,
            max_new_tokens: int = 256, run_detector: bool = True,
            prereg: str = DEFAULT_PREREG, score_out: str = DEFAULT_SCORE,
            cpk_provenance: dict | None = None, verbose: bool = True,
            write: bool = True) -> dict:
    """Score matched in-sample / unseen prompts through ONE policy and write the records.

    `model`/`tokenizer` may be supplied (the live RL loop passes the candidate it is
    already holding, so the 27B is never reloaded); otherwise they are loaded from
    `--ckpt`.

    `write=False` runs the WHOLE measurement and returns the stats but touches no file.
    The live multi-rank path needs it: rollout's decode loop carries collectives, so a
    non-main rank must still execute the generation, but only ONE rank may write.
    """
    if not os.path.isfile(prereg):
        raise SystemExit("REFUSING: no preregistered instrument at %s" % prereg)
    if not os.path.isfile(train_file) or not os.path.isfile(validation_file):
        raise SystemExit("REFUSING: target rows missing: %s / %s"
                         % (train_file, validation_file))
    if model is None or tokenizer is None:
        if not ckpt:
            raise SystemExit("REFUSING: --ckpt is required (no model supplied)")
        model, tokenizer = load_policy(ckpt, device)

    stats: dict = {"train_file": train_file, "validation_file": validation_file,
                   "out": out, "limit": int(limit), "seed": int(seed),
                   "device": device, "ckpt": ckpt,
                   "realized_net_bp_definition": REALIZED_NET_BP_DEFINITION,
                   "skipped": {}, "skipped_total": 0}

    seen_rows = load_target_rows(train_file, True)
    unseen_rows = load_target_rows(validation_file, False)
    # dose-response input: how often each in-sample prompt actually appeared
    repeats = collections.Counter(r["prompt_sha256"] for r in seen_rows)
    stats["in_sample_rows_in_file"] = len(seen_rows)
    stats["unseen_rows_in_file"] = len(unseen_rows)
    stats["in_sample_rows_with_group_usable"] = sum(1 for r in seen_rows
                                                    if r.get("group_usable"))
    # rows the RL dataset itself would mask are not scorable: count, do not sample
    sel = []
    masked = 0
    for r in seen_rows + unseen_rows:
        if not r.get("group_usable"):
            masked += 1
            continue
        sel.append(r)
    stats["skipped"]["row_not_group_usable"] = masked

    q_in = stratified_queues([r for r in sel if r["in_sample"]], seed, cap=0)
    q_un = stratified_queues([r for r in sel if not r["in_sample"]], seed + 1, cap=0)
    stats["skipped"]["row_no_regime"] = q_in["unkeyable"] + q_un["unkeyable"]
    stats["regimes_in_sample"] = q_in["regimes"]
    stats["regimes_unseen"] = q_un["regimes"]
    for reg in sorted(set(q_in["regimes"]) | set(q_un["regimes"])):
        stats.setdefault("available_rows", {})[reg] = {
            "in_sample": len(q_in["per_regime"].get(reg, [])),
            "unseen": len(q_un["per_regime"].get(reg, []))}

    per_group_limit = int(limit) if limit else 0
    # interleave the two groups by regime so a truncated run stays regime-matched
    picks = {"in_sample": [], "unseen": []}
    cur = {"in_sample": 0, "unseen": 0}
    queues = {"in_sample": q_in["interleaved"], "unseen": q_un["interleaved"]}
    exhausted = {"in_sample": False, "unseen": False}
    while True:
        progressed = False
        for g in ("in_sample", "unseen"):
            if exhausted[g] or (per_group_limit and len(picks[g]) >= per_group_limit):
                continue
            if cur[g] >= len(queues[g]):
                exhausted[g] = True
                continue
            reg, row = queues[g][cur[g]]
            cur[g] += 1
            picks[g].append(row)
            progressed = True
        if not progressed or all(exhausted[g]
                                 or (per_group_limit
                                     and len(picks[g]) >= per_group_limit)
                                 for g in ("in_sample", "unseen")):
            break

    to_score = picks["in_sample"] + picks["unseen"]
    stats["sampled"] = {"in_sample": len(picks["in_sample"]),
                        "unseen": len(picks["unseen"])}
    if not to_score:
        raise SystemExit("REFUSING: no candidate rows to score (both target files "
                         "empty after filtering)")

    cfg = {"loop": {"group_size": int(group_size), "temperature": float(temperature),
                    "top_p": float(top_p), "max_new_tokens": int(max_new_tokens)}}
    res = chosen_arms_from_policy(model, tokenizer, to_score, cfg, device,
                                  completions=completions,
                                  batch_size=int(batch_size))
    arms = res["arms"]
    stats["policy_rows_scored"] = res["n_rows"]

    # ---- tapes, loaded only for the mints actually sampled -------------------
    from rl_reward_v3 import V3Engine, ReserveRegistry, STALE_ANY, load_canonical_tapes
    mints = sorted({r["mint"] for r in to_score if r.get("mint")})
    tapes = load_canonical_tapes(TAPES, mints=set(mints),
                                 min_notional_lamports=TAPE_MIN_NOTIONAL_LAMPORTS,
                                 max_px_ratio=TAPE_MAX_PX_RATIO)
    reg_obj = ReserveRegistry.build(verbose=False)
    engine = V3Engine(reg_obj, max_reserve_stale_ms=STALE_ANY, deploy_sol=1.0)

    def _skip(reason: str):
        stats["skipped"][reason] = stats["skipped"].get(reason, 0) + 1
        stats["skipped_total"] += 1

    provenance = {"ckpt": ckpt or (cpk_provenance or {}).get("ckpt") or "in-memory",
                  "records_producer": "emit_memorization_records.py",
                  "policy_path": "grpo_loop.rollout -> grpo_loss.parse_decision",
                  "scored_through": "rl_reward_v3.score_entry",
                  "test_seam": ({"enabled": True, "file_sha256": test_seam_sha,
                                 "note": "TEST-ONLY fixed completions; not a real "
                                         "policy sample"}
                                if completions else {"enabled": False})}
    if cpk_provenance:
        provenance.update(cpk_provenance)

    records, mix = [], {"in_sample": collections.Counter(), "unseen": collections.Counter()}
    for row in to_score:
        g = "in_sample" if row["in_sample"] else "unseen"
        sha = row["prompt_sha256"]
        arm = arms.get(sha)
        if arm is None:
            _skip("policy_emitted_no_parseable_action")
            continue
        tape = tapes.get(row["mint"])
        if tape is None:
            _skip("no_tape_for_mint")
            continue
        t_dec = row.get("t_dec_ms")
        if t_dec is None:
            _skip("row_has_no_t_dec_ms")
            continue
        t_dec = int(t_dec)
        # grpo_dataset's exact window rule: a decision outside the recorded tape
        # window cannot be priced at all - it is a skip, not an engine refusal.
        if not (int(tape.tt[0]) <= t_dec <= int(tape.tt[-1])):
            _skip("outside_tape_window")
            continue
        pr = price_arm(tape, t_dec, engine, arm, horizon_ms=HORIZON_MS)
        if not pr.get("ok"):
            _skip("refused:%s" % pr.get("status"))
            continue
        try:
            rec = make_record(row, arm, pr,
                              (repeats.get(sha, 0) if row["in_sample"] else 0),
                              provenance)
        except ValueError as e:                                  # noqa: BLE001
            _skip("record_refused:%s" % type(e).__name__)
            continue
        records.append(rec)
        mix[g][rec["regime"]] += 1

    n_in = sum(1 for r in records if r["in_sample"])
    n_un = len(records) - n_in
    stats.update({"written": len(records), "n_in_sample": n_in, "n_unseen": n_un,
                  "regime_mix": {"in_sample": dict(mix["in_sample"]),
                                 "unseen": dict(mix["unseen"])}})
    # ---- fail-closed refusals -------------------------------------------------
    if n_in < 30 or n_un < 30:
        stats["status"] = "REFUSED_insufficient_groups"
        top = sorted(((k, v) for k, v in stats["skipped"].items() if v),
                     key=lambda kv: -kv[1])[:8]
        stats["reason"] = ("%d in-sample / %d unseen priced rows; the detector needs "
                           ">=30 per group. Not writing a report the gate cannot use. "
                           "sampled=%s skips=%s"
                           % (n_in, n_un, json.dumps(stats.get("sampled")),
                              json.dumps(dict(top))))
        _write_report(stats, report_path)
        raise SystemExit("REFUSING: %s" % stats["reason"])

    # The prereg's floor is 30/group and is REFUSED below; the configured `limit` is
    # this producer's own POWER choice, so a shortfall against it is REPORTED (never
    # silently absorbed, never made fatal): the verdict is still a valid verdict at
    # 30+ rows, and blocking selection over 10 missing rows would be dogma.
    if limit:
        stats["limit_requested"] = int(limit)
        stats["limit_shortfall"] = {"in_sample": max(0, int(limit) - n_in),
                                    "unseen": max(0, int(limit) - n_un)}
    stats["written"] = len(records)
    if not write:
        stats["status"] = "measured_no_write"
        return stats
    write_records(records, out)
    stats["status"] = "written"

    # ---- the detector's OWN verdict is the acceptance test --------------------
    if run_detector:
        verdict = run_detector_subprocess(out, prereg, score_out)
        stats["detector"] = verdict
        stats["detector_rc"] = verdict.get("rc")
        _write_report(stats, report_path)
        return stats

    _write_report(stats, report_path)
    return stats


def run_detector_subprocess(records_path: str, prereg: str, score_out: str) -> dict:
    """Call the detector. The producer does NOT reimplement or second-guess its math."""
    cmd = [sys.executable, os.path.join(HERE, "memorization_detector.py"),
           "--score", records_path, "--prereg", prereg, "--out", score_out]
    rc = subprocess.run(cmd, capture_output=True, text=True)
    try:
        rep = json.load(open(score_out, encoding="utf-8"))
    except Exception:                                            # noqa: BLE001
        rep = {}
    rep["rc"] = rc.returncode
    rep["verdict_ok"] = bool(rc.returncode == 0 and rep.get("status") == "clean")
    rep["command"] = " ".join(cmd)
    if rc.returncode != 0:
        rep.setdefault("reason", (rc.stdout or rc.stderr or "")[-400:])
    return rep


def _write_report(stats: dict, report_path: str):
    if not report_path:
        return
    os.makedirs(os.path.dirname(report_path) or ".", exist_ok=True)
    tmp = report_path + ".tmp"
    json.dump(stats, open(tmp, "w", encoding="utf-8"), indent=1, sort_keys=True,
              default=str)
    os.replace(tmp, report_path)


# ---------------------------------------------------------------------------
# CPU-only self-check: no 27B, no GPU, detector's verdict as the acceptance test
# ---------------------------------------------------------------------------
def _synth(n: int, in_sample: bool, arm_fn, net_fn, seed: int,
           repeat_fn=None) -> list:
    import numpy as np
    rng = np.random.default_rng(seed)
    out = []
    for i in range(n):
        sha = hashlib.sha256(("s%d%d%d" % (seed, in_sample, i)).encode()).hexdigest()
        out.append({"prompt_sha256": sha, "in_sample": bool(in_sample),
                    "chosen": arm_fn(rng), "realized_net_bp": net_fn(rng),
                    "repeat_count": (repeat_fn(rng) if repeat_fn else
                                     (int(rng.integers(1, 9)) if in_sample else 0))})
    return out


def _self_check() -> int:
    import tempfile
    import numpy as np
    fails = []

    def chk(tag, cond, extra=""):
        if not cond:
            fails.append(tag if not extra else "%s %s" % (tag, extra))

    d = tempfile.mkdtemp(prefix="mem_selfcheck_")
    acts = ["BUY_FULL", "WATCH", "SKIP"]

    def score(recs, tag):
        p = os.path.join(d, "%s.jsonl" % tag)
        write_records(recs, p)
        rep = run_detector_subprocess(p, DEFAULT_PREREG, os.path.join(d, "%s.json" % tag))
        return rep, p

    # (a) an HONEST policy: choices independent of exposure, no outcome gap
    honest = (
        _synth(400, True, lambda r: str(r.choice(acts)), lambda r: float(r.normal(10, 120)), 7)
        + _synth(400, False, lambda r: str(r.choice(acts)),
                 lambda r: float(r.normal(10, 120)), 11))
    h, hpath = score(honest, "honest")
    chk("honest_is_clean", h.get("status") == "clean", json.dumps(h)[:200])
    chk("honest_rc_zero", h.get("rc") == 0, "rc=%s" % h.get("rc"))

    # (b) a MEMORIZER: in-sample it repeats one arm and its realized return is far
    # better there. The detector must FLAG it - passing the honest arm is not enough.
    memo = (
        _synth(400, True, lambda r: "BUY_FULL", lambda r: float(r.normal(400, 80)), 7)
        + _synth(400, False, lambda r: str(r.choice(acts)),
                 lambda r: float(r.normal(-30, 120)), 11))
    m, mpath = score(memo, "memorizer")
    chk("memorizer_flagged", m.get("status") == "MEMORIZATION_SUSPECTED",
        json.dumps(m)[:300])
    chk("memorizer_flags_both_signals",
        bool(m.get("tv_exceeds")) and bool(m.get("outcome_gap_exceeds")),
        json.dumps(m)[:300])
    chk("memorizer_rc_nonzero", m.get("rc") == 1, "rc=%s" % m.get("rc"))

    # (c) thin groups must REFUSE, not pass
    thin = ([_synth(5, True, lambda r: "SKIP", lambda r: 1.0, 7)]
            + [_synth(5, False, lambda r: "SKIP", lambda r: 1.0, 11)])
    t, _tp = score(_flatten(thin), "thin")
    chk("thin_groups_refuse", t.get("status") == "insufficient_groups",
        json.dumps(t)[:200])
    chk("thin_rc_nonzero", t.get("rc") == 1, "rc=%s" % t.get("rc"))

    # (d) a MISSING realized_net_bp must not silently become 0.0
    try:
        make_record({"prompt_sha256": "x", "in_sample": True, "mint": "m",
                     "t_dec_ms": 1, "regime": "amm"}, "SKIP",
                    {"realized_net_bp": None}, 1, {})
        chk("missing_bp_refused", False, "make_record accepted a null realized_net_bp")
    except ValueError:
        chk("missing_bp_refused", True)
    for junk in (float("nan"), float("inf"), True, "0.0"):
        try:
            make_record({"prompt_sha256": "x", "in_sample": True, "mint": "m",
                         "t_dec_ms": 1, "regime": "amm"}, "SKIP",
                        {"realized_net_bp": junk}, 1, {})
            chk("malformed_bp_refused", False, "accepted %r" % (junk,))
        except ValueError:
            chk("malformed_bp_refused", True)
    # and a NULL is READ as null by the detector, not as 0.0: the gap statistic
    # drops it (it needs >=10 non-null per group), so the outcome gap stays 0.
    nulled = ([{"prompt_sha256": hashlib.sha256(("n%d" % i).encode()).hexdigest(),
                "in_sample": True, "chosen": "SKIP", "realized_net_bp": None,
                "repeat_count": 1} for i in range(60)]
              + [{"prompt_sha256": hashlib.sha256(("u%d" % i).encode()).hexdigest(),
                  "in_sample": False, "chosen": "SKIP", "realized_net_bp": 500.0,
                  "repeat_count": 0} for i in range(60)])
    nr, _nrp = score(nulled, "nulled")
    chk("null_bp_not_read_as_zero", float(nr.get("outcome_gap_bp", -1)) == 0.0,
        "outcome_gap_bp=%s (a null must not be folded in as 0.0)"
        % nr.get("outcome_gap_bp"))
    chk("null_bp_group_counts_present", int(nr.get("n_in_sample", 0)) == 60,
        json.dumps(nr)[:160])

    # identity check on the pinned definition
    from reward_terms import episode_risk
    ep = {"net_sol_returned": 0.5, "final_equity_sol": 1.5,
          "equity_path": [[0, 1.0], [1, 1.5]]}
    chk("realized_net_bp_identity",
        abs(realized_net_bp(ep) - 10000.0 * 0.5 / float(episode_risk(ep)["capital_sol"]))
        < 1e-9)

    print(json.dumps({"suite": "emit_memorization_records", "failed": len(fails),
                      "failures": fails,
                      "honest": {k: h.get(k) for k in ("status", "rc", "tv_distance",
                                                       "tv_bound")},
                      "memorizer": {k: m.get(k) for k in ("status", "rc", "tv_distance",
                                                          "tv_bound", "outcome_gap_bp",
                                                          "outcome_gap_bound_bp")},
                      "thin": {"status": t.get("status"), "rc": t.get("rc")},
                      "null_bp": {"outcome_gap_bp": nr.get("outcome_gap_bp"),
                                  "n_in_sample": nr.get("n_in_sample")}},
                     indent=1))
    return 1 if fails else 0


def _flatten(x):
    return [r for part in x for r in part]


# ---------------------------------------------------------------------------
def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ckpt", default="", help="candidate policy directory")
    ap.add_argument("--limit", type=int, default=0,
                    help="max rows PER GROUP (0 = all available)")
    ap.add_argument("--out", default=DEFAULT_OUT)
    ap.add_argument("--report", default=DEFAULT_REPORT)
    ap.add_argument("--prereg", default=DEFAULT_PREREG)
    ap.add_argument("--score-out", default=DEFAULT_SCORE)
    ap.add_argument("--train-file", default=DEFAULT_TRAIN)
    ap.add_argument("--validation-file", default=DEFAULT_VALIDATION)
    ap.add_argument("--device", default="cpu", choices=["cpu", "cuda"])
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--group-size", type=int, default=3)
    ap.add_argument("--temperature", type=float, default=0.7)
    ap.add_argument("--top-p", type=float, default=0.95)
    ap.add_argument("--max-new-tokens", type=int, default=256)
    ap.add_argument("--batch-size", type=int, default=4)
    ap.add_argument("--no-detector", action="store_true",
                    help="write records but do not run the verdict (not a pass)")
    ap.add_argument("--test-completions", default="",
                    help="TEST-ONLY: fixed completions per prompt_sha256, "
                         "refused unless --device cpu")
    ap.add_argument("--self-check", action="store_true")
    a = ap.parse_args(argv)

    if a.self_check:
        return _self_check()

    if a.test_completions and a.device != "cpu":
        raise SystemExit("REFUSING: --test-completions is a TEST seam: refused on %s"
                         % a.device)
    comp, seam_sha = (load_test_completions(a.test_completions)
                      if a.test_completions else (None, ""))
    st = produce(ckpt=a.ckpt, train_file=a.train_file, validation_file=a.validation_file,
                 out=a.out, limit=a.limit, device=a.device, seed=a.seed,
                 report_path=a.report, completions=comp, test_seam_sha=seam_sha,
                 batch_size=a.batch_size, group_size=a.group_size,
                 temperature=a.temperature, top_p=a.top_p,
                 max_new_tokens=a.max_new_tokens, run_detector=not a.no_detector,
                 prereg=a.prereg, score_out=a.score_out)
    v = st.get("detector") or {}
    print(json.dumps({"out": a.out, "written": st.get("written"),
                      "n_in_sample": st.get("n_in_sample"),
                      "n_unseen": st.get("n_unseen"),
                      "regime_mix": st.get("regime_mix"),
                      "skipped": st.get("skipped_total"),
                      "verdict": v.get("status"), "detector_rc": v.get("rc")},
                     indent=1, default=str))
    if a.no_detector:
        print("NOT A PASS: --no-detector wrote records but ran no verdict; the gate "
              "still needs memorization_detector.py --score to return rc=0")
        return 3
    if not v.get("verdict_ok"):
        print("FAIL-CLOSED: verdict is %r (rc=%s) - selection must refuse this "
              "checkpoint" % (v.get("status"), v.get("rc")))
        return 1
    print("CLEAN: the detector found no exposure effect at the %s rows scored"
          % st.get("written"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
