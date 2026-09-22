#!/usr/bin/env python3
"""Measure whether a WIDE (whole-transaction) net-position search can resolve
the MULTI_MOVER residuals -- and whether the results are trustworthy.

The current resolver only searches accounts listed in the swap instruction
(accts[:20]). On a router route the economic actor can sit outside that list, so
those swaps fail regardless of how clean the sign pattern is.

Wide pass: search every account in the transaction for the sign pattern
    buy  -> (SOL+WSOL delta) < 0 and (mint delta) > 0
    sell -> (SOL+WSOL delta) > 0 and (mint delta) < 0

A wide match is only trustworthy if it is the real counterparty, so each match is
checked for:
  * CONSERVATION  the sum of ALL accounts' deltas for the mint should be ~0
                  (tokens are not created or destroyed in a swap)
  * IS_FEEPAYER   whether the match is the tx fee payer
A wide match that breaks conservation is a false positive, not a trade.
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
    n_trade = 0
    examples = []
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
            fee_payer = keys[0] if keys else None

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
                # only look at residuals: instruction-scoped resolution failed
                if any(resolve_trader(accts, keys, side, cm, pre_sol, post_sol,
                                      pre_tok, post_tok, prefer_idx=6)
                       for cm in mints):
                    continue

                venue = 'pumpfun' if prog == PUMP_FUN else 'pumpswap'
                c[(venue, 'residual')] += 1
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
                            hits.append((j, ow, td, sd))
                        elif side == 'sell' and sd > 0 and td < 0:
                            hits.append((j, ow, td, sd))
                    if not hits:
                        c[(venue, 'wide_NO_MATCH')] += 1
                        continue
                    c[(venue, 'wide_MATCH')] += 1
                    if len(hits) == 1:
                        c[(venue, 'wide_UNIQUE')] += 1
                    else:
                        c[(venue, 'wide_MULTI')] += 1
                    # conservation: sum of every account's delta for this mint
                    tot = 0
                    owners = {e.get('owner') for e in list(pre_tok) + list(post_tok)
                              if e.get('mint') == cm and e.get('owner')}
                    for ow in owners:
                        tot += (_tok_amt(post_tok, ow, cm) - _tok_amt(pre_tok, ow, cm))
                    if abs(tot) <= max(1, abs(hits[0][2]) // 1000):
                        c[(venue, 'conserved')] += 1
                    else:
                        c[(venue, 'NOT_conserved')] += 1
                    if hits[0][1] == fee_payer:
                        c[(venue, 'is_feepayer')] += 1
                    if len(examples) < 6:
                        examples.append((venue, side, len(keys), len(mints),
                                         len(hits), hits[0][2], hits[0][3],
                                         hits[0][1] == fee_payer, tot))
                    break

    print(f'trade instructions examined: {n_trade:,}')
    if n_trade == 0:
        print('FAIL: examined zero trades')
        return
    print('--- wide net-position resolution on residuals ---')
    for k, v in sorted(c.items(), key=lambda kv: -kv[1]):
        print(f'  {str(k):40s} {v:9,d}')
    print('\n--- samples: venue side n_keys n_mints n_hits tok_delta sol_delta is_feepayer conservation_sum ---')
    for e in examples:
        print('  ', e)


if __name__ == '__main__':
    main(sys.argv[1:])