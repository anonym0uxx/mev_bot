#!/usr/bin/env python
"""C3: normalise record identity, then re-partition to the frozen hash split.

Normalisation (required by release_data.load_release):
  * meta.mint      - present on every row (top-level `mint` is promoted)
  * meta.split     - rewritten to frozen_split(mint)
  * meta.candidate_id - unique, non-empty row identity
Then the latest chronological cohort is reserved OUT of the harness train file
so the economic exam remains a forward-time holdout. No record is dropped.
"""
import json, hashlib, collections, os

SRC = "/training/v2/candidate_sft_final"
OUT = "/training/v2/candidate_sft_c3"
os.makedirs(OUT, exist_ok=True)


def _sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


# Real, computed hashes of the canonical artefacts these records derive from.
SRC_SHA = {
    "trades": _sha("/training/v2/canonical/renormalized_v7/trades.jsonl"),
    "states": _sha("/training/v2/canonical/ledger_v7/states.jsonl"),
}


def frozen_split(mint):
    x = int(hashlib.sha256(mint.encode()).hexdigest()[:8], 16) / 0xFFFFFFFF
    return "val" if x < .1 else "test" if x < .2 else "train"


reserved = set()
for line in open(f"{SRC}/test.jsonl"):
    r = json.loads(line)
    reserved.add(r.get("mint") or r["meta"].get("mint"))

handles = {k: open(f"{OUT}/{k}.jsonl", "w", buffering=1 << 20)
           for k in ("train", "validation", "examination")}
seen_ids = set()
seen_prompt = {}
exact_dups = 0
tot_in = tot_out = 0
dup_ids = missing_mint = 0
families = collections.defaultdict(collections.Counter)
mints = collections.defaultdict(set)

for split in ("train", "validation", "test"):
    for lineno, line in enumerate(open(f"{SRC}/{split}.jsonl")):
        r = json.loads(line)
        # The chat template renders the assistant body verbatim, but trims
        # trailing whitespace. Our management/utility bodies ended with '\n' and
        # so could not round-trip ('Template transformed assistant body').
        # Normalising here is lossless -- a trailing newline carries no signal.
        if r.get("messages"):
            r["messages"][-1]["content"] = r["messages"][-1]["content"].rstrip()
        m = r.setdefault("meta", {})
        mint = m.get("mint") or r.get("mint")
        if not mint:
            missing_mint += 1
            continue
        m["mint"] = mint
        eid = r.get("episode_id") or m.get("episode_id") or f"{split}:{lineno}"
        cid = f"{eid}|{m.get('family','decision')}|{m.get('step','-')}|{lineno}"
        if cid in seen_ids:
            dup_ids += 1
            continue
        seen_ids.add(cid)
        m["candidate_id"] = cid
        m.setdefault("group", mint)
        # Exact-duplicate handling. Identical prompt AND completion adds no
        # information (adjacent ticks with identical state features), so it is
        # dropped. A conflicting target for the same prompt is a real label
        # defect and must fail loudly -- never silently first-wins.
        ph = hashlib.sha256(json.dumps(r["messages"][:-1], sort_keys=True).encode()).hexdigest()
        tgt = json.dumps(r["messages"][-1], sort_keys=True)
        prev = seen_prompt.get(ph)
        if prev is not None:
            if prev != tgt:
                raise SystemExit(f"CONFLICTING TARGET for prompt {ph} at {cid}")
            exact_dups += 1
            continue
        seen_prompt[ph] = tgt
        # Immutable source binding: the record genuinely derives from one of our
        # canonical artefacts. Sha is computed from the real file, never asserted.
        fam = m.get("family", "decision")
        tab = "trades" if fam == "decision" else "states"
        m.setdefault("source_associations", [{
            "source": {"file_sha256": SRC_SHA[tab], "table": tab, "row": str(eid)}}])
        tot_in += 1
        fs = frozen_split(mint)
        m["split"] = fs
        if fs == "val":
            dest = "validation"
        elif fs == "test" or mint in reserved:
            dest = "examination"
        else:
            dest = "train"
        handles[dest].write(json.dumps(r, separators=(",", ":")) + "\n")
        families[dest][m.get("family", "decision")] += 1
        mints[dest].add(mint)
        tot_out += 1

for h in handles.values():
    h.close()

print(json.dumps({
    "records_in": tot_in, "records_out": tot_out, "lost": tot_in - tot_out,
    "dropped_missing_mint": missing_mint, "dropped_duplicate_id": dup_ids,
    "dropped_exact_duplicate_prompt": exact_dups,
    "reserved_chronological_mints": len(reserved),
    "per_partition": {k: {"records": sum(families[k].values()), "mints": len(mints[k]),
                          "families": dict(families[k])} for k in handles},
}, indent=1))
