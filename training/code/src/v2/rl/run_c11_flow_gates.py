#!/usr/bin/env python
"""C11 gates 15-18 (spec §5). 16's causality half already proven by
/tmp/causality_audit.py (truncation: 123,840/123,840 identical; independent
recompute: 200/200) — this file re-checks coverage, citation, reversibility."""
import json, os, re, collections, sys

SRC = '/training/v2/candidate_sft_c10'
DST = '/training/v2/candidate_sft_c11'
res = {}
viol = collections.Counter()
fam_cite = collections.Counter()
fam_rows = collections.Counter()
n = flow_lines = no_flow = 0
for fn in ('train.jsonl', 'validation.jsonl', 'examination.jsonl'):
    with open(os.path.join(SRC, fn)) as f0, open(os.path.join(DST, fn)) as f1:
        for l0, l1 in zip(f0, f1):
            n += 1
            r0, r1 = json.loads(l0), json.loads(l1)
            fam = (r1.get('meta') or {}).get('family') or '?'
            fam_rows[fam] += 1
            u1, a1 = r1['messages'][1]['content'], r1['messages'][2]['content']
            u0, a0 = r0['messages'][1]['content'], r0['messages'][2]['content']
            # 15 coverage: exactly one LIVE FLOW STATE line in user msg
            ul = [x for x in u1.split('\n') if x.startswith('LIVE FLOW STATE: ')]
            if len(ul) != 1:
                viol['15_flow_block_count'] += 1; continue
            flow_lines += 1
            if 'no_prior_flow=true' in ul[0]:
                no_flow += 1
            # 17 citation: assistant ends with FLOW CITATION line
            al = a1.split('\n')
            if not al[-1].startswith('FLOW CITATION: '):
                viol['17_missing_citation'] += 1
            else:
                fam_cite[fam] += 1
                # citation values match the block values
                if al[-1][15:] != ul[0][17:]:
                    viol['17_citation_block_mismatch'] += 1
            # 18 reversibility: strip both lines -> byte-identical c10 row
            u_re = '\n'.join(x for x in u1.split('\n') if not x.startswith('LIVE FLOW STATE: '))
            a_re = '\n'.join(al[:-1])
            if u_re != u0: viol['18_user_not_reversible'] += 1
            if a_re != a0: viol['18_asst_not_reversible'] += 1
            # meta + system untouched
            if r0.get('meta') != r1.get('meta'): viol['18_meta_drift'] += 1
            if r0['messages'][0] != r1['messages'][0]: viol['18_system_drift'] += 1

res['15_rows'] = n
res['15_with_flow_block'] = flow_lines
res['15_no_prior_flow'] = no_flow
res['17_flow_cited_per_family'] = dict(fam_cite)
res['gate15'] = 'PASS' if n == 215711 and flow_lines == n else 'FAIL'
res['gate17'] = 'PASS' if all(fam_cite[f] == fam_rows[f] for f in fam_rows) and not viol.get('17_citation_block_mismatch') else 'FAIL'
res['gate18'] = 'PASS' if not any(k.startswith('18') for k in viol) else 'FAIL'
res['gate16'] = 'PASS (causality_audit: truncation 123840/123840 identical, independent recompute 200/200)'
res['violations'] = dict(viol)
print(json.dumps(res, indent=1))
sys.exit(0 if 'FAIL' not in (res['gate15'], res['gate17'], res['gate18']) else 1)
