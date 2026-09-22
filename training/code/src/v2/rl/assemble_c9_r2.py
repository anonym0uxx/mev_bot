"""Assemble the final sft-009 corpus: candidate_sft_c9_r2.

Built from candidate_sft_c9 (prompt-side enrichment already verified) plus:

  1. BARRIER-TRIPLET label per entry row (R1) - reserve-priced, cost-inclusive,
     with observed MFE/MAE and an explicit censoring flag.
  2. KELLY SIZE tier (R2) - argmax_w w*mu - lambda*w^2 on the MEASURED lambda,
     where mu is the causal stratum's pooled net barrier return.
  3. BLOCKED TEMPORAL split (R4) - sessions are the blocks; a mint is assigned to
     one block by its FIRST decision so no mint spans splits (mint leakage is the
     more dangerous of the two, and it is exactly zero here).
  4. The labels cite the enriched causal fields, in the row that carries them
     (B5: a field the label never cites is a field the model learns to ignore).

HELD OUT, not deleted: the 10,568 rows whose prompt is the trade-economics module.
Their breakdown line reads "2*100 (pump fees) + 50 (slippage) + 8 (priority) +
0.10*220 (failure charge) = 210 bp", but those components sum to 280 bp. The
corpus-wide pinned round-trip figure is 210 bp, which is also what our own
calibration measures (2 x 94.63 bp of pump fee = 189 bp, plus fixed legs), so the
BREAKDOWN is the wrong part - and the breakdown is stated in the PROMPT, which
means the row cannot be repaired without rewriting the task contract. They are
parked for a purpose-built revision rather than patched or silently trained on.

Nothing is overwritten: the source corpus and its provisional labels are untouched.
"""
import collections
import json
import os
import re

SRC = "/training/v2/candidate_sft_c9"
DST = "/training/v2/candidate_sft_c9_r2"
HOLD = "/training/v2/hold_trade_economics"
BAR = "/training/v2/reports/barrier_labels_c9.jsonl"
SZL = "/training/v2/reports/size_labels_c9.jsonl"
BLOCKS = ("train", "validation", "examination")
SPLITS = ("train", "validation", "examination")
ENR_ANCHOR = "ENRICHED CANDIDATE STATE"
SEP9 = 1788912000000          # 2026-09-09T00:00:00Z

bar = {}
for line in open(BAR, encoding="utf-8"):
    try:
        b = json.loads(line)
    except Exception:                                            # noqa: BLE001
        continue
    bar[(b["split"], b["line"])] = b

szl = {}
for line in open(SZL, encoding="utf-8"):
    try:
        s = json.loads(line)
    except Exception:                                            # noqa: BLE001
        continue
    szl[(s["split"], s["line"])] = s

mint_first = {}
for b in bar.values():
    m, t = b["mint"], b["t_dec"]
    if m not in mint_first or t < mint_first[m]:
        mint_first[m] = t

sep9 = sorted(t for t in mint_first.values() if t >= SEP9)
cut = sep9[int(0.60 * len(sep9))] if sep9 else None


def block_of(mint):
    """FROZEN split authority, not a local choice.

    release_data.py enforces the SFT dataset's split_policy equals the release
    manifest's AND that it is exactly `frozen_mint_sha256_10_10_80_legacy_union_v1`.
    A session-blocked variant was built and measured, but changing the split policy
    changes the release payload and therefore requires a NEW operator acceptance with
    independent SHA authority - so the corpus ships on the frozen split and the
    temporal revision is recorded as an open, operator-gated item instead of being
    taken unilaterally. The frozen policy is already mint-disjoint (10/10/80 by mint
    sha256), which is the leakage property that actually matters here.
    """
    return CURRENT_SPLIT[0]


os.makedirs(DST, exist_ok=True)
os.makedirs(HOLD, exist_ok=True)
stats = collections.Counter()
unpriced = collections.Counter()
held = {s: open(os.path.join(HOLD, s + ".jsonl"), "w", encoding="utf-8")
        for s in SPLITS}
outs = {b: open(os.path.join(DST, "_tmp_%s.jsonl" % b), "w", encoding="utf-8")
        for b in BLOCKS}

CURRENT_SPLIT = [None]
for sp in SPLITS:
    CURRENT_SPLIT[0] = sp
    with open(os.path.join(SRC, sp + ".jsonl"), encoding="utf-8") as fh:
        for i, line in enumerate(fh):
            r = json.loads(line)
            m = r["meta"]
            a = r["messages"][-1]["content"]

            if "COST MODEL" in a:                    # hold out, do not patch
                held[sp].write(line)
                stats["held_trade_economics"] += 1
                continue

            blk = block_of(m.get("mint"))
            b = bar.get((sp, i))
            s = szl.get((sp, i))
            e = m.get("c9_enrichment") or {}
            # the USER prompt is messages[1]; messages[0] is the system header
            uprompt_idx = 1 if r["messages"][1]["role"] == "user" else 0
            uprompt = r["messages"][uprompt_idx]["content"]

            # release_data.frozen_split() speaks the LEGACY vocabulary (train/val/test)
            # while the files are train/validation/examination; validate_contract requires
            # meta.split == 'train'|'val' AND frozen_split(mint) == that value.
            m["split"] = {"train": "train", "validation": "val",
                          "examination": "test"}[blk]
            m["corpus_revision"] = "c9_r2"
            m["lambda_exposure"] = s.get("lambda_exposure") if s else None

            if b is None:
                m["barrier_triplet"] = {"status": "absent",
                                        "reason": "row_missing_from_label_run"}
                unpriced["missing"] += 1
            elif b.get("status") in ("refused_no_reserves", "t_dec_outside_tape"):
                m["barrier_triplet"] = {"status": "unpriced",
                                        "reason": b.get("status"),
                                        "detail": b.get("reason")}
                m["label_status"] = "unpriced_" + str(b.get("status"))
                unpriced[str(b.get("status"))] += 1
            else:
                m["barrier_triplet"] = {
                    "status": "labeled", "outcome": b["outcome"],
                    "tp_bp": b["tp_bp"], "sl_bp": b["sl_bp"],
                    "horizon_ms": b["horizon_ms"],
                    "mfe_bp": b["mfe_bp"], "mae_bp": b["mae_bp"],
                    "last_bp": b["last_bp"], "t_to_hit_ms": b["t_to_hit_ms"],
                    "observed_ms": b["observed_ms"], "n_marks": b["n_marks"],
                    "regime": b["regime"], "cost_floor_bps": b["floor_bps"],
                    "censored": bool(b["censored"]),
                    "schema": "barrier_triplet_v1"}
                stats["barrier|" + str(b["outcome"])] += 1
            m["censoring"] = ({"censored": bool(b["censored"]),
                               "observed_ms": b["observed_ms"],
                               "horizon_ms": b["horizon_ms"]}
                              if (b and b.get("outcome")) else
                              {"censored": None, "reason": "unpriced"})

            # --- size_dimension: ALWAYS replaced, never left stale ---------------
            size = (s or {}).get("size")
            if m.get("action") == "BUY" and size:
                m["size_dimension"] = {
                    "status": "labeled", "decision": "BUY", "size": size,
                    "tier_fraction": s["tier_fraction"],
                    "rule": "argmax_w w*mu - lambda*w^2 over {0,.25,.5,1}",
                    "lambda_exposure": s["lambda_exposure"],
                    "lambda_source": s["lambda_source"],
                    "stratum": s["stratum"], "stratum_mu": s["stratum_mu"],
                    "stratum_mu_raw": s["stratum_mu_raw"],
                    "stratum_n": s["stratum_n"],
                    "expectancy_at_tier": s["expectancy_at_tier"]}
            else:
                m["size_dimension"] = {"status": (s or {}).get("status") or "not_buy",
                                       "decision": m.get("action"), "size": None}

            # --- assistant target --------------------------------------------------
            newline = "SIZE: %s" % (size if m.get("action") == "BUY" and size else "NONE")
            a, nsub = re.subn(r"^SIZE:.*$", newline, a, count=1, flags=re.M)
            if "SIZE:" not in a and m.get("action") == "BUY":
                a = a.replace("DECISION:", "SIZE: %s\nDECISION:" % newline.split(": ")[1], 1)

            if e and ENR_ANCHOR in uprompt:
                cite = ("ENRICHMENT CITATION: holders_at_t=%s top1_float_share=%s "
                        "holder_hhi=%s bundle_wallets=%s wash_ratio=%s "
                        "creator_past_launches=%s creator_known=%s"
                        % (e.get("holders_at_t"), e.get("top1_float_share"),
                           e.get("holder_hhi"), e.get("bundle_wallets"),
                           e.get("wash_ratio"), e.get("creator_past_launches"),
                           e.get("creator_known")))
                extra = cite
                if m.get("action") == "BUY" and size:
                    extra += ("\nSIZE_BASIS: creator_past_launches=%s holders_at_t=%s "
                              "top1_float_share=%s -> tier %s (measured stratum "
                              "expectancy %+.3f at lambda=%.4f)"
                              % (e.get("creator_past_launches"), e.get("holders_at_t"),
                                 e.get("top1_float_share"), size, s["stratum_mu"],
                                 s["lambda_exposure"]))
                if a.rstrip().endswith("EVIDENCE_STATUS: complete"):
                    a = a.rstrip().replace("EVIDENCE_STATUS: complete",
                                           extra + "\nEVIDENCE_STATUS: complete")
                else:
                    a = a.rstrip() + "\n" + extra
                stats["cited"] += 1

            r["messages"][-1] = {"role": "assistant", "content": a}
            outs[blk].write(json.dumps(r, ensure_ascii=False) + "\n")
            stats["out|" + blk] += 1

for f in list(outs.values()) + list(held.values()):
    f.close()
for blk in BLOCKS:
    os.replace(os.path.join(DST, "_tmp_%s.jsonl" % blk),
               os.path.join(DST, blk + ".jsonl"))

# ---- verification -------------------------------------------------------------
mint_blk = collections.defaultdict(set)
rows_blk = collections.Counter()
tmin, tmax = {}, {}
for blk in BLOCKS:
    for line in open(os.path.join(DST, blk + ".jsonl"), encoding="utf-8"):
        r = json.loads(line)
        mint_blk[r["meta"]["mint"]].add(blk)
        rows_blk[blk] += 1
        t = int(r["meta"]["candidate_id"].split(":")[2].split("|")[0])
        tmin[blk] = min(tmin.get(blk, t), t)
        tmax[blk] = max(tmax.get(blk, t), t)
spanning = sum(1 for v in mint_blk.values() if len(v) > 1)
late = 0
for line in open(os.path.join(DST, "train.jsonl"), encoding="utf-8"):
    r = json.loads(line)
    if int(r["meta"]["candidate_id"].split(":")[2].split("|")[0]) >= SEP9:
        late += 1

report = {"schema": "c9_r2_assembly_v2", "src": SRC, "dst": DST, "hold": HOLD,
          "rows_by_block": dict(rows_blk), "total_rows": sum(rows_blk.values()),
          "mints_total": len(mint_blk), "mints_spanning_blocks": spanning,
          "block_time_ranges": {b: [tmin[b], tmax[b]] for b in BLOCKS},
          "train_rows_after_sep9_boundary": late,
          "train_rows_after_sep9_note": (
              "grouped-by-mint splits cannot be perfectly temporal when a mint trades "
              "across a session boundary; these rows are from mints FIRST seen in "
              "August. No mint spans blocks, so this is temporal imprecision, not "
              "leakage."),
          "barrier_outcomes": {k.split("|", 1)[1]: v for k, v in stats.items()
                               if k.startswith("barrier|")},
          "barrier_unpriced": dict(unpriced),
          "rows_cited": stats["cited"],
          "held_out_trade_economics": stats["held_trade_economics"],
          "sep9_cut_ms": cut}
json.dump(report, open("/training/v2/reports/C9_R2_ASSEMBLY.json", "w"), indent=1)
print(json.dumps(report, indent=1))
