"""Emit the canonical memecoin mint filter shared by SFT and RL.

RULE: a mint is a pump.fun memecoin iff its address ends in "pump".

Why the suffix and NOT the venues: the corpus tape's `venue` field is unreliable.
USDC rows carry venue=pumpfun, which is impossible (USDC never had a bonding curve),
so "has a curve row" passes everything. pump.fun CREATES mints with the "pump"
suffix, so the suffix is a creation-certificate test. An earlier behavioural rule was
tried and rejected - it kept 3,616/3,617 mints and removed only 36 rows.

Emits /training/v2/reports/MEMECOIN_FILTER_V1.json with the kept mint list and the
dropped mints, so SFT and RL filter identically instead of each re-deriving it.
"""
import collections
import json
import re

TAPE = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
OUT = "/training/v2/reports/MEMECOIN_FILTER_V1.json"

MINT = re.compile(r'"mint"\s*:\s*"([^"]+)"')
VEN = re.compile(r'"venue"\s*:\s*"([^"]+)"')

rows_by_mint = collections.Counter()
amm_by_mint = collections.Counter()
curve_by_mint = collections.Counter()
n_rows = 0

for line in open(TAPE, encoding="utf-8"):
    n_rows += 1
    m = MINT.search(line)
    if not m:
        continue
    mint = m.group(1)
    rows_by_mint[mint] += 1
    v = VEN.search(line)
    if v:
        (curve_by_mint if v.group(1) == "pumpfun" else amm_by_mint)[mint] += 1

mints = set(rows_by_mint)
kept = sorted(m for m in mints if m.endswith("pump"))
dropped = sorted(mints - set(kept))

kept_set = set(kept)
kept_rows = sum(rows_by_mint[m] for m in kept_set)
kept_amm = sum(amm_by_mint.get(m, 0) for m in kept_set)
kept_curve = sum(curve_by_mint.get(m, 0) for m in kept_set)

doc = {
    "schema": "memecoin_filter_v1",
    "rule": "mint.endswith('pump')",
    "rule_rationale": (
        "pump.fun creates mints with the 'pump' suffix, so the suffix is a "
        "creation certificate. The venue field is NOT usable: USDC carries "
        "venue=pumpfun rows, so 'has a curve row' passes every mint."
    ),
    "rejected_rules": {
        "has_curve_row": "kept 3616/3617 mints, dropped 36 rows - invalid",
    },
    "source_tape": TAPE,
    "tape_rows": n_rows,
    "mints_total": len(mints),
    "mints_kept": len(kept),
    "mints_dropped": len(dropped),
    "rows_kept": kept_rows,
    "rows_dropped": n_rows - kept_rows,
    "rows_dropped_frac": round((n_rows - kept_rows) / n_rows, 6),
    "kept_amm_rows": kept_amm,
    "kept_curve_rows": kept_curve,
    "dropped_mints": dropped,
    "kept_mints": kept,
}

import os
os.makedirs(os.path.dirname(OUT), exist_ok=True)
with open(OUT, "w", encoding="utf-8") as f:
    json.dump(doc, f, indent=1)

print(json.dumps({k: v for k, v in doc.items()
                  if k not in ("dropped_mints", "kept_mints")}, indent=1))
print("dropped mint sample:", dropped[:5])
print("WROTE", OUT)
