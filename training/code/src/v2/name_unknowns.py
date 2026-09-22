#!/usr/bin/env python3
"""Name the unknown pump/pumpswap discriminators by sha256-matching candidates.

Anchor-program discriminators are sha256('global:<name>')[:8]. So for any
unrecognized 8-byte discriminator actually present in the stream, test it
against a candidate name list and report the count. The point is to answer
"is a TRADE variant hiding in the unknown bucket?" -- if the unknowns are all
liquidity/config/admin instructions, dropping them costs no trades. If a
buy/sell variant shows up, it must be added to the table.

Only prints candidates that actually matched something.
"""
import base64
import hashlib
import io
import json
import subprocess
import sys
from collections import Counter

PUMP = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
SWAP = 'pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA'

CANDIDATES = [
    # pump.fun bonding curve
    'buy', 'sell', 'create', 'create_v2', 'buy_v2', 'sell_v2',
    'buy_exact_sol_in', 'buy_exact_quote_in',
    'migrate', 'migrate_v2', 'complete', 'complete_v2',
    'extend_account', 'transfer_creator_fees', 'distribute_creator_fees',
    'collect_creator_fee', 'set_creator', 'set_metaplex_creator',
    'initialize', 'initialize_v2', 'set_params', 'set_global_authority',
    'update_global_authority', 'withdraw', 'deposit', 'close_account',
    'transfer_creator_fees_v2', 'create_fee_sharing_config',
    # pumpswap AMM
    'create_pool', 'add_liquidity', 'remove_liquidity',
    'deposit', 'withdraw', 'create_config', 'update_fee_config',
    'set_coin_creator', 'sync_coin_creator', 'create_coin_creator',
    'buy_exact_quote_in', 'buy_exact_sol_in',
    'sell_exact_quote_in', 'sell_exact_sol_in',
    'collect_coin_creator_fee', 'transfer_creator_fees',
    'close_pool', 'pause', 'unpause', 'set_admin',
]


def main(parts, per_file=1500):
    seen = Counter()
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
            if not meta.get('err_is_none', True):
                continue
            inner = []
            for grp in (meta.get('inner_instructions') or []):
                inner.extend(grp.get('instructions') or [])
            for ix in list(msg.get('instructions') or []) + inner:
                pi = ix.get('program_id_index')
                d = ix.get('data_b64')
                if pi is None or d is None or pi >= len(keys):
                    continue
                prog = keys[pi]
                if prog not in (PUMP, SWAP):
                    continue
                try:
                    raw = base64.b64decode(d)
                except Exception:
                    continue
                if len(raw) < 8:
                    continue
                seen[(prog, raw[:8])] += 1

    # build reverse map: discriminator -> candidate names
    by_disc = {}
    for name in CANDIDATES:
        d = hashlib.sha256(('global:' + name).encode()).digest()[:8]
        by_disc.setdefault(d, []).append(name)

    print(f'distinct (program, discriminator) pairs seen: {len(seen):,}')
    print(f'total instructions seen                     : {sum(seen.values()):,}\n')

    matched = 0
    unmatched = []
    for (prog, disc), cnt in seen.most_common():
        names = by_disc.get(disc)
        if names:
            matched += cnt
            tag = 'pumpfun' if prog == PUMP else 'pumpswap'
            print(f'  {tag:9s} {disc.hex():16s} {cnt:9,d}  -> {"/".join(names)}')
        else:
            unmatched.append(((prog, disc), cnt))

    print(f'\ninstructions attributable to a named candidate: {matched:,}')
    print(f'distinct unmatched discriminators: {len(unmatched)}')
    print('top unmatched (still unidentified):')
    for (prog, disc), cnt in sorted(unmatched, key=lambda kv: -kv[1])[:15]:
        tag = 'pumpfun' if prog == PUMP else 'pumpswap'
        print(f'  {tag:9s} {disc.hex():16s} {cnt:9,d}')


if __name__ == '__main__':
    main(sys.argv[1:])
