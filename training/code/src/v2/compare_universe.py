#!/usr/bin/env python3
"""Does the frozen slinky dataset cover the same mint universe as my capture?

Decides whether the capture-derived corpus is still needed, or whether slinky
supersedes it entirely. Prints non-zero-work assertions so an empty read cannot
masquerade as a clean answer.
"""
import glob
import json

import pyarrow.compute as pc
import pyarrow.parquet as pq

S = '/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact/pump_state_v3'
LAUNCHES = '/training/v2/canonical/renormalized_v6/launches.jsonl'


def main():
    files = sorted(glob.glob(S + '/*.parquet'))
    slinky_mints = set()
    scans = 0
    for f in files[:20]:
        t = pq.read_table(f, columns=['mint'])
        slinky_mints.update(pc.unique(t.column('mint')).to_pylist())
        scans += 1
    print(f'slinky parts scanned : {scans}')
    print(f'slinky distinct mints: {len(slinky_mints):,}')
    if not slinky_mints:
        print('FAIL: read zero mints -- result not trustworthy')
        return

    capture_mints = set()
    with open(LAUNCHES) as fh:
        for line in fh:
            try:
                capture_mints.add(json.loads(line)['mint'])
            except Exception:
                pass
    print(f'capture v6 launches  : {len(capture_mints):,}')
    if not capture_mints:
        print('FAIL: capture mint set is empty')
        return

    both = slinky_mints & capture_mints
    print(f'INTERSECTION         : {len(both):,}')
    print(f'slinky-only          : {len(slinky_mints - capture_mints):,}')
    print(f'capture-only         : {len(capture_mints - slinky_mints):,}')
    print()
    print(f'capture mints covered by slinky: '
          f'{100.0 * len(both) / len(capture_mints):.2f}%')


if __name__ == '__main__':
    main()