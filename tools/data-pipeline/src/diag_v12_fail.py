import json, math, sys
sys.path.insert(0, '.')
p = '../output/qwen_curriculum_v1/eval_v1_2/qwen_eval_v1_2.jsonl'
recs = [json.loads(l) for l in open(p, encoding='utf-8') if l.strip()]
panels = [r for r in recs if r.get('candidates')]

def isnan(v):
    return isinstance(v, float) and math.isnan(v)

# NaN utilities: where and why
nan_c = [(r['id'], r['panel_source'], c) for r in panels for c in r['candidates']
         if c['continuous_utility']['robust_executable_utility_v1'] is None
         or isnan(c['continuous_utility']['robust_executable_utility_v1'])]
print('NaN utils:', len(nan_c))
for pid, src, c in nan_c[:3]:
    cu = c['continuous_utility']
    print(pid, src, {k: cu[k] for k in cu})

# gate inputs across all candidates
from collections import Counter
stats = Counter()
near = 0
for r in panels:
    for c in r['candidates']:
        cu = c['continuous_utility']; l2 = c['l2_evidence'] or {}
        feas = cu.get('feasibility_score') or 0.0
        mae, mfe = l2.get('mae_bp'), l2.get('mfe_bp')
        surv = l2.get('survived_60s')
        util = cu.get('robust_executable_utility_v1')
        stats['feas>=0.8'] += feas >= 0.8
        stats['surv_true'] += bool(surv) and not isnan(surv if isinstance(surv, float) else 0.0)
        stats['surv_nan'] += isnan(surv) if isinstance(surv, float) else 0
        stats['mae_null'] += mae is None or isnan(mae)
        stats['mfe_null'] += mfe is None or isnan(mfe)
        stats['mae_ok'] += (mae is not None and not isnan(mae) and mae > -2000)
        stats['mfe_ok'] += (mfe is not None and not isnan(mfe) and mfe > 3000)
        stats['util>0'] += (util is not None and not isnan(util) and util > 0)
        allok = (feas >= 0.8 and bool(surv) and util and util > 0
                 and mae is not None and not isnan(mae) and mae > -2000
                 and mfe is not None and not isnan(mfe) and mfe > 3000)
        stats['ALL'] += bool(allok)
        if (mfe is not None and not isnan(mfe) and mfe > 3000
                and feas >= 0.8 and util and util > 0):
            near += 1
print(dict(stats), 'near(mfe+feas+util):', near)

# distribution of mfe/mae for slinky
import statistics
mfes = [c['l2_evidence'].get('mfe_bp') for r in panels for c in r['candidates']
        if c.get('l2_evidence') and c['l2_evidence'].get('mfe_bp') is not None
        and not isnan(c['l2_evidence'].get('mfe_bp'))]
maes = [c['l2_evidence'].get('mae_bp') for r in panels for c in r['candidates']
        if c.get('l2_evidence') and c['l2_evidence'].get('mae_bp') is not None
        and not isnan(c['l2_evidence'].get('mae_bp'))]
sv = [c['l2_evidence'].get('survived_60s') for r in panels for c in r['candidates'] if c.get('l2_evidence')]
print('mfe n=%d p50=%.0f p90=%.0f max=%.0f' % (len(mfes), statistics.median(mfes), sorted(mfes)[int(len(mfes)*0.9)], max(mfes)))
print('mae n=%d p50=%.0f p10=%.0f min=%.0f' % (len(maes), statistics.median(maes), sorted(maes)[int(len(maes)*0.1)], min(maes)))
print('survived types:', Counter(type(x).__name__ for x in sv), 'true:', sum(1 for x in sv if x is True or x == 1))
