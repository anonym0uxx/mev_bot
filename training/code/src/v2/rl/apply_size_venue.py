"""Apply the venue-separated size labels to the c10 entry corpus.

- curve BUY -> SMALL (0.25) fixed satellite (Kelly = 0; median negative)
- amm BUY  -> tier = argmax w*mu - 0.13681*w^2 on the venue-separated stratum mu
              (keep current size when the venue-separated mu is negative/unknown,
               so a BUY never carries a NONE size)

Patches size_dimension, meta.lambda_exposure, and the DECISION/SIZE header.
"""
import json, os, re, shutil

SRC = "/training/v2/candidate_sft_c10_entry_labeled"
BACKUP = "/training/v2/candidate_sft_c10_entry_labeled_prev_lambda"
MAP = "/training/v2/reports/VENUE_SIZE_MAP.json"
LAM_AMM = 0.13681
LAM_CURVE = 0.16720
TIER_FRAC = {"FULL": 1.0, "MID": 0.5, "SMALL": 0.25}
RE_SIZE = re.compile(r"^SIZE:\s*\S+", re.M)

tier_map = json.load(open(MAP))["tier_map"]
lookup = {(v["stratum"], v["regime"]): v for v in tier_map.values()}

if not os.path.isdir(BACKUP):
    shutil.copytree(SRC, BACKUP)

def size_for(stratum, reg, cur):
    if reg == "bonding_curve":
        return "SMALL", LAM_CURVE, None
    v = lookup.get((stratum, "amm"))
    if not v or not v.get("size"):
        return cur, LAM_AMM, v.get("mu_shrunk") if v else None
    return v["size"], LAM_AMM, v.get("mu_shrunk")

stats = {}
for sp in ("train", "validation", "examination"):
    path = os.path.join(SRC, f"{sp}.jsonl")
    tmp = path + ".tmp"
    c = {"n": 0, "changed": 0}
    with open(path) as fi, open(tmp, "w") as fo:
        for ln in fi:
            r = json.loads(ln)
            meta = r.get("meta", {})
            if meta.get("task") == "decision_action" and meta.get("action") == "BUY":
                c["n"] += 1
                sd = meta.get("size_dimension") or {}
                stratum = sd.get("stratum")
                reg = (meta.get("cost_authority") or {}).get("regime") or "amm"
                cur = sd.get("size")
                new_size, lam, mu = size_for(stratum, reg, cur)
                # venue-conditional lambda + venue tag: set for EVERY BUY row
                sd["lambda_exposure"] = lam
                sd["lambda_source"] = "venue_separated_buy_only"
                sd["venue"] = reg
                sd["rule"] = ("venue-conditional: argmax_w w*mu - lambda_venue*w^2 (amm) "
                              "/ fixed SMALL satellite (curve)")
                meta["lambda_exposure"] = lam
                if new_size != cur:
                    c["changed"] += 1
                    sd["size"] = new_size
                    sd["tier_fraction"] = TIER_FRAC.get(new_size)
                    if mu is not None:
                        sd["stratum_mu"] = round(mu, 6)
                    a = r["messages"][2]["content"]
                    r["messages"][2]["content"] = RE_SIZE.sub(f"SIZE: {new_size}", a, count=1)
                meta["size_dimension"] = sd
            fo.write(json.dumps(r, ensure_ascii=False) + "\n")
    os.replace(tmp, path)
    stats[sp] = c
    print(sp, c)

json.dump({"schema": "venue_size_apply_v1", "lambda_amm": LAM_AMM, "lambda_curve": LAM_CURVE,
           "per_split": stats}, open("/training/v2/reports/VENUE_SIZE_APPLY.json", "w"), indent=1)
print("backup ->", BACKUP)
print("apply report -> /training/v2/reports/VENUE_SIZE_APPLY.json")
