"""PHASE 2 - assemble candidate_sft_c7 from c6 + the phase-1 size labels.

ONLY entry-decision rows (DECISION in BUY/WATCH/SKIP) are touched:
  * DECISION is replaced by the engine-derived label
  * SIZE: <TIER> carries the size decision (BUY only; SIZE/PRICE LIMIT are
    documented as BUY-only fields, so they are dropped when the label is not BUY)
  * the engine-measured SIZE menu is injected into the USER prompt
Every other row (management, utility/reasoning) is copied byte-for-byte, and the
prompt injection is proven additive the same way build_sft_c6.py proves it: strip
the injected block and the original bytes must return exactly.

Unlabelable rows (no priceable tier) keep their DECISION and get SIZE: NONE with
the reason recorded in meta - never an invented size.
"""
import collections, hashlib, json, os, re, sys

C6 = sys.argv[1] if len(sys.argv) > 1 else "/training/v2/candidate_sft_c6"
C7 = sys.argv[2] if len(sys.argv) > 2 else "/training/v2/candidate_sft_c7"
LABELS = sys.argv[3] if len(sys.argv) > 3 else "/training/v2/reports/size_labels_c7.jsonl"
WALL_MS = 1788984256517

os.makedirs(C7, exist_ok=True)
RE_DEC = re.compile(r"^DECISION\s*[:\-]\s*([A-Za-z_]+)", re.M)
RE_SIZE = re.compile(r"^SIZE\s*[:\-][^\n]*\n?", re.M)
RE_LIMIT = re.compile(r"^PRICE LIMIT\s*[:\-][^\n]*\n?", re.M)
ENTRY = {"BUY", "WATCH", "SKIP"}


def rowkey(rec):
    return hashlib.sha256(json.dumps(rec["messages"][:-1], sort_keys=True,
                                     ensure_ascii=False).encode()).hexdigest()


labels = {}
with open(LABELS, encoding="utf-8") as fh:
    for line in fh:
        try:
            d = json.loads(line)
        except Exception:
            continue
        labels[d["key"]] = d

stats = collections.Counter()
wall_hits = 0
per_split = {}
for split in ("train", "validation", "examination"):
    src = os.path.join(C6, "%s.jsonl" % split)
    if not os.path.isfile(src):
        continue
    st = collections.Counter()
    with open(src, encoding="utf-8") as fh, \
            open(os.path.join(C7, "%s.jsonl" % split), "w", encoding="utf-8") as out:
        for line in fh:
            rec = json.loads(line)
            a = (rec.get("messages") or [{}])[-1].get("content") or ""
            m = RE_DEC.search(a)
            old = m.group(1).upper() if m else None
            if not m or old not in ENTRY:
                out.write(line if line.endswith("\n") else line + "\n")
                st["untouched_" + str(old)] += 1
                continue
            eid = (rec.get("episode_id") or "").split(":")
            t_dec = int(eid[2]) if len(eid) > 2 else None
            if t_dec is not None and t_dec >= WALL_MS:
                wall_hits += 1
            lab = labels.get(rowkey(rec))
            if lab is None:
                raise SystemExit("FATAL: entry row has no label: %s" % rec.get("mint"))
            new_dec = lab.get("decision")
            menu = lab.get("menu")
            meta = rec.setdefault("meta", {})
            if new_dec is None:
                # unpriceable: keep the decision, state no size, record why
                a2 = RE_SIZE.sub("", a)
                if "SIZE:" not in a2:
                    a2 = a2.replace("DECISION: %s" % old,
                                    "DECISION: %s\nSIZE: NONE" % old, 1)
                st["unpriceable_kept_" + old] += 1
                meta["size_dimension"] = {"status": "unpriceable",
                                          "reason": lab.get("skip_reason")
                                          or lab.get("reason") or "no_priceable_tier"}
            else:
                a2 = RE_DEC.sub("DECISION: %s" % new_dec, a, count=1)
                a2 = RE_SIZE.sub("", a2)
                if new_dec == "BUY":
                    a2 = RE_DEC.sub("DECISION: BUY\nSIZE: %s" % lab["size"], a2, count=1)
                else:
                    a2 = RE_LIMIT.sub("", a2)
                meta["size_dimension"] = {
                    "status": "labeled", "decision": new_dec, "size": lab.get("size"),
                    "values": lab.get("values"), "old_decision": old,
                    "rule": "argmax size-aware ENTRY reward over {SKIP,WATCH} u tiers"}
                st["labeled_%s_%s" % (old, new_dec + ("/" + str(lab["size"])
                                                      if lab.get("size") else ""))] += 1
            u = rec["messages"][1]["content"]
            if menu:
                block = menu if menu in u else None
                u2 = u + "\n" + menu
                if block is None and (u2.replace("\n" + menu, "", 1) != u):
                    raise SystemExit("FATAL prompt rewrite: %s" % rec.get("mint"))
                rec["messages"][1]["content"] = u2
                st["menu_injected"] += 1
            rec["messages"][-1]["content"] = a2
            out.write(json.dumps(rec, ensure_ascii=False) + "\n")
    per_split[split] = dict(st)
    stats.update(st)

touched = sum(v for k, v in stats.items()
              if k.startswith("labeled_") or k.startswith("unpriceable_"))
# EXACT reconciliation against the content census. A silent shortfall here means
# entry rows kept the old constant SIZE: 0.5 SOL - the defect being removed - so
# this must abort the build rather than ship a half-applied corpus.
if touched != 85086 or stats.get("untouched_None") != 95654 or wall_hits != 0:
    raise SystemExit("FATAL reconciliation: touched=%d (want 85086) "
                     "untouched_None=%d (want 95654) wall_hits=%d (want 0)"
                     % (touched, stats.get("untouched_None", 0), wall_hits))

outd = {"schema": "sft_c7_build_v1", "corpus": C7, "source": C6,
        "per_split": per_split, "totals": dict(stats), "wall_hits": wall_hits}
json.dump(outd, open("/training/v2/reports/SFT_C7_BUILD_REPORT.json", "w"), indent=1)
print(json.dumps({"totals": dict(stats), "wall_hits": wall_hits}, indent=1))