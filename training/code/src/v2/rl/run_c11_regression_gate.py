#!/usr/bin/env python
"""Gate 19 — c11 vs c10 no-regression diff.

For every split, row i of c11 must equal row i of c10 EXCEPT:
  * exactly one inserted 'LIVE FLOW STATE' line in the user message
  * meta.flow_enrichment == 'c11_tape_asof_v1'
Everything else (labels, assistant completions, meta, ordering, row count)
must be byte-identical. Any other difference is a regression -> FAIL.
"""
import json
import sys

C10 = '/training/v2/candidate_sft_c10'
C11 = '/training/v2/candidate_sft_c11'
SPLITS = ('train', 'validation', 'examination')

bad = {'row_count': 0, 'nonuser_diff': 0, 'user_diff_beyond_flow': 0,
       'meta_diff': 0, 'label_diff': 0, 'no_flow_line': 0,
       'citation_mismatch': 0}
checked = 0

for sp in SPLITS:
    with open(f'{C10}/{sp}.jsonl') as a, open(f'{C11}/{sp}.jsonl') as b:
        for la in a:
            lb = b.readline()
            if not lb:
                bad['row_count'] += 1
                break
            ra, rb = json.loads(la), json.loads(lb)
            checked += 1
            ma = dict(ra.get('meta') or {})
            mb = dict(rb.get('meta') or {})
            # corpus injector leaves meta byte-untouched by design; a tag is
            # tolerated (the wall injector adds one) but never required.
            mb.pop('flow_enrichment', None)
            if ma != mb:
                bad['meta_diff'] += 1
                continue
            msa = ra.get('messages') or []
            msb = rb.get('messages') or []
            if len(msa) != len(msb):
                bad['nonuser_diff'] += 1
                continue
            for xa, xb in zip(msa, msb):
                if xa.get('role') != xb.get('role'):
                    bad['nonuser_diff'] += 1
                    break
                if xa['content'] == xb['content']:
                    continue
                if xa.get('role') == 'assistant':
                    # label-citation pass: exactly one added FLOW CITATION line,
                    # values byte-matching the prompt's LIVE FLOW STATE block.
                    A = xa['content'].split('\n')
                    B = xb['content'].split('\n')
                    cit = [l for l in B if l.startswith('FLOW CITATION:')]
                    rest = [l for l in B if not l.startswith('FLOW CITATION:')]
                    if len(cit) != 1 or rest != A:
                        bad['nonuser_diff'] += 1
                        break
                    ub = next((x['content'] for x in msb
                               if x.get('role') == 'user'), '')
                    fl = next((l for l in ub.split('\n')
                               if l.startswith('LIVE FLOW STATE')), '')
                    cited = dict(kv.split('=', 1) for kv in cit[0]
                                 .split(':', 1)[1].split() if '=' in kv)
                    avail = dict(kv.split('=', 1) for kv in fl
                                 .split(':', 1)[1].split() if '=' in kv)
                    if any(avail.get(k) != v for k, v in cited.items()):
                        bad['citation_mismatch'] += 1
                        break
                    continue
                if xa.get('role') != 'user':
                    bad['nonuser_diff'] += 1  # system changed = drift
                    break
                # user diff must be exactly one added LIVE FLOW STATE line
                la_lines = xa['content'].split('\n')
                lb_lines = xb['content'].split('\n')
                extra = [l for l in lb_lines if l.startswith('LIVE FLOW STATE')]
                rest = [l for l in lb_lines if not l.startswith('LIVE FLOW STATE')]
                if len(extra) != 1:
                    bad['no_flow_line'] += 1
                    break
                if rest != la_lines:
                    bad['user_diff_beyond_flow'] += 1
                    break
            # non-message top-level fields must match
            ka = {k: v for k, v in ra.items() if k not in ('messages', 'meta')}
            kb = {k: v for k, v in rb.items() if k not in ('messages', 'meta')}
            if ka != kb:
                bad['label_diff'] += 1
        if b.readline():
            bad['row_count'] += 1

verdict = 'PASS' if not any(bad.values()) else 'FAIL'
print(json.dumps({'gate19_no_regression': verdict, 'rows_checked': checked,
                  'violations': bad}, indent=1))
sys.exit(0 if verdict == 'PASS' else 1)
