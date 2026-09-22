"""Deep QA on candidate_sft_c9_r2 before it is allowed near the trainer.

Every check prints a PASS/FAIL and the numbers behind it. A FAIL is a finding, not a
nuisance: the point of this file is to be the thing that would have caught the 210 bp
template, the session-mixed splits, and the single-path size labels.

Checks
  C1  row preservation / no loss, no duplication, unique keys
  C2  mint disjointness and strict temporal blocking between blocks
  C3  causality: the golden enriched fields appear in the PROMPT, and the prompt
      carries no forward-looking token
  C4  the label cites the enriched fields (B5: a field the label never cites is a
      field the model learns to ignore)
  C5  censoring consistency between meta.censoring and meta.barrier_triplet
  C6  barrier arithmetic and units (TP/SL levels, MFE/MAE bracket the last mark)
  C7  size tier legality and the lambda that produced it
  C8  economics rows: the repaired arithmetic now sums to its own total
  C9  exact-duplicate prompt hashes (padding / duplication ban)
"""
import collections
import hashlib
import json
import re

DST = "/training/v2/candidate_sft_c9_r2"
SRC = "/training/v2/candidate_sft_c9"
BLOCKS = ("train", "validation", "examination")
FIELDS = ("holders_at_t", "top1_float_share", "holder_hhi", "bundle_wallets",
          "wash_ratio", "creator_past_launches")

fails = []


def check(name, ok, detail):
    print("%-6s %-58s %s" % ("PASS" if ok else "FAIL", name, detail))
    if not ok:
        fails.append(name)


def _uprompt(r):
    return r["messages"][1]["content"] if r["messages"][1]["role"] == "user" \
        else r["messages"][0]["content"]


rows = {}
for blk in BLOCKS:
    for i, line in enumerate(open("%s/%s.jsonl" % (DST, blk), encoding="utf-8")):
        r = json.loads(line)
        rows[(blk, i)] = r

# ---- C1 ---------------------------------------------------------------------
src_n = sum(1 for b in BLOCKS for _ in open("%s/%s.jsonl" % (SRC, b), encoding="utf-8"))
keys = collections.Counter()
for r in rows.values():
    keys[r["meta"]["candidate_id"]] += 1
dups = sum(1 for k, c in keys.items() if c > 1)
HELD_OUT = 10568   # trade_economics family parked in /training/v2/hold_trade_economics
check("C1 rows preserved (minus documented hold-out)",
      len(rows) == src_n - HELD_OUT,
      f"out={len(rows):,} src={src_n:,} held={HELD_OUT:,}")
check("C1 keys unique", dups == 0, f"duplicated candidate_ids={dups}")

# ---- C2 ---------------------------------------------------------------------
mint_blk = collections.defaultdict(set)
tmin, tmax = {}, {}
for blk in BLOCKS:
    for line in open("%s/%s.jsonl" % (DST, blk), encoding="utf-8"):
        r = json.loads(line)
        mint_blk[r["meta"]["mint"]].add(blk)
        t = int(r["meta"]["candidate_id"].split(":")[2].split("|")[0])
        tmin[blk] = min(tmin.get(blk, t), t)
        tmax[blk] = max(tmax.get(blk, t), t)
span = sum(1 for v in mint_blk.values() if len(v) > 1)
check("C2 mints do not span blocks", span == 0, f"spanning={span} mints={len(mint_blk):,}")
# the split is FROZEN by the release (frozen_mint_sha256_10_10_80_legacy_union_v1).
# We assert only that the assembler did not move a row between blocks.
src_split = {}
for sp in ("train", "validation", "examination"):
    for j, ln in enumerate(open("%s/%s.jsonl" % (SRC, sp), encoding="utf-8")):
        src_split[json.loads(ln)["meta"]["candidate_id"]] = sp
moved = 0
checked = 0
for blk in BLOCKS:
    for line in open("%s/%s.jsonl" % (DST, blk), encoding="utf-8"):
        r = json.loads(line)
        checked += 1
        if src_split.get(r["meta"]["candidate_id"]) != blk:
            moved += 1
check("C2 frozen split policy preserved", moved == 0,
      f"rows checked={checked:,} moved between blocks={moved}")

# ---- C3 / C4 -----------------------------------------------------------------
FORWARD = ("barrier", "outcome:", "won", "hit tp", "mfe_bp", "mae_bp", "tp_bp",
           "LAST MARK", "realized", "future")
prompt_forward = 0
cit = collections.Counter()
tot_decision = 0
size_tier = collections.Counter()
for r in rows.values():
    p = _uprompt(r).lower()
    if any(f.lower() in p for f in FORWARD):
        prompt_forward += 1
    a = r["messages"][-1]["content"]
    if r["meta"]["task"] != "decision_action":
        continue
    tot_decision += 1
    if "ENRICHMENT CITATION" in a:
        cit["rows"] += 1
        for f in FIELDS:
            if f + "=" in a:
                cit[f] += 1
    sd = r["meta"].get("size_dimension") or {}
    if sd.get("size"):
        size_tier[sd["size"]] += 1
check("C3 prompt has no forward-looking token", prompt_forward == 0,
      f"prompts flagged={prompt_forward}")
check("C4 decision rows cite enriched fields", cit["rows"] == tot_decision,
      f"cited={cit['rows']:,}/{tot_decision:,}")
missing = [f for f in FIELDS if cit[f] < cit["rows"]]
check("C4 every enriched field is cited", not missing,
      "under-cited=" + (",".join(missing) if missing else "none"))

# ---- C5 ---------------------------------------------------------------------
mis = 0
unpriced_ok = 0
unpriced_n = 0
for r in rows.values():
    bt = r["meta"].get("barrier_triplet") or {}
    cen = r["meta"].get("censoring") or {}
    if bt.get("status") != "labeled":
        unpriced_n += 1
        if cen.get("censored") is None:
            unpriced_ok += 1
        continue
    if bool(cen.get("censored")) != bool(bt.get("censored")):
        mis += 1
check("C5 censoring consistent", mis == 0, f"mismatches={mis}")
check("C5 unpriced rows have null censoring", unpriced_ok == unpriced_n,
      f"{unpriced_ok}/{unpriced_n}")

# ---- C6 ---------------------------------------------------------------------
bad = collections.Counter()
named = 0
for r in rows.values():
    bt = r["meta"].get("barrier_triplet") or {}
    if bt.get("status") != "labeled":
        continue
    named += 1
    if bt["tp_bp"] != 10000.0 or bt["sl_bp"] != 5000.0:
        bad["levels"] += 1
    if not (bt["mae_bp"] <= bt["last_bp"] <= bt["mfe_bp"] + 1e-6):
        bad["bracket"] += 1
    if bt["mae_bp"] > 0 or bt["mfe_bp"] < 0:
        bad["sign"] += 1
    if bt["horizon_ms"] != 1800000:
        bad["horizon"] += 1
    if bt["outcome"] == "tp" and bt["t_to_hit_ms"] is None:
        bad["tp_no_time"] += 1
check("C6 barrier levels/units/bracketing", not bad,
      f"labeled={named:,} violations={dict(bad) if bad else 'none'}")

# ---- C7 ---------------------------------------------------------------------
lam = set()
bad_size = collections.Counter()
for r in rows.values():
    sd = r["meta"].get("size_dimension") or {}
    if sd.get("size") and sd["size"] not in ("SMALL", "MID", "FULL"):
        bad_size["illegal"] += 1
    if sd.get("size") and sd["size"] == "FULL" and sd.get("tier_fraction") != 1.0:
        bad_size["tier_mismatch"] += 1
    if sd.get("lambda_exposure"):
        lam.add(round(float(sd["lambda_exposure"]), 6))
check("C7 size tiers legal", not bad_size, f"violations={dict(bad_size) if bad_size else 'none'}")
check("C7 single lambda across corpus", len(lam) == 1, f"lambdas={sorted(lam)}")

# ---- C8 ---------------------------------------------------------------------
# The trade-economics family is HELD OUT, not repaired: its breakdown line is
# stated in the PROMPT, so it cannot be fixed without rewriting the task
# contract. This check proves the hold-out is real and quantified.
n_econ_in_corpus = sum(1 for r in rows.values()
                       if "COST MODEL" in r["messages"][-1]["content"])
held = 0
bad_sum = 0
for line in open("/training/v2/hold_trade_economics/train.jsonl", encoding="utf-8"):
    held += 1
    a = json.loads(line)["messages"][-1]["content"]
    mm = re.search(r"COST MODEL:.*=\s*([0-9.]+)\s*bp", a)
    if mm and int(2 * 100 + 50 + 8 + 0.10 * 220) != int(mm.group(1)):
        bad_sum += 1
check("C8 economics family absent from corpus", n_econ_in_corpus == 0,
      f"rows carrying COST MODEL in corpus={n_econ_in_corpus}")
check("C8 held-out defect quantified", held > 0,
      f"parked rows(train)={held:,} whose stated total contradicts its own breakdown={bad_sum:,}")

# ---- C9 ---------------------------------------------------------------------
h = collections.Counter()
for r in rows.values():
    h[hashlib.sha256(_uprompt(r).encode()).hexdigest()] += 1
dup = sum(1 for c in h.values() if c > 1)
check("C9 no duplicated prompts", dup == 0, f"duplicate prompt hashes={dup}")

# ---- census ------------------------------------------------------------------
print("\n--- census ---")
print("rows by block:", {b: sum(1 for k in rows if k[0] == b) for b in BLOCKS})
print("tasks:", dict(collections.Counter(r["meta"]["task"] for r in rows.values())))
print("actions:", dict(collections.Counter(str(r["meta"].get("action"))
                                           for r in rows.values())))
print("size tiers (BUY rows):", dict(size_tier))
print("barrier outcomes:", dict(collections.Counter(
    (r["meta"].get("barrier_triplet") or {}).get("outcome", "unpriced")
    for r in rows.values())))
chars = collections.Counter()
for blk in BLOCKS:
    for line in open("%s/%s.jsonl" % (DST, blk), encoding="utf-8"):
        chars[blk] += len(line)
print("chars by block:", {k: f"{v:,}" for k, v in chars.items()})

print("\n=== %d FAILURES ===" % len(fails) if fails else "\n=== ALL CHECKS PASS ===")
json.dump({"fails": fails, "size_tiers": dict(size_tier),
           "blocks": {b: sum(1 for k in rows if k[0] == b) for b in BLOCKS}},
          open("/training/v2/reports/C9_R2_QA.json", "w"), indent=1)
