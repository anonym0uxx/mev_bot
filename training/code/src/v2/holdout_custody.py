#!/usr/bin/env python3
"""Gap 2 — protected-holdout custody: hash-pinned freeze + exposure audit.

Two questions, kept separate because they fail differently:

  (1) FREEZE  — is the protected holdout byte-pinned, so it cannot drift
      between the moment it was defined and the moment a result is reported?

  (2) EXPOSURE — did any training-consumed split touch a protected record?
      Zero overlap is the claim; it is checked here rather than assumed.
      A group-disjoint split is only as good as the identity key used, so the
      audit reports overlap under BOTH a content key and a group key. If the
      two disagree, the group key is wrong and overlap is understated.

Read-only. Writes a custody receipt.
"""
import hashlib
import json
import sys
from pathlib import Path

ID_FIELDS = ('episode_id', 'group_id', 'decision_id', 'mint', 'mint_address',
             'token_address', 'wallet', 'trader', 'signature')


def sha256_file(p, chunk=1 << 20):
    h = hashlib.sha256()
    with open(p, 'rb') as fh:
        while True:
            b = fh.read(chunk)
            if not b:
                break
            h.update(b)
    return h.hexdigest()


def record_keys(rec):
    """(content_key, group_key, n_fields_found)

    NOTE: the CPT family stores its text under `content`; the SFT family uses
    `messages`. Hashing only `messages` makes every CPT record collide on
    `null`, which reports a leak that does not exist. Key off whichever is
    present.
    """
    body = rec.get('messages')
    if body is None:
        body = rec.get('content')
    ck = hashlib.sha256(
        json.dumps(body, sort_keys=True).encode()).hexdigest()
    meta = rec.get('meta') or {}
    ev = rec.get('evidence') or {}
    merged = {**ev, **meta}
    parts = [f'{k}={merged[k]}' for k in ID_FIELDS if merged.get(k) is not None]
    gk = hashlib.sha256('|'.join(sorted(parts)).encode()).hexdigest() if parts else None
    return ck, gk, len(parts)


def scan(p):
    """-> (n, set(content), set(group), field_hits)"""
    n = 0
    cs, gs = set(), set()
    hits = {k: 0 for k in ID_FIELDS}
    with open(p) as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except Exception:
                continue
            n += 1
            ck, gk, _ = record_keys(rec)
            cs.add(ck)
            if gk:
                gs.add(gk)
            merged = {**(rec.get('evidence') or {}), **(rec.get('meta') or {})}
            for k in ID_FIELDS:
                if merged.get(k) is not None:
                    hits[k] += 1
    return n, cs, gs, hits


def main():
    root = Path(sys.argv[1])
    out = Path(sys.argv[2])
    pairs = [
        ('cpt', root / 'factual_cpt_protected_eval.jsonl',
         [root / 'factual_cpt_train.jsonl', root / 'factual_cpt_validation.jsonl']),
        ('sft', root / 'observed_sft_protected_eval.jsonl',
         [root / 'observed_sft_train.jsonl', root / 'observed_sft_validation.jsonl']),
    ]

    receipt = {'schema': 'north_star_holdout_custody_v1', 'root': str(root),
               'families': {}, 'verdict': None}

    for fam, prot, consumed in pairs:
        if not prot.exists():
            continue
        n_p, c_p, g_p, hits = scan(prot)
        entry = {
            'protected_eval': {
                'path': prot.name,
                'bytes': prot.stat().st_size,
                'sha256': sha256_file(prot),
                'records': n_p,
            },
            'consumed_splits': [],
            'id_field_coverage': {k: v for k, v in hits.items() if v},
            'overlap': {},
        }
        c_all, g_all = set(), set()
        for c in consumed:
            if not c.exists():
                continue
            n_c, cc, gc, _ = scan(c)
            entry['consumed_splits'].append({
                'path': c.name, 'bytes': c.stat().st_size,
                'sha256': sha256_file(c), 'records': n_c})
            c_all |= cc
            g_all |= gc
        entry['overlap'] = {
            'content_key_intersection': len(c_p & c_all),
            'group_key_intersection': len(g_p & g_all),
            'group_key_available': bool(g_p),
        }
        # A degenerate content key (e.g. hashing a field that is absent in this
        # family) collapses every record into one bucket and reports CLEAN.
        # That failure mode is silent and points the wrong way, so refuse it.
        if n_p and len(c_p) < n_p * 0.9:
            entry['verdict'] = 'KEY_DEGENERATE'
            entry['key_degeneracy'] = {'records': n_p, 'distinct_content_keys': len(c_p)}
        else:
            entry['verdict'] = (
                'CLEAN' if not (c_p & c_all) and not (g_p & g_all)
                else 'LEAK'
            )
        receipt['families'][fam] = entry

    bad = [f for f, e in receipt['families'].items() if e['verdict'] != 'CLEAN']
    receipt['verdict'] = 'CLEAN' if not bad else f'LEAK: {bad}'
    receipt['note'] = (
        'Content-key overlap is the strict check. Group-key overlap protects '
        'against near-duplicates sharing an identity but differing in text; '
        'if id coverage is thin, group-level leakage can be understated.'
    )
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()