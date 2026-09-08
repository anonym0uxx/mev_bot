import json, os
from collections import Counter

d = os.path.join(os.path.dirname(__file__), '..', 'output', 'qwen_curriculum_v1')
for sub in ('eval', 'eval_v1_1'):
    p = os.path.join(d, sub)
    if os.path.isdir(p):
        print(sub, '->', os.listdir(p))

path = os.path.join(d, 'eval_v1_1', 'qwen_eval_v1_1.jsonl')
recs = [json.loads(l) for l in open(path, encoding='utf-8') if l.strip()]
print('total records:', len(recs))
print('top-level keys of rec0:', sorted(recs[0].keys()))
cat = Counter(r.get('eval_category', r.get('category', '?')) for r in recs)
print('categories:', dict(cat))
src = Counter(r.get('panel_source', r.get('source_corpus', '?')) for r in recs)
print('sources:', dict(src))
# how many have candidates (cross panels) vs rust etc
n_cross = sum(1 for r in recs if r.get('candidates'))
print('with candidates:', n_cross)
for r in recs:
    if r.get('candidates'):
        c = r['candidates'][0]
        print('panel keys:', sorted(r.keys()))
        print('candidate keys:', sorted(c.keys()))
        print('provenance_ids:', c.get('provenance_ids'))
        print('has causal_state fields:', len(c.get('causal_state') or {}))
        cu = c.get('continuous_utility') or {}
        print('utility sample:', {k: cu.get(k) for k in list(cu)[:4]})
        break
# a rust record
for r in recs:
    if not r.get('candidates'):
        print('non-cross rec keys:', sorted(r.keys()), '| cat:', r.get('eval_category', r.get('category')))
        break
