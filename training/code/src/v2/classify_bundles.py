#!/usr/bin/env python3
"""Decide whether a multi-hit residual is ONE trader or SEVERAL.

wide_resolve picks the largest token mover when more than one account fits the
sign pattern. That is correct for a single trader whose route touches several
accounts, but WRONG if the transaction bundles several independent wallets --
there the record must become multiple labelled decisions, not one.

Token-balance entries carry the wallet authority in `owner`, so two hits with
different owners are different wallets. What separates the two cases is the
SIZE distribution:

  SINGLE  one hit carries essentially all of the mint's movement; the others
          are dust/rounding. Picking the largest is correct.
  BUNDLE  two or more hits carry comparable, material amounts. These are
          independent traders and picking one silently discards the rest.

Reports the fraction of each. Only the BUNDLE class needs a schema decision.
"""
import base64
import io
import json
import subprocess
import sys
from collections import Counter

sys.path.insert(0, '.')
from renormalize_raw import (DISC, PUMP_FUN, PUMP_SWAP, resolve_trader,
                             _tok_amt, WSOL)


def main(parts):
    c = Counter()
    examples = []
    n_trade = 0
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
            keys = (list(msg.get('account_keys_b58') or [])
                    + list(meta.get('loaded_writable_addresses_b58') or [])
                    + list(meta.get('loaded_readonly_addresses_b58') or []))
            pre_sol = meta.get('pre_balances') or []
            post_sol = meta.get('post_balances') or []
            pre_tok = meta.get('pre_token_balances') or []
            post_tok = meta.get('post_token_balances') or []

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
                n_trade += 1
                side = kind[1]
                accts = list(base64.b64decode(ix.get('accounts_b64') or b''))
                mints = []
                seen = set()
                for e in post_tok:
                    m = e.get('mint')
                    if m and m not in seen:
                        seen.add(m)
                        mints.append(m)
                if any(resolve_trader(accts, keys, side, cm, pre_sol, post_sol,
                                      pre_tok, post_tok, prefer_idx=6)
                       for cm in mints):
                    continue  # resident in instruction accounts: not a residual
                for cm in mints:
                    hits = []
                    for j in range(min(len(keys), len(pre_sol), len(post_sol))):
                        ow = keys[j]
                        sd = ((post_sol[j] - pre_sol[j])
                              + (_tok_amt(post_tok, ow, WSOL)
                                 - _tok_amt(pre_tok, ow, WSOL)))
                        td = (_tok_amt(post_tok, ow, cm)
                              - _tok_amt(pre_tok, ow, cm))
                        if side == 'buy' and sd < 0 and td > 0:
                            hits.append((ow, td))
                        elif side == 'sell' and sd > 0 and td < 0:
                            hits.append((ow, td))
                    if len(hits) < 2:
                        break
                    # hit accounts are wallets; group by owner (already wallet-level)
                    mags = sorted((abs(t) for _o, t in hits), reverse=True)
                    total = sum(mags)
                    if total == 0:
                        break
                    share = mags[0] / total
                    if share >= 0.995:
                        cls = 'SINGLE_dominant'
                    elif share >= 0.90:
                        cls = 'SINGLE_probable'
                    else:
                        cls = 'BUNDLE_suspect'
                    c[cls] += 1
                    c[f'n_hits={min(len(hits), 5)}'] += 1
                    if cls.startswith('BUNDLE') and len(examples) < 8:
                        examples.append((('pumpfun' if prog == PUMP_FUN else 'pumpswap'),
                                         side, len(hits), round(share, 3),
                                         [int(m) for m in mags[:4]]))
                    break

    print(f'trade instructions examined: {n_trade:,}')
    if n_trade == 0:
        print('FAIL: examined zero trades')
        return
    print('--- multi-hit residuals: one trader or several? ---')
    for k, v in sorted(c.items(), key=lambda kv: -kv[1]):
        print(f'  {str(k):22s} {v:9,d}')
    tot = sum(v for k, v in c.items() if k.startswith(('SINGLE', 'BUNDLE')))
    if tot:
        b = sum(v for k, v in c.items() if k.startswith('BUNDLE'))
        print(f'\n  multi-hit total {tot:,}; bundle-suspect {b:,} ({b / tot * 100:.2f}%)')
    print('\n--- bundle-suspect samples: venue side n_hits top_share top4_magnitudes ---')
    for e in examples:
        print('  ', e)


if __name__ == '__main__':
    main(sys.argv[1:])