import json
from collections import Counter
p = '../output/qwen_curriculum_v1/sft/qwen_sft_v2.jsonl'
acts = Counter(); panel_act = Counter(); modes = Counter()
n = 0
for l in open(p, encoding='utf-8'):
    r = json.loads(l)
    if not str(r.get('id', '')).startswith('sft_cross'):
        continue
    n += 1
    modes[r.get('cross_mode')] += 1
    out = r['output']
    if out.get('mode') == 'live_action':
        panel_act[out.get('action')] += 1
        acts['BUY'] += len(out.get('buys', []))
        acts['WATCH'] += len(out.get('watch', []))
        acts['SKIP'] += sum(out.get('skip_summary', {}).values())
    else:
        for row in out.get('rankings', []):
            acts[row[2]] += 1
        panel_act['AUX_' + ('NO_BUY' if out.get('no_buy_flag') else 'HAS_BUY')] += 1
print('cross records:', n, dict(modes))
print('panel actions:', dict(panel_act))
print('candidate actions:', dict(acts))
