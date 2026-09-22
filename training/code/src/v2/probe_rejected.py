#!/usr/bin/env python3
"""What do the REJECTED swaps actually look like?

resolve_trader fails when no account in the instruction shows both legs. Before
excluding those swaps from the SFT set, classify them. Suspects:

  A. ROUTER/CPI   -- the real swap is an inner instruction; the outer pump/
                     pumpswap ix is a wrapper, so the swapper's legs sit on a
                     different account set.
  B. MULTI-HOP    -- value passes through an intermediate mint (e.g. a route
                     that touches a third token), so the "value leg" is not
                     native SOL or WSOL.
  C. FEE-ONLY     -- the instruction touched a mint but no account moved it
                     (dust, closed account, or a non-trade).

For each rejected instruction this prints which shape it matches, plus the
distribution of how many accounts DID move each leg independently. If a large
share is shape A or B, the swap is recoverable and should be labelled; if C,
dropping it is correct.
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


def main(parts, per_file=1200, max_cases=6):
    shapes = Counter()
    examples = []
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
            n_inner = sum(len(ix.get('instructions') or [])
                          for ix in (meta.get('inner_instructions') or []))
            outer_progs = set()
            for ix in (msg.get('instructions') or []):
                pi = ix.get('program_id_index')
                if pi is not None and pi < len(keys):
                    outer_progs.add(keys[pi])
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
                    shapes[('NO_MINT',)] += 1
                    continue
                # does ANY account satisfy the pattern for ANY mint?
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
                    continue
                # classify the reject
                wsol_moves = 0
                tok_movers = set()
                for e in list(post_tok or []) + list(pre_tok or []):
                    ow = e.get('owner')
                    if ow and e.get('mint'):
                        tok_movers.add(ow)
                for ow in list(tok_movers)[:40]:
                    if tamt(post_tok, ow, WSOL) - tamt(pre_tok, ow, WSOL):
                        wsol_moves += 1
                venue = 'pumpfun' if prog == PUMP else 'pumpswap'
                if n_inner > 0:
                    cls = 'ROUTER_OR_CPI'
                elif not wsol_moves:
                    cls = 'NO_VALUE_LEG'
                else:
                    cls = 'OTHER'
                shapes[(venue, side, cls, bool(n_inner), wsol_moves > 0)] += 1
                if len(examples) < max_cases:
                    det = []
                    for ai in accts[:14]:
                        if ai >= len(pre_sol):
                            continue
                        ow = keys[ai]
                        sd = (post_sol[ai] - pre_sol[ai]) + (
                            tamt(post_tok, ow, WSOL) - tamt(pre_tok, ow, WSOL))
                        tds = {m: (tamt(post_tok, ow, m) - tamt(pre_tok, ow, m))
                               for m in mints[:3]}
                        if sd or any(tds.values()):
                            det.append({'i': ai, 'acct': ow[:8], 'sd': sd,
                                        'tok': {k[:6]: v for k, v in tds.items() if v}})
                    examples.append({'venue': venue, 'side': side, 'class': cls,
                                     'n_accts': len(accts), 'n_inner': n_inner,
                                     'n_outer_progs': len(outer_progs),
                                     'n_wsol_movers': wsol_moves,
                                     'mints': [m[:8] for m in mints[:3]],
                                     'detail': det[:8]})

    print('rejected-swap classes:')
    for k, c in shapes.most_common(20):
        print(f'  {k}  ->  {c:,}')
    print('\nexamples:')
    for ex in examples:
        print(json.dumps(ex, indent=2))
        print('-' * 55)


if __name__ == '__main__':
    main(sys.argv[1:])
