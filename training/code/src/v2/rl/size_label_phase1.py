"""PHASE 1 - parallel size labelling for the c7 corpus.

For every ENTRY decision row (DECISION in BUY/WATCH/SKIP) compute:
  * the size-aware label (SKIP / WATCH / BUY + tier) by engine simulation
  * the SIZE menu lines, measured by the SAME engine from the SAME reserves, so
    the prompt's stated costs can never diverge from the scorer that grades it
  * the full per-tier values for audit

Deterministic and resumable: jobs are mint-stratified, and a row whose key is
already in the output is skipped, so a restart resumes rather than restarts.
Runs under fork so the loaded reserves are shared, not duplicated.
"""
import collections, hashlib, json, os, sys, time
sys.path.insert(0, "/training/v2/code/src/v2/rl")
import multiprocessing as mp

from rl_reward_v3 import (V3Engine, ReserveRegistry, STALE_ANY,
                          load_canonical_tapes, ACCOUNT_CAPITAL_SOL)
from size_dimension import label_for, menu_text, SIZE_NOTIONAL_SOL

OUT = "/training/v2/reports/size_labels_c7.jsonl"
WORKERS = int(sys.argv[1]) if len(sys.argv) > 1 else 24
LIMIT = int(sys.argv[2]) if len(sys.argv) > 2 else 0
CORPUS = "/training/v2/candidate_sft_c6"
ENTRY = {"BUY", "WATCH", "SKIP"}

REG = ReserveRegistry.build(verbose=False)
ENG = V3Engine(REG, max_reserve_stale_ms=STALE_ANY, deploy_sol=1.0)
TAPES = load_canonical_tapes("/training/v2/canonical/renorm_corpus_mints/trades.jsonl",
                             min_notional_lamports=100_000, max_px_ratio=50)

import re
RE_DEC = re.compile(r"^DECISION\s*[:\-]\s*([A-Za-z_]+)", re.M)


def rowkey(rec):
    return hashlib.sha256(json.dumps(rec["messages"][:-1], sort_keys=True,
                                     ensure_ascii=False).encode()).hexdigest()


jobs = []
for split in ("train", "validation", "examination"):
    p = os.path.join(CORPUS, "%s.jsonl" % split)
    if not os.path.isfile(p):
        continue
    with open(p, encoding="utf-8") as fh:
        for ln, line in enumerate(fh):
            if '"DECISION' not in line[:200000] and "DECISION" not in line:
                continue
            try:
                rec = json.loads(line)
            except Exception:
                continue
            a = (rec.get("messages") or [{}])[-1].get("content") or ""
            m = RE_DEC.search(a)
            if not m or m.group(1).upper() not in ENTRY:
                continue
            eid = (rec.get("episode_id") or "").split(":")
            if len(eid) < 3:
                continue
            try:
                t_dec = int(eid[2])
            except ValueError:
                continue
            jobs.append((split, ln, eid[1], t_dec, m.group(1).upper(), rowkey(rec)))

jobs.sort(key=lambda j: (j[2], j[3]))
done = set()
if os.path.isfile(OUT):
    with open(OUT, encoding="utf-8") as fh:
        for line in fh:
            try:
                done.add(json.loads(line)["key"])
            except Exception:
                continue
todo = [j for j in jobs if j[5] not in done]
skipped_done = len(jobs) - len(todo)
if LIMIT:
    todo = todo[:LIMIT]
print(json.dumps({"entry_rows": len(jobs), "already_done": skipped_done,
                  "todo": len(todo), "workers": WORKERS}), flush=True)


def work(j):
    split, ln, mint, t_dec, old, key = j
    tape = TAPES.get(mint)
    base = {"key": key, "split": split, "line": ln, "mint": mint, "t_dec": t_dec,
            "old": old}
    if tape is None or not (int(tape.tt[0]) <= t_dec <= int(tape.tt[-1])):
        base.update({"decision": None, "size": None, "menu": None,
                     "skip_reason": "no_tape_or_window"})
        return base
    try:
        lab = label_for(ENG, tape, t_dec, account_sol=ACCOUNT_CAPITAL_SOL)
        menu = menu_text(ENG, tape, t_dec, SIZE_NOTIONAL_SOL)
    except Exception as e:                                       # noqa: BLE001
        base.update({"decision": None, "size": None, "menu": None,
                     "skip_reason": "exc:" + type(e).__name__})
        return base
    dec, size = lab["decision"], lab["size"]
    # WATCH/SKIP are value-identical (both 0.0) in the RL group. Break that tie
    # toward the EXISTING label so deferral stays a live capability instead of
    # being silently deleted from the corpus.
    if dec == "SKIP" and old == "WATCH":
        dec = "WATCH"
    base.update({"decision": dec, "size": size,
                 "values": {k: round(v, 6) for k, v in lab["values"].items()},
                 "menu": menu, "reason": lab.get("reason")})
    return base


t0 = time.time()
n = 0
with open(OUT, "a", encoding="utf-8") as fh:
    with mp.Pool(WORKERS) as pool:
        for r in pool.imap_unordered(work, todo, chunksize=8):
            fh.write(json.dumps(r) + "\n")
            n += 1
            if n % 500 == 0:
                fh.flush()
                print(json.dumps({"done": n, "of": len(todo),
                                  "rate_s": round((time.time() - t0) / n, 3)}),
                      flush=True)
print(json.dumps({"finished": n, "seconds": round(time.time() - t0, 1)}), flush=True)