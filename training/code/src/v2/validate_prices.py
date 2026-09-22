#!/usr/bin/env python3
"""Validate capture-derived swap prices.

Two SOL-leg conventions exist in the capture:
  A) trader balance delta   (post_sol - pre_sol at the trader's key index)
  B) the swap's own bound   (min_amount_out for a sell, max_amount_in for a buy)

If both describe the same trade, price_A / price_B must cluster near 1. A fat
tail means one convention is wrong -- which is exactly what a physically
impossible mean (+1e18x) implies.

Read-only over a capture events file.
"""
import json
import subprocess
import sys
from pathlib import Path


def stream(path: Path):
    p = subprocess.Popen(['zstd', '-dc', str(path)],
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    for line in p.stdout:
        if len(line) < 3:
            continue
        yield json.loads(line)
    p.stdout.close()
    p.wait()


def main():
    f = Path(sys.argv[1])
    n = int(sys.argv[2]) if len(sys.argv) > 2 else 400000

    both = 0
    only_a = 0
    only_b = 0
    neither = 0
    ratios = []

    for i, o in enumerate(stream(f)):
        if i >= n:
            break
        if o.get('record_type') not in (None, 'transaction'):
            continue
        if not (o.get('trade_side') or o.get('event_type')):
            continue
        ai, ao = o.get('amount_in'), o.get('amount_out')
        side = o.get('trade_side') or o.get('event_type')
        tok = ai if side == 'sell' else (ao if side == 'buy' else None)
        if not tok:
            continue
        ak = list(o.get('account_keys_b58') or [])
        ak += list(o.get('loaded_writable_addresses_b58') or [])
        ak += list(o.get('loaded_readonly_addresses_b58') or [])
        pre = o.get('pre_sol_balances') or []
        post = o.get('post_sol_balances') or []
        trader = o.get('trader_b58')

        a = None
        if trader and len(pre) == len(post) and trader in ak:
            k = ak.index(trader)
            if k < len(pre):
                a = post[k] - pre[k]
        b = o.get('min_amount_out') if side == 'sell' else o.get('max_amount_in')

        if a and b:
            both += 1
            try:
                ratios.append(abs(float(a)) / abs(float(b)))
            except ZeroDivisionError:
                pass
        elif a:
            only_a += 1
        elif b:
            only_b += 1
        else:
            neither += 1

    ratios.sort()

    def q(p):
        if not ratios:
            return None
        return ratios[min(len(ratios) - 1, int(p * len(ratios)))]

    print(json.dumps({
        'events_with_token_amount': both + only_a + only_b + neither,
        'both_conventions_present': both,
        'only_balance_delta': only_a,
        'only_fallback_bound': only_b,
        'neither': neither,
        'a_over_b_quantiles': {
            'p01': q(0.01), 'p10': q(0.10), 'p50': q(0.50),
            'p90': q(0.90), 'p99': q(0.99), 'max': ratios[-1] if ratios else None,
        },
        'agree_within_5pct': (sum(1 for r in ratios if 0.95 <= r <= 1.05) / len(ratios)) if ratios else None,
    }, indent=2))


if __name__ == '__main__':
    main()
