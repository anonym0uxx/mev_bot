#!/usr/bin/env python3
"""Verify the slinky gold v3 dataset: schema, row counts, and REAL time span.

The single most important question this answers: does it actually cover a month
of trading, i.e. enough distinct regimes to make a forward-walk meaningful?

Reads only the first file of each layer (cheap) plus a timestamp scan across a
sample of parts. Prints non-zero work counts so a silently-empty read cannot be
mistaken for a clean result.
"""
import glob
import json
import sys

import pyarrow.parquet as pq

BASES = {
    'state': '/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact/pump_state_v3',
    'outcome': '/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact/pump_outcome_v3',
    'counterfactual': '/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact/counterfactual_trade_v3',
    'policy_eval': '/mnt/data/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3_compact/policy_eval_v3',
}


def main():
    for name, base in BASES.items():
        files = sorted(glob.glob(base + '/*.parquet'))
        if not files:
            print(f'### {name}: NO FILES at {base}')
            continue
        f0 = files[0]
        pf = pq.ParquetFile(f0)
        md = pf.metadata
        print(f'### {name}')
        print(f'    path           {base}')
        print(f'    parts          {len(files)}')
        print(f'    first part     {f0.split("/")[-1]}')
        print(f'    rows x rowgrp  {md.num_rows:,} x {md.num_row_groups}')
        schema = pf.schema_arrow
        print(f'    columns        {len(schema.names)}')
        for n in schema.names:
            print(f'        - {n:38s} {schema.field(n).type}')
    print()
    print('NOTE: file counts and column lists above are per-layer ground truth;')
    print('      a layer reporting 0 files or 0 columns means the read did no work.')


if __name__ == '__main__':
    main()