#!/usr/bin/env python3
"""Split rejected swaps by transaction status.

A failed transaction has no balance movement at all, so every one of its swap
instructions is unresolvable by construction. Those are not lost data -- they
are not trades. If most rejections are failed txs, the real coverage gap is far
smaller than the raw rejection count suggests, and the fix is to exclude failed
txs up front rather than to invent a trader for them.
"""
import base64
import io
import json
import subprocess
import sys
from collections import Counter

PUMP = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
SWAP = 'pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA'
WSOL = 'So11111111111111111111111111111111111111112'
DISC = {
    (PUMP, bytes([102, 6, 61, 18, 1, 218, 235, 234])): 'buy',
    (PUMP, bytes([184, 23, 238, 97, 103, 197, 211, 61])): 'buy',
    (PUMP, bytes([56, 252, 116, 8, 158, 223, 205, 95])): 'buy',
    (PUMP, bytes([51, 230, 133, 164, 1, 127, 131, 173])): 'sell',
    (PUMP, bytes([93, 246, 130, 60, 231, 233, 64, 178])): 'sell',
    (SWAP, bytes([102, 6, 61, 18, 1, 218, 235, 234])): 'buy',
    (SWAP, bytes([198, 46, 21, 82, 180, 217, 232, 112])): 'buy',
    (SWAP, bytes([51, 230, 133, 164, 1, 127, 131, 173])): 'sell',
}


def tamt(entries, owner, mint):
    for e in entries or []:
        if e.get('owner') == owner and e.get('mint') == mint:
            try:
                return int((e.get('ui_token_amount') or {}).get('amount') or 0)
            except Exception:
                return 0
    return 0


def main(parts, per_file=1200):
    c = Counter()
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
            ok = bool(meta.get('err_is_none', True))
            keys = (list(msg.get('account_keys_b58') or [])
                    + list(meta.get('loaded_writable_addresses_b58') or [])
                    + list(meta.get('loaded_readonly_addresses_b58') or []))
            pre_sol, post_sol = meta.get('pre_balances') or [], meta.get('post_balances') or []
            pre_tok, post_tok = meta.get('pre_token_balances') or [], meta.get('post_token_balances') or []
            if len(pre_sol) != len(post_sol) or not pre_sol:
                continue
            n_inner = sum(len(ix.get('instructions') or [])
                          for ix in (meta.get('inner_instructions') or []))
            for ix in (msg.get('instructions') or []):
                pi = ix.get('program_id_index')
                if pi is None or pi >= len(keys) or keys[pi] not in (PUMP, SWAP):
                    continue
                d = ix.get('data_b64')
                if not d:
                    continue
                try:
                    raw = base64.b64decode(d)
                except Exception:
                    continue
                prog = keys[pi]
                side = DISC.get((prog, bytes(raw[:8])))
                if side is None:
                    continue
                venue = 'pumpfun' if prog == PUMP else 'pumpswap'
                try:
                    accts = list(base64.b64decode(ix.get('accounts_b64') or ''))
                except Exception:
                    accts = []
                mints = []
                for e in list(post_tok or []) + list(pre_tok or []):
                    m = e.get('mint')
                    if m and m != WSOL and m not in mints:
                        mints.append(m)
                if not mints:
                    c[('NO_MINT', 'ok' if ok else 'FAILED')] += 1
                    continue
                fixed = False
                for mint in mints:
                    for ai in accts[:20]:
                        if ai >= len(pre_sol):
                            continue
                        ow = keys[ai]
                        sd = ((post_sol[ai] - pre_sol[ai])
                              + tamt(post_tok, ow, WSOL) - tamt(pre_tok, ow, WSOL))
                        td = tamt(post_tok, ow, mint) - tamt(pre_tok, ow, mint)
                        if (side == 'buy' and sd < 0 and td > 0) or \
                           (side == 'sell' and sd > 0 and td < 0):
                            fixed = True
                            break
                    if fixed:
                        break
                if fixed:
                    c[(venue, 'RESOLVED', 'ok' if ok else 'FAILED')] += 1
                else:
                    cls = 'CPI' if n_inner else 'FLAT'
                    c[(venue, f'REJECT_{cls}', 'ok' if ok else 'FAILED')] += 1

    for k, n in sorted(c.items(), key=lambda kv: -kv[1]):
        print(f'{str(k):50s} {n:9,d}')
    tot_ok = sum(n for k, n in c.items() if k[-1] == 'ok')
    tot_f = sum(n for k, n in c.items() if k[-1] == 'FAILED')
    res_ok = sum(n for k, n in c.items() if 'RESOLVED' in str(k) and k[-1] == 'ok')
    rej_ok = sum(n for k, n in c.items() if 'REJECT' in str(k) and k[-1] == 'ok')
    print(f'\nsuccessful-tx swaps resolved : {res_ok:,}')
    print(f'successful-tx swaps rejected : {rej_ok:,}')
    rate = (rej_ok / (res_ok + rej_ok) * 100) if (res_ok + rej_ok) else 0
    print(f'=> rejection rate among SUCCESSFUL txs: {rate:.1f}%')
    print(f'total swap instructions ok/failed: {tot_ok:,} / {tot_f:,}')


if __name__ == '__main__':
    main(sys.argv[1:])
