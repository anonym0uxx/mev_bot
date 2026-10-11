"""Regression: comparison key is (mint, t_dec_ms, family). Two corpus rows with the same mint and clock but different
families must never overwrite the decision row. Run: python3 test_fielddiff_key.py"""
import re
def index(rows, mint):
    out = {}
    for r in rows:
        if r.get('mint') != mint or r.get('family') != 'decision':
            continue
        t = int(re.search('t_dec_ms=([0-9]+)', r['prompt']).group(1))
        assert (mint, t) not in out, 'duplicate decision row for key'
        out[(mint, t)] = r
    return out
rows = [{'mint': 'M', 'family': 'decision', 'prompt': 't_dec_ms=5 a'},
        {'mint': 'M', 'family': 'utility_regression', 'prompt': 't_dec_ms=5 b'}]
assert index(rows, 'M')[('M', 5)]['prompt'].endswith(' a')
assert index(list(reversed(rows)), 'M')[('M', 5)]['prompt'].endswith(' a')
print('ok')
