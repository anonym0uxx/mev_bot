#!/usr/bin/env python3
"""Characterize the single colliding record found by holdout_custody.py.

Identity keys are disjoint but content hashes match. Two explanations:
  (a) benign -- identical text legitimately recurring under different episodes
      (boilerplate / templated factual statement), or
  (b) a real leak -- the same episode emitted into both splits.
Distinguish by printing the colliding record and where else it appears.
"""
import hashlib
import json
import sys
from pathlib import Path


def ck(rec):
    # CPT stores text under `content`, SFT under `messages`.
    body = rec.get('messages')
    if body is None:
        body = rec.get('content')
    return hashlib.sha256(
        json.dumps(body, sort_keys=True).encode()).hexdigest()


def load(p, tag):
    out = {}
    with open(p) as fh:
        for i, line in enumerate(fh):
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except Exception:
                continue
            out.setdefault(ck(rec), []).append((tag, i, rec))
    return out


root = Path(sys.argv[1])
prot = load(root / 'factual_cpt_protected_eval.jsonl', 'protected_eval')
train = load(root / 'factual_cpt_train.jsonl', 'train')
val = load(root / 'factual_cpt_validation.jsonl', 'validation')
cons = {**train, **val} if not set(train) & set(val) else None

collide = set(prot) & (set(train) | set(val))
print(f'protected={len(prot)} train={len(train)} val={len(val)}')
print(f'colliding content hashes: {len(collide)}')

for h in collide:
    print('=' * 70)
    for tag, i, rec in prot[h]:
        meta = rec.get('meta') or {}
        print(f'[{tag} line {i}]')
        print('  meta:', json.dumps(meta)[:600])
        msgs = rec.get('messages') or []
        for m in msgs:
            print(f"  {m.get('role')}: {str(m.get('content'))[:400]}")
    for src in (train, val):
        for tag, i, rec in src.get(h, []):
            meta = rec.get('meta') or {}
            print(f'  ALSO IN [{tag} line {i}] meta: {json.dumps(meta)[:400]}')
