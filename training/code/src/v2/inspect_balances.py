#!/usr/bin/env python3
"""Work out how to derive the SOL leg of a swap from capture event fields."""
import json
from collections import Counter

path = '/tmp/ev_sample.ndjson'
shown = 0
side_counts = Counter()
for line in open(path):
    o = json.loads(line)
    ai, ao = o.get('amount_in'), o.get('amount_out')
    if ai is None and ao is None:
        continue
    side = o.get('trade_side')
    side_counts[(side, ai is not None, ao is not None)] += 1
    if shown < 2:
        ak = o.get('account_keys_b58')
        pre = o.get('pre_sol_balances')
        post = o.get('post_sol_balances')
        print('=' * 74)
        print('side:', side, '| venue:', o.get('venue'), '| type:', o.get('event_type'))
        print('  amount_in :', ai)
        print('  amount_out:', ao)
        print('  min_amount_out:', o.get('min_amount_out'), '| max_amount_in:', o.get('max_amount_in'))
        print('  fee_bps:', o.get('fee_bps'))
        print('  n account_keys:', len(ak) if isinstance(ak, list) else ak)
        print('  n pre_sol_balances:', len(pre) if isinstance(pre, list) else pre)
        print('  n post_sol_balances:', len(post) if isinstance(post, list) else post)
        if isinstance(pre, list) and isinstance(post, list) and len(pre) == len(post):
            print('  sol deltas:', [b - a for a, b in zip(pre, post)][:10])
        ptb = o.get('post_token_balances_json')
        print('  post_token_balances_json (head):', str(ptb)[:260])
        print('  inner_ix (head):', str(o.get('inner_instructions_json'))[:200])
        shown += 1
    if shown >= 2 and sum(side_counts.values()) > 60000:
        break

print()
print('side/amount-shape counts:', dict(side_counts.most_common(8)))
