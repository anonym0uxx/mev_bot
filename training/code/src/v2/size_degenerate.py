#!/usr/bin/env python3
"""Size the degenerate-trade population before choosing a filter threshold.

Suspicion: trades whose token leg is a handful of raw units carry no price
information (price = sol/token explodes), and they are what put p99 at 3,711x.

Reports the token-delta distribution and how much of the SOL notional sits in
the degenerate tail. Prints numbers only -- the threshold choice stays explicit.
"""
import json
import sys

BINS = [0, 1, 10, 100, 1_000, 10_000, 100_000, 1_000_000, 10_000_000, 10**9, 10**12]


def main():
    path = sys.argv[1]
    n = 0
    hist = [0] * (len(BINS))
    rent_like = 0
    tot_abs_sol = 0
    degen_abs_sol = 0
    with open(path) as f:
        for line in f:
            try:
                t = json.loads(line)
            except Exception:
                continue
            sol = t.get('sol_lamports')
            tok = t.get('tokens_raw')
            if not sol or not tok:
                continue
            n += 1
            a_tok = abs(tok)
            a_sol = abs(sol)
            tot_abs_sol += a_sol
            b = 0
            for i, edge in enumerate(BINS):
                if a_tok >= edge:
                    b = i
            hist[b] += 1
            if a_tok <= 1:
                degen_abs_sol += a_sol
                if 800_000 <= a_sol <= 2_100_000:
                    rent_like += 1

    print(f'trades with both legs: {n:,}')
    print(f'\ntoken-delta magnitude histogram (raw units):')
    for i, edge in enumerate(BINS):
        lo = edge
        hi = BINS[i + 1] if i + 1 < len(BINS) else None
        label = f'{lo:>15,} .. ' + (f'{hi:>15,}' if hi else f'{"inf":>15}')
        share = hist[i] / n if n else 0
        print(f'  {label}  {hist[i]:>10,}  {share*100:6.2f}%')
    print(f'\ntrades with |tokens_raw| <= 1: {hist[0]:,}')
    print(f'  of those, |sol| in rent-exempt band (0.0008-0.0021 SOL): {rent_like:,}')
    print(f'\nnotional share held by |tokens_raw| <= 1: '
          f'{degen_abs_sol/tot_abs_sol*100:.6f}% of total lamport movement')


if __name__ == '__main__':
    main()
