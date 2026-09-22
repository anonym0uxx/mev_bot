"""SUPERSEDED - DO NOT RUN. Kept only as the record of the diagnostic that found the
three defects. Its candidate table began REDUCE with HOLD, it valued every candidate
from a FRESH 1 SOL entry at t_dec rather than from the held position, and it loaded
the tape with no dust filter. It produced an 87% EXIT distribution that was pure
artifact and was never shipped.

Use `build_management_c10.py` (held-position continuation + engine argmax + the
pre-action state) and validate with `qa_management_c10.py` and
`test_held_position.py`.

--- original header follows ---

Relabel the management (exit-authority) family on the CURRENT accounting.

WHY THIS EXISTS. `candidate_sft_c9` and `c9_r2` dropped the 39,821-row
`management_replay` family because its labels were produced by
`replay_v2.0_constant_notional` - the retired accounting. Dropping it removed
HOLD/ADD/REDUCE/EXIT from the model's vocabulary entirely, i.e. the policy could
size an entry but never manage a position. The correct remedy is to RE-LABEL, which
is what this script does.

AUTHORITY. `reward_terms.episode_reward` -> `reward_engine.score_action_sequence`:
the same engine that grades RL, run at the DERIVED exposure penalty
(lambda_exposure = 0.08884, reports/LAMBDA_KELLY_DERIVATION.json) so the SFT
supervision and the RL reward share one risk aversion.

Each candidate first-action is encoded as the action SEQUENCE the engine values:

    EXIT   -> ["EXIT"]
    HOLD   -> ["HOLD", "EXIT"]
    REDUCE -> ["HOLD", "REDUCE", "EXIT"]
    ADD    -> ["ADD", "HOLD", "EXIT", "EXIT"]

BASIS (stated, not hidden): all four candidates are valued from a flat start at
t_dec. That makes the RANKING sound - every candidate sees the same tape and the
same starting state - but the absolute values ignore the position's sunk cost basis.
The label is the argmax, so the ranking is what matters. The prompt already carries
the realised position state (entry price, inventory, unrealised PnL, MFE/MAE), and
the relabelled EVIDENCE cites it.

Output is keyed by (split, line_index) so the assembler can join it back without
prompt-text matching.
"""
from __future__ import annotations

import collections
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from reward_engine import load_canonical_tapes, ReserveUnavailable  # noqa: E402
from reward_terms import RewardConfig, episode_reward  # noqa: E402
from rl_reward_v3 import V3Engine, ReserveRegistry, STALE_ANY  # noqa: E402

SRC = "/training/v2/candidate_sft_c8"
TAPES_P = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
OUT = os.environ.get("MGMT_OUT", "/training/v2/reports/management_labels_c10.jsonl")
LAMBDA_EXPOSURE = 0.08884        # derived; see reports/LAMBDA_KELLY_DERIVATION.json
SPLITS = ("train", "validation", "examination")

CANDS = {
    "EXIT": ["EXIT"],
    "HOLD": ["HOLD", "EXIT"],
    "REDUCE": ["HOLD", "REDUCE", "EXIT"],
    "ADD": ["ADD", "HOLD", "EXIT", "EXIT"],
}

WORKERS = int(sys.argv[1]) if len(sys.argv) > 1 else 16
LIMIT = int(sys.argv[2]) if len(sys.argv) > 2 else 0

TAPES = load_canonical_tapes(TAPES_P)
REG = ReserveRegistry.build()
ENG = V3Engine(REG, max_reserve_stale_ms=STALE_ANY)
CFG = RewardConfig(lambda_exposure=LAMBDA_EXPOSURE)


def row_key(rec) -> str:
    import hashlib
    blob = json.dumps(rec["messages"][:-1], sort_keys=True, ensure_ascii=False)
    return hashlib.sha256(blob.encode()).hexdigest()


def t_dec_of(rec) -> int:
    m = rec["meta"]
    for k in ("decision_time_unix_ms", "t_dec_ms"):
        if m.get(k):
            return int(m[k])
    cid = m.get("candidate_id") or ""
    try:
        return int(cid.split(":")[2].split("|")[0])
    except Exception:
        return None


def value_one(job):
    split, idx, mint, t_dec, key = job
    tape = TAPES.get(mint)
    if tape is None:
        return {"key": key, "split": split, "line": idx, "mint": mint, "t_dec": t_dec,
                "status": "no_tape"}
    out = {"key": key, "split": split, "line": idx, "mint": mint, "t_dec": t_dec,
           "schema": "management_action_value_v1", "lambda_exposure": LAMBDA_EXPOSURE}
    try:
        vals, detail = {}, {}
        for act, seq in CANDS.items():
            # Value each candidate INDEPENDENTLY: an unpriceable leg (e.g. the ADD
            # candidate asking the book for more than it can absorb) must disqualify
            # only that candidate, not the whole row. Losing a row because one of four
            # counterfactuals refuses would bias the label set toward quiet books.
            try:
                r = episode_reward(tape, t_dec, seq, engine=ENG, cfg=CFG,
                                   refuse_cross_graduation=False)
            except Exception as e:                               # noqa: BLE001
                detail[act] = {"status": "refused", "reason": f"{type(e).__name__}"[:80]}
                continue
            if r.get("refused") or r.get("reward") is None:
                detail[act] = {"status": r.get("status"), "reward": None}
                continue
            vals[act] = float(r["reward"])
            detail[act] = {"status": r.get("status"), "reward": round(float(r["reward"]), 6),
                           "net_sol": (r.get("episode") or {}).get("net_sol_returned")}
        if not vals:
            out["status"] = "refused_no_reserves"
            out["detail"] = detail
            return out
        best = max(vals, key=lambda a: vals[a])
        out.update({"status": "labeled", "action": best,
                    "values": {k: round(v, 6) for k, v in vals.items()},
                    "detail": detail,
                    "runner_up": sorted(vals, key=lambda a: -vals[a])[1]
                    if len(vals) > 1 else None})
        return out
    except ReserveUnavailable as e:
        out["status"] = "refused_no_reserves"
        out["reason"] = str(e)[:200]
        return out
    except Exception as e:                                       # noqa: BLE001
        out["status"] = "error"
        out["reason"] = f"{type(e).__name__}: {e}"[:200]
        return out


def main():
    jobs = []
    for split in SPLITS:
        p = os.path.join(SRC, split + ".jsonl")
        if not os.path.isfile(p):
            continue
        with open(p, encoding="utf-8") as fh:
            for i, line in enumerate(fh):
                rec = json.loads(line)
                m = rec.get("meta") or {}
                if m.get("family") != "management_replay":
                    continue
                t = t_dec_of(rec)
                if t is None:
                    continue
                jobs.append((split, i, m.get("mint"), t, row_key(rec)))
    # round-robin by mint so any prefix is representative
    by_mint = collections.defaultdict(list)
    for j in jobs:
        by_mint[j[2]].append(j)
    jobs = [by_mint[mm][ii] for ii in range(max((len(v) for v in by_mint.values()), default=0))
            for mm in sorted(by_mint) if ii < len(by_mint[mm])]
    done = set()
    if os.path.isfile(OUT):
        for line in open(OUT, encoding="utf-8"):
            try:
                done.add(json.loads(line)["key"])
            except Exception:
                continue
    todo = [j for j in jobs if j[4] not in done]
    print(json.dumps({"management_rows": len(jobs), "already_done": len(jobs) - len(todo),
                      "todo": len(todo), "workers": WORKERS,
                      "lambda_exposure": LAMBDA_EXPOSURE}), flush=True)
    if LIMIT:
        todo = todo[:LIMIT]
    if not todo:
        return
    import multiprocessing as mp
    n = 0
    tally = collections.Counter()
    with open(OUT, "a", encoding="utf-8") as fo, mp.Pool(WORKERS) as pool:
        for res in pool.imap_unordered(value_one, todo, chunksize=8):
            fo.write(json.dumps(res, ensure_ascii=False) + "\n")
            n += 1
            tally[res.get("status")] += 1
            if res.get("status") == "labeled":
                tally["act|" + str(res.get("action"))] += 1
            if n % 2000 == 0:
                print(json.dumps({"done": n, "of": len(todo), "tally": dict(tally)}), flush=True)
    print(json.dumps({"rows": n, "out": OUT, "tally": dict(tally)}), flush=True)


if __name__ == "__main__":
    main()
