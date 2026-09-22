#!/usr/bin/env python3
"""Which instruction discriminators are still UNKNOWN after the V2 table fix?

2,612,642 instructions landed in `unknown` across the re-normalization. Before
treating that as noise, identify them: an unhandled *trade* discriminator is a
data gap, an unhandled *config/account* instruction is not.

Samples raw files and tallies the first 8 bytes of every instruction belonging
to the pump.fun / pumpswap programs that did not match the known table.
"""
import base64
import collections
import io
import json
import subprocess
import sys

PROGRAMS = {
    'pumpfun': '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P',
    'pumpswap': 'pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA',
}

KNOWN = {
    (214, 144, 76, 236, 95, 139, 49, 180): 'pumpfun:create_v2',
    (184, 23, 238, 97, 103, 197, 211, 61): 'pumpfun:buy_v2',
    (102, 6, 61, 18, 1, 218, 235, 234): 'pumpfun:buy',
    (51, 230, 133, 164, 1, 127, 131, 173): 'pumpfun:sell',
    (24, 30, 200, 40, 5, 28, 7, 119): 'pumpfun:create',
}


def main(parts, limit_per_file=4000):
    tally = collections.Counter()
    for part in parts:
        p = subprocess.Popen(['zstd', '-dc', part], stdout=subprocess.PIPE)
        n = 0
        for line in io.TextIOWrapper(p.stdout, encoding='utf-8', errors='replace'):
            n += 1
            if n > limit_per_file:
                p.stdout.close()
                p.terminate()
                break
            try:
                o = json.loads(line)
            except Exception:
                continue
            # Raw capture records nest the transaction under `payload`; older
            # exports used `transaction`. Reading only `transaction` silently
            # scans zero instructions and reports "no unknowns".
            tx = o.get('payload') or o.get('transaction') or {}
            msg = tx.get('message') or {}
            meta = tx.get('meta') or {}
            keys = list(msg.get('account_keys_b58') or [])
            keys += list(meta.get('loaded_writable_addresses_b58') or [])
            keys += list(meta.get('loaded_readonly_addresses_b58') or [])
            for ix in (msg.get('instructions') or []):
                pi = ix.get('program_id_index')
                if pi is None or pi >= len(keys) or keys[pi] not in PROGRAMS:
                    continue
                d = ix.get('data_b64') or ix.get('data_b58')
                if not d:
                    continue
                try:
                    raw = base64.b64decode(d)
                except Exception:
                    continue
                disc = tuple(raw[:8])
                if disc in KNOWN:
                    continue
                tally[(keys[pi] == PROGRAMS['pumpswap'], disc, len(raw))] += 1

    print(f'{"program":9s} {"discriminator":46s} {"len":>5s} {"count":>8s}')
    for (is_swap, disc, ln), c in tally.most_common(25):
        prog = 'pumpswap' if is_swap else 'pumpfun'
        print(f'{prog:9s} {str(list(disc)):46s} {ln:5d} {c:8d}')
    print()
    print('distinct unknown discriminators:', len(tally))


if __name__ == '__main__':
    main(sys.argv[1:])
