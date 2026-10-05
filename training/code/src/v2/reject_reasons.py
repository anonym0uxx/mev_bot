#!/usr/bin/env python3
"""Why does resolve_trader reject 57% of swap candidates?

Buckets every swap instruction by the first reason it fails, split by venue.
The distinction that matters: a rejection caused by a bug (wrong side, wrong
account indices) looks different from one caused by an unresolvable route
(no single account performs both legs).

Reasons, in the order resolve_trader tests them:
  NOT_SIDE          -- instruction did not classify as buy/sell at all
  LEN_MISMATCH      -- pre/post balance arrays differ in length
  NO_MINT           -- no token-balance entry for a usable mint
  NO_CANDIDATE      -- no account in the instruction list has both legs
  OK                -- a unique valid swapper was found
"""
import base64
import io
import json
import subprocess
import sys
from collections import Counter

PUMP = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
SWAP = 'pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA'
DISC = {
    (PUMP, bytes([102, 6, 61, 18, 1, 218, 235, 234])): 'buy',
    (PUMP, bytes([184, 23, 238, 97, 103, 197, 211, 61])): 'buy',
    (PUMP, bytes([51, 230, 133, 164, 1, 127, 131, 173])): 'sell',
    (PUMP, bytes([93, 246, 130, 60, 231, 233, 64, 178])): 'sell',
    (SWAP, bytes([102, 6, 61, 18, 1, 218, 235, 234])): 'buy',
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


def main(parts, per_file=2000):
    c = Counter()
    seen_discs = Counter()
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
            pre_sol = meta.get('pre_balances') or []
            post_sol = meta.get('post_balances') or []
            pre_tok = meta.get('pre_token_balances') or []
            post_tok = meta.get('post_token_balances') or []
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
                prog = 'pumpfun' if keys[pi] == PUMP else 'pumpswap'
                seen_discs[(prog, tuple(raw[:8]))] += 1
                side = DISC.get((keys[pi], bytes(raw[:8])))
                if side is None:
                    c[(prog, 'NOT_SIDE')] += 1
                    continue
                if len(pre_sol) != len(post_sol):
                    c[(prog, 'LEN_MISMATCH')] += 1
                    continue
                mints = [e.get('mint') for e in (post_tok or pre_tok) if e.get('mint')]
                mints = [m for m in mints if m]
                if not mints:
                    c[(prog, 'NO_MINT')] += 1
                    continue
                ok = 0
                try:
                    accts = list(base64.b64decode(ix.get('accounts_b64') or ''))
                except Exception:
                    accts = []
                for mint in set(mints):
                    for ai in accts[:20]:
                        if ai >= len(keys) or ai >= len(pre_sol):
                            continue
                        ow = keys[ai]
                        sd = post_sol[ai] - pre_sol[ai]
                        td = tamt(post_tok, ow, mint) - tamt(pre_tok, ow, mint)
                        if side == 'buy' and sd < 0 and td > 0:
                            ok += 1
                        elif side == 'sell' and sd > 0 and td < 0:
                            ok += 1
                c[(prog, 'OK' if ok else 'NO_CANDIDATE')] += 1

    print(f'{"venue":9s} {"reason":14s} {"count":>8s}')
    for (prog, rs), n in sorted(c.items()):
        print(f'{prog:9s} {rs:14s} {n:8d}')
    print('\ndiscriminators seen by program:')
    for (prog, disc), n in seen_discs.most_common(12):
        print(f'  {prog:9s} {list(disc)!s:44s} {n:8d}')


if __name__ == '__main__':
    main(sys.argv[1:])
