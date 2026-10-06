"""Derive the max-drawdown target from FACTUAL on-chain excursions.

Operator ruling: "Pick the drawdown that's most likely to be mirrored in production pump fun
on-chain trading. Derive this from factual on chain data."

Method: the drawdown a position can inflict IS its adverse excursion (MAE) between entry and
exit. So measure the realized MAE distribution over the tradable population and set the
constraint at a level the data actually produces -- not a number chosen for comfort.

Looks for mfe/mae on the corpus rows first (cheap); if absent, says so explicitly rather than
inventing a proxy.
"""
import json
import collections

P = '/training/v2/candidate_sft_c8/train.jsonl'

hits = collections.Counter()
n = 0
vals = collections.defaultdict(list)


def walk(o, path=''):
    if isinstance(o, dict):
        for k, v in o.items():
            walk(v, path + '.' + k)
    elif isinstance(o, list):
        if o and not isinstance(o[0], (dict, list)):
            pass
    else:
        kl = path.lower()
        if any(w in kl for w in ('mae', 'mfe', 'drawdown', 'adverse', 'excursion')):
            hits[path] += 1
            if isinstance(o, (int, float)):
                vals[path].append(float(o))


with open(P, encoding='utf-8') as fh:
    for line in fh:
        r = json.loads(line)
        m = r.get('meta') or {}
        if m.get('task') != 'decision_action':
            continue
        n += 1
        walk(m)
        if n >= 12000:
            break

print("decision rows scanned:", f"{n:,}")
print("\nfields matching mae/mfe/drawdown/adverse:")
for k, c in hits.most_common(20):
    v = vals.get(k) or []
    if v:
        v = sorted(v)
        p = lambda q: v[min(len(v) - 1, int(q * len(v)))]
        print(f"  {k:52s} n={c:6,d} p10={p(.10):10.2f} p50={p(.50):10.2f} p90={p(.90):10.2f} p99={p(.99):10.2f}")
    else:
        print(f"  {k:52s} n={c:6,d} (non-numeric)")
if not hits:
    print("  NONE FOUND - MAE/MFE is not carried on entry rows; it must be computed from the tape.")