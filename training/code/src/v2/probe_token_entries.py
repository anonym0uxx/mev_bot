#!/usr/bin/env python3
"""Do pre/post_token_balances entries always carry ui_token_amount.amount?

The resistant outlier class has a token leg ~2,000 raw units against ~0.02 SOL,
which no pool rate can produce. If one side of the delta silently reads 0
(missing field, or a differently-named field), the "delta" is really just the
other side's absolute balance -- which is exactly this signature.

Prints the distinct key-shapes seen in token balance entries, and how often the
amount path is missing or non-integer.
"""
import io
import json
import subprocess
import sys
from collections import Counter


def main():
    parts = sys.argv[1:]
    shapes = Counter()
    amt_path_ok = 0
    amt_path_missing = 0
    decimal_mismatch = 0
    for part in parts:
        p = subprocess.Popen(['zstd', '-dc', part], stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL)
        n = 0
        for line in io.TextIOWrapper(p.stdout, encoding='utf-8', errors='replace'):
            n += 1
            if n > 3000:
                break
            try:
                o = json.loads(line)
            except Exception:
                continue
            meta = ((o.get('payload') or {}).get('meta')) or {}
            for key in ('pre_token_balances', 'post_token_balances'):
                for e in (meta.get(key) or []):
                    shapes[tuple(sorted(e.keys()))] += 1
                    u = e.get('ui_token_amount')
                    if not isinstance(u, dict):
                        amt_path_missing += 1
                        continue
                    a = u.get('amount')
                    if a is None:
                        amt_path_missing += 1
                    else:
                        amt_path_ok += 1
                        try:
                            int(a)
                        except Exception:
                            decimal_mismatch += 1

    print('distinct token-balance entry shapes:')
    for k, c in shapes.most_common(8):
        print(f'  {c:>9,}  {k}')
    print(f'\namount path present : {amt_path_ok:,}')
    print(f'amount path MISSING : {amt_path_missing:,}')
    print(f'amount non-integer  : {decimal_mismatch:,}')


if __name__ == '__main__':
    main()
