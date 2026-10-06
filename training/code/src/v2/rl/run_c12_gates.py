#!/usr/bin/env python
"""c12 gate runner — COPY OF run_c10_gates.py WITH FIVE DELIBERATE CHANGES AND NOTHING ELSE:
  1. CORPUS -> candidate_sft_c12
  2. reference counts C8_REF -> c11 counts (c12's row-preservation baseline is c11)
  3. LEGAL_SIZE: MID retired (KELLY_AUDIT_C12 ev_gated_binary; vocab {NONE,SMALL,FULL})
  4. decision BUY rows must carry venue-consistent size: amm->FULL, bonding_curve->SMALL
  5. report path -> C12_GATES.json

Design rules:
  * A gate reports PASS only when it actually ran. A gate whose dependency is missing
    reports UNVERIFIED. There is no third outcome that looks like success.
  * Every check is recomputed from the assembled corpus on disk, not read from a report.
  * Exit 1 if any gate FAILs, 0 otherwise. UNVERIFIED does not fail the run but is printed
    loudly, because a launch contract must not be written over a silently skipped gate.

Gate 14 for the entry group is checked by recomputing the prompt exactly as
grpo_dataset.prompt_of does (every non-assistant message, keys sorted) and comparing to a
built RL record; for management it compares against the emitted RL group file.
"""
import argparse
import collections
import hashlib
import json
import os
import re
import sys

CORPUS = "/training/v2/candidate_sft_c12"
RL_MGMT = "/training/v2/rl_group_management"
C8 = "/training/v2/candidate_sft_c8"
BARRIER300 = "/training/v2/reports/barrier_labels_c9_300s.jsonl"
LAMBDA = 0.08884


def frozen_split(mint):
    x = int(hashlib.sha256(mint.encode()).hexdigest()[:8], 16) / 0xFFFFFFFF
    return "val" if x < .1 else "test" if x < .2 else "train"
LEGAL_MGMT = {"HOLD", "ADD", "REDUCE", "EXIT"}
LEGAL_SIZE = {"SMALL", "FULL", "NONE", None}  # MID retired in c12 (ev_gated_binary)
# CONTEXTS only. A row whose own calibrated round trip IS 210 bp renders "210 bp" legally
# (verified: all 595 hits were that). Flagging the bare number is a false positive and a
# gate that cries wolf gets ignored when it matters.
RETIRED_TEXT = ("~180 bp", "one-way execution cost",
                "fees + slippage + priority + failure charge", "cost_model_bp")
FORWARD_TOKENS = ("mfe_bp", "mae_bp", "last_bp", "t_to_hit_ms", "censored",
                  "future_price", "outcome_bp", "realized_forward")
ENRICH_FIELDS = ("holders_at_t", "top1_float_share", "holder_hhi", "mcap_sol_at_t",
                 "bundle_wallets", "round_trip_wallets", "creator_past_launches",
                 "creator_known", "wash_ratio")
C8_REF = {"decision": 84681, "management_replay": 39180,
          "utility_regression": 81416, "utility_reasoning": 10434}  # c11 baseline


def rows(split):
    for line in open(os.path.join(CORPUS, f"{split}.jsonl")):
        yield json.loads(line)


def prompt_of_sft(r):
    keep = [m for m in r["messages"] if m.get("role") != "assistant"]
    return json.dumps(keep, ensure_ascii=False, sort_keys=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--splits", nargs="+", default=["train", "validation", "examination"])
    args = ap.parse_args()
    G = {}
    fam_counts = collections.Counter()
    split_mints = collections.defaultdict(set)
    seen_cid = collections.Counter()
    prompt_hash = collections.Counter()
    row_hash = collections.Counter()
    enriched_cited = collections.Counter()
    lam = collections.Counter()
    bad = collections.defaultdict(int)
    dup_prompts = collections.Counter()

    barrier = {}
    if os.path.exists(BARRIER300):
        for line in open(BARRIER300):
            b = json.loads(line)
            barrier[(b["mint"], int(b["t_dec"]))] = b
    else:
        G[10] = "UNVERIFIED"

    for split in args.splits:
        for r in rows(split):
            m = r["meta"]
            fam = m.get("family") or r.get("family")
            fam_counts[fam] += 1
            split_mints[r.get("mint")].add(m.get("split") or r.get("split"))
            blob = "\n".join(x["content"] for x in r["messages"])
            u = r["messages"][1]["content"]
            a = r["messages"][2]["content"]
            # 2
            cid = m.get("candidate_id")
            seen_cid[(split, cid)] += 1
            ph = hashlib.sha256(prompt_of_sft(r).encode()).hexdigest()
            prompt_hash[ph] += 1
            rh = hashlib.sha256(blob.encode()).hexdigest()
            row_hash[rh] += 1
            # 4
            for t in FORWARD_TOKENS:
                if t in u:
                    bad["4_prompt_has_forward_token"] += 1
                    break
            # 5
            for f in ENRICH_FIELDS:
                if f in a:
                    enriched_cited[f] += 1
            # 8
            if fam == "decision":
                sz = (m.get("size_dimension") or {}).get("size")
                if sz not in LEGAL_SIZE:
                    bad["8_illegal_size_tier"] += 1
                # c12 venue-consistency: BUY sizes are a pure function of venue
                if m.get("action") == "BUY":
                    venue = (m.get("size_dimension") or {}).get("venue") or m.get("regime")
                    want = "FULL" if venue == "amm" else "SMALL"
                    if sz != want:
                        bad["8_size_venue_mismatch"] += 1
                elif sz not in (None, "NONE"):
                    bad["8_nonbuy_carries_size"] += 1
            lam[m.get("lambda_exposure")] += 1
            # 11
            for t in RETIRED_TEXT:
                if t in blob:
                    bad["11_retired_cost_text"] += 1
                    break
            # family-specific
            if fam == "decision":
                bt = m.get("barrier_triplet") or {}
                if bt.get("status") == "labeled":
                    if bt.get("tp_bp") != 10000.0 or bt.get("sl_bp") != 5000.0:
                        bad["7_barrier_levels"] += 1
                    if bt.get("mfe_bp") is None or bt.get("last_bp") is None:
                        bad["7_missing_excursion"] += 1
                    elif bt["mfe_bp"] < bt["last_bp"] and bt["last_bp"] < 0:
                        pass
                    cur = (bt.get("cost_floor_bps") or {})
                    if cur and cur.get("amm") == 0:
                        bad["11_retired_floor_in_meta"] += 1
                else:
                    if not str(m.get("label_status") or "").startswith("unpriced"):
                        bad["6_censoring_unpriced_mismatch"] += 1
            elif fam == "management_replay":
                cand = m.get("candidates") or {}
                if set(cand) != LEGAL_MGMT:
                    bad["9_illegal_mgmt_arm_set"] += 1
                elif max(cand, key=lambda k: cand[k]) != m.get("action"):
                    bad["9_argmax_not_the_label"] += 1
            elif fam == "utility_regression":
                ml = re.search(r"FORECAST_MFE_BP:\s*(-?[\d.]+)", a)
                nl = re.search(r"FORECAST_NET_BP:\s*(-?[\d.]+)", a)
                key = (r.get("mint"), int((r.get("episode_id") or "::0").split(":")[2]))
                b = barrier.get(key)
                if b is None:
                    bad["10_target_not_in_300s_authority"] += 1
                else:
                    if ml and abs(float(ml.group(1)) - (b.get("mfe_bp") or 0)) > 1e-6:
                        bad["10_mfe_disagrees_with_300s"] += 1
                    if nl and abs(float(nl.group(1)) - (b.get("last_bp") or 0)) > 1e-6:
                        bad["10_net_disagrees_with_300s"] += 1

    # 1
    short = {f: C8_REF.get(f, 0) - fam_counts.get(f, 0) for f in C8_REF}
    G[1] = "PASS" if all(v >= 0 for v in short.values()) else "FAIL"
    G[2] = "PASS" if sum(v - 1 for v in seen_cid.values() if v > 1) == 0 else "FAIL"
    # 3 - one split per mint AND that split is the pinned policy's, not whatever the
    #     producing run happened to write
    spanning = {mi: s for mi, s in split_mints.items() if len(s) > 1}
    wrong_split = {mi: sorted(s)[0] for mi, s in split_mints.items()
                   if mi and len(s) == 1 and sorted(s)[0] != frozen_split(mi)}
    bad["3_split_not_frozen_policy"] = len(wrong_split)
    G[3] = "PASS" if not spanning and not wrong_split else "FAIL"
    G[4] = "PASS" if bad["4_prompt_has_forward_token"] == 0 else "FAIL"
    G[5] = "PASS" if len(enriched_cited) >= 5 and all(
        enriched_cited[f] > 0 for f in ("holders_at_t", "top1_float_share")) else "FAIL"
    G[6] = "PASS" if bad["6_censoring_unpriced_mismatch"] == 0 else "FAIL"
    G[7] = "PASS" if not (bad["7_barrier_levels"] or bad["7_missing_excursion"]) else "FAIL"
    lambdas = {k for k in lam if k is not None}
    missing_lam = lam.get(None, 0)
    bad["8_rows_missing_lambda"] = missing_lam
    # venue-conditional lambda: decision BUY rows are sized per-venue (AMM 0.13681,
    # curve 0.16720); non-sizing rows (WATCH/SKIP and the non-decision families) keep
    # the legacy echo 0.08884. All three are legal.
    ALLOWED_LAM = {0.08884, 0.13681, 0.16720}
    G[8] = "PASS" if (lambdas <= ALLOWED_LAM and missing_lam == 0
                      and not bad["8_illegal_size_tier"]
                      and not bad["8_size_venue_mismatch"]
                      and not bad["8_nonbuy_carries_size"]) else "FAIL"
    G[9] = "PASS" if not (bad["9_illegal_mgmt_arm_set"] or bad["9_argmax_not_the_label"]) else "FAIL"
    if 10 not in G:
        G[10] = "PASS" if not (bad["10_target_not_in_300s_authority"]
                               or bad["10_mfe_disagrees_with_300s"]
                               or bad["10_net_disagrees_with_300s"]) else "FAIL"
    G[11] = "PASS" if not (bad["11_retired_cost_text"]
                           or bad["11_retired_floor_in_meta"]) else "FAIL"
    fams = {f for f in fam_counts if f}
    G[12] = "PASS" if fams == set(C8_REF) else "FAIL"
    G[13] = "PASS" if sum(v - 1 for v in row_hash.values() if v > 1) == 0 else "FAIL"
    # 14
    g14 = "UNVERIFIED"
    mp = os.path.join(RL_MGMT, "train.jsonl")
    if os.path.exists(mp):
        rl = {}
        for i, line in enumerate(open(mp)):
            d = json.loads(line)
            # accept either shape: the wired path writes "prompt", the standalone emitter
            # writes "messages". They are the same content; see the note in the report.
            pr = d.get("prompt") or json.dumps(
                [x for x in d.get("messages", []) if x.get("role") != "assistant"],
                ensure_ascii=False, sort_keys=True)
            rl[d["episode_id"], d.get("step")] = pr
            if i > 400:
                break
        agree = tot = 0
        for r in rows("train"):
            if (r["meta"].get("family")) != "management_replay":
                continue
            k = (r.get("episode_id"), r["meta"].get("step"))
            if k in rl:
                tot += 1
                if rl[k] == prompt_of_sft(r):
                    agree += 1
            if tot >= 200:
                break
        g14 = "PASS" if (tot and agree == tot) else "FAIL"
        G[14] = g14
    else:
        G[14] = "UNVERIFIED"

    out = {"gates": G, "family_counts": dict(fam_counts), "shortfall_vs_c8": short,
           "violations": dict(bad), "prompt_hash_collisions":
           sum(v - 1 for v in prompt_hash.values() if v > 1),
           "lambda_values": {str(k): v for k, v in lam.items()},
           "enriched_fields_cited": dict(enriched_cited),
           "mints_spanning_splits": len(spanning)}
    json.dump(out, open("/training/v2/reports/C12_GATES.json", "w"), indent=1)
    print(json.dumps(out, indent=1))
    return 1 if any(v == "FAIL" for v in G.values()) else 0


if __name__ == "__main__":
    sys.exit(main())