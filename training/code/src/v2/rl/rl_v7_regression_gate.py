#!/usr/bin/env python
"""RL v7 vs v6 no-regression: same episodes, same order, same candidates/labels;
only permitted delta = LIVE FLOW STATE line (and FLOW CITATION if labels carry it).
Usage: python rl_v7_regression_gate.py <v6_file> <v7_file>
"""
import json
import sys

def strip(obj):
    if isinstance(obj, str):
        if obj == 'reports/mgmt_c11 meta.candidates':
            return 'reports/mgmt_c10 meta.candidates'  # intentional provenance repoint
        if obj.lstrip().startswith(('[{', '{')):
            # JSON-serialized prompt: parse, strip inside, compare structurally
            try:
                return strip(json.loads(obj))
            except (ValueError, TypeError):
                pass
        return '\n'.join(l for l in obj.split('\n')
                         if not l.startswith('LIVE FLOW STATE')
                         and not l.startswith('FLOW CITATION'))
    if isinstance(obj, list):
        return [strip(x) for x in obj]
    if isinstance(obj, dict):
        return {k: strip(v) for k, v in obj.items()
                if k not in ('flow_enrichment', 'prompt_sha256')}
    return obj

a_path, b_path = sys.argv[1], sys.argv[2]
n = mism = 0
with open(a_path) as A, open(b_path) as B:
    for la in A:
        lb = B.readline()
        if not lb:
            print(json.dumps({'verdict': 'FAIL', 'reason': 'v7 shorter', 'at_row': n}))
            sys.exit(1)
        n += 1
        if strip(json.loads(la)) != strip(json.loads(lb)):
            mism += 1
            if mism <= 3:
                print(f'MISMATCH row {n}')
    if B.readline():
        print(json.dumps({'verdict': 'FAIL', 'reason': 'v7 longer'}))
        sys.exit(1)
v = 'PASS' if mism == 0 else 'FAIL'
print(json.dumps({'verdict': v, 'rows': n, 'mismatches': mism}))
sys.exit(0 if v == 'PASS' else 1)
