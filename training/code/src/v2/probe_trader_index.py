#!/usr/bin/env python3
"""Determine the trader's account index empirically, per program and side.

The layout index is knowable but I would rather measure it than assert it: for
every candidate index we compute the account's SOL delta and its token delta for
the traded mint, and check the sign pattern a real trade must show.

  buy  -> SOL delta < 0 (pays)   and token delta > 0 (receives)
  sell -> SOL delta > 0 (receives) and token delta < 0 (gives)

The index that satisfies this on ~every observation is the trader.
"""
import base64
import io
import json
import subprocess
import sys
from collections import Counter, defaultdict

PUMP = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
SWAP = 'pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA'
DISC = {
    (102, 6, 61, 18, 1, 218, 235, 234): ('buy', 'pumpfun'),
    (184, 23, 238, 97, 103, 197, 211, 61): ('buy', 'pumpfun'),
    (51, 230, 133, 164, 1, 127, 131, 173): ('sell', 'pumpfun'),
}
SWAP_DISC_ANY = True   # pumpswap layouts differ; scan rather than assume


def amt(entries, owner, mint):
    for e in entries or []:
        if e.get('owner') == owner and e.get('mint') == mint:
            try:
                return int((e.get('ui_token_amount') or {}).get('amount') or 0)
            except Exception:
                return 0
    return 0


def main(parts, per_file=2500):
    good = defaultdict(Counter)
    total = Counter()
    for part in parts:
        p = subprocess.Popen(['zstd', '-dc', part], stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL)
        n = 0
        for line in io.TextIOWrapper(p.stdout, encoding='utf-8', errors='replace'):
            n += 1
            if n > per_file:
                break
            try:
                o = json.loads(line)
            except Exception:
                continue
            if o.get('record_type') != 'transaction':
                continue
            pl = o.get('payload') or {}
            msg = pl.get('message') or {}
            meta = pl.get('meta') or {}
            keys = (list(msg.get('account_keys_b58') or [])
                    + list(meta.get('loaded_writable_addresses_b58') or [])
                    + list(meta.get('loaded_readonly_addresses_b58') or []))
            pre_sol, post_sol = meta.get('pre_balances') or [], meta.get('post_balances') or []
            pre_tok, post_tok = meta.get('pre_token_balances') or [], meta.get('post_token_balances') or []
            if len(pre_sol) != len(post_sol) or not pre_sol:
                continue
            for ix in (msg.get('instructions') or []):
                pi = ix.get('program_id_index')
                if pi is None or pi >= len(keys) or keys[pi] not in (PUMP, SWAP):
                    continue
                d = ix.get('data_b64')
                if not d:
                    continue
                try:
                    raw = base64.b64decode(d)
                    accts = list(base64.b64decode(ix.get('accounts_b64') or ''))
                except Exception:
                    continue
                prog = 'pumpfun' if keys[pi] == PUMP else 'pumpswap'
                side = None
                if prog == 'pumpfun':
                    hit = DISC.get(tuple(raw[:8]))
                    if not hit:
                        continue
                    side = hit[0]
                else:
                    # pumpswap: infer side from the sign of the pool's own move
                    side = None
                if side is None:
                    continue
                # mint = the mint whose owner set shows a token move
                mints = {e.get('mint') for e in (pre_tok + post_tok) if e.get('mint')}
                for mint in mints:
                    if not mint:
                        continue
                    for idx, ai in enumerate(accts[:14]):
                        if ai >= len(keys):
                            continue
                        ow = keys[ai]
                        sd = post_sol[ai] - pre_sol[ai]
                        td = amt(post_tok, ow, mint) - amt(pre_tok, ow, mint)
                        if side == 'buy' and sd < 0 and td > 0:
                            good[(prog, side, idx)][mint] += 1
                        if side == 'sell' and sd > 0 and td < 0:
                            good[(prog, side, idx)][mint] += 1
                    total[(prog, side)] += 1

    print(f'{"program":9s} {"side":5s} {"idx":>3s} {"hits":>8s} {"distinct mints":>14s}')
    for (prog, side), _ in sorted(total.items()):
        rows = [(k[2], sum(v.values()), len(v)) for k, v in good.items()
                if k[0] == prog and k[1] == side]
        rows.sort(key=lambda r: -r[1])
        for idx, hits, mints in rows[:6]:
            print(f'{prog:9s} {side:5s} {idx:3d} {hits:8d} {mints:14d}')
        print(f'  (side observations: {total[(prog, side)]:,})')


if __name__ == '__main__':
    main(sys.argv[1:])
