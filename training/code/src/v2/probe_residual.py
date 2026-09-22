#!/usr/bin/env python3
"""Characterize the swaps that STILL fail resolution after the inner-instruction
scan. This is the class the operator flagged: aggregator routes and multi-wallet
traders. Before excluding them we need to know what they actually are.

Classifies each residual by shape:
  TOKEN_NO_VALUE  - an account moves the token but no account moves value
  VALUE_NO_TOKEN  - value moves but no token leg
  NEITHER         - nothing moves at all (should be empty on successful txs)
  MULTI_MOVER     - several distinct accounts move the token (multi-wallet)
"""
import base64
import io
import json
import subprocess
import sys
from collections import Counter

sys.path.insert(0, '.')
from renormalize_raw import (DISC, PUMP_FUN, PUMP_SWAP, NOT_A_LAUNCH,
                             resolve_trader, full_keys, token_delta, WSOL)


def tok_entries(meta, key):
    return meta.get(key) or []


def main(parts, limit=12):
    c = Counter()
    samples = []
    n_examined = 0
    for part in parts:
        p = subprocess.Popen(['zstd', '-dc', part], stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL)
        for line in io.TextIOWrapper(p.stdout, encoding='utf-8', errors='replace'):
            try:
                o = json.loads(line)
            except Exception:
                continue
            if o.get('record_type') != 'transaction':
                continue
            pl = o.get('payload') or {}
            msg = pl.get('message') or {}
            meta = pl.get('meta') or {}
            if not meta.get('err_is_none', True):
                continue
            keys = full_keys(msg, meta)
            pre_sol = meta.get('pre_balances') or []
            post_sol = meta.get('post_balances') or []
            pre_tok = tok_entries(meta, 'pre_token_balances')
            post_tok = tok_entries(meta, 'post_token_balances')

            inner = []
            for g in (meta.get('inner_instructions') or []):
                inner.extend(g.get('instructions') or [])
            for ix in list(msg.get('instructions') or []) + inner:
                pi = ix.get('program_id_index')
                d = ix.get('data_b64')
                if pi is None or d is None or pi >= len(keys):
                    continue
                prog = keys[pi]
                if prog not in (PUMP_FUN, PUMP_SWAP):
                    continue
                raw = base64.b64decode(d)
                if len(raw) < 8:
                    continue
                kind = DISC.get((prog, raw[:8]))
                if kind is None or kind[0] not in ('buy', 'sell'):
                    continue
                side = kind[1]
                n_examined += 1
                accts = list(base64.b64decode(ix.get('accounts_b64') or b''))
                prefer = 6
                mints = []
                for e in post_tok:
                    m = e.get('mint')
                    if m:
                        mints.append(m)
                seen = set()
                mints = [m for m in mints if not (m in seen or seen.add(m))]
                ok = False
                for cm in mints:
                    if resolve_trader(accts, keys, side, cm, pre_sol, post_sol,
                                      pre_tok, post_tok, prefer_idx=prefer):
                        ok = True
                        break
                if ok:
                    continue

                venue = 'pumpfun' if prog == PUMP_FUN else 'pumpswap'
                # who moved each side?
                tok_movers = set()
                for e in post_tok + pre_tok:
                    ow = e.get('owner')
                    if ow and token_delta(pre_tok, post_tok, ow, e.get('mint')):
                        tok_movers.add(ow)
                sol_movers = set()
                for k2 in range(min(len(pre_sol), len(post_sol))):
                    if post_sol[k2] != pre_sol[k2] and post_sol[k2] > 0:
                        sol_movers.add(keys[k2] if k2 < len(keys) else f'#{k2}')
                has_wsol = any(m == WSOL for m in mints)
                if not tok_movers and not sol_movers:
                    shape = 'NEITHER'
                elif tok_movers and not sol_movers and not has_wsol:
                    shape = 'TOKEN_NO_VALUE'
                elif sol_movers and not tok_movers:
                    shape = 'VALUE_NO_TOKEN'
                elif len(tok_movers) > 1:
                    shape = 'MULTI_MOVER'
                else:
                    shape = 'OTHER'
                c[(venue, shape)] += 1
                c[(venue, f'has_wsol={has_wsol}')] += 1
                c[(venue, f'tok_movers={min(len(tok_movers),4)}')] += 1
                if len(samples) < limit:
                    samples.append((venue, side, len(accts), len(mints),
                                    len(tok_movers), len(sol_movers), shape))

    print(f'trade instructions examined: {n_examined:,}')
    if n_examined == 0:
        print('FAIL: examined zero trades -- clean result is not trustworthy')
        return
    print('--- residual rejections (successful txs, outer+inner scanned) ---')
    for k, v in sorted(c.items(), key=lambda kv: -kv[1]):
        print(f'  {str(k):45s} {v:8,d}')
    print(f'\n--- {len(samples)} samples: venue side n_accts n_mints tok_movers sol_movers shape ---')
    for s in samples:
        print('  ', s)


if __name__ == '__main__':
    main(sys.argv[1:])