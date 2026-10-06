#!/usr/bin/env python
"""Inject LIVE FLOW STATE into the v7 wall-eval file.

rl_wall_eval.raw.jsonl -> rl_wall_eval.jsonl. Wall prompts carry no MINT: line;
mint comes from the record (top-level or meta). t_dec_ms is parsed from the user
prompt, falling back to meta.decision_time_unix_ms. Enrichment source is
C11_FLOW_ENRICHMENT_WALL.jsonl (18,053 wall clocks) plus the corpus enrichment
file as fallback. Placement mirrors inject_c11_flow.py: after DEV HISTORY if
present, else before CURVE STATE, else before the final instruction line.
Refuses (exit 1) if any row cannot be enriched.
"""
import json
import re
import sys

SRC = '/training/v2/rl_targets_v7/rl_wall_eval.raw.jsonl'
DST = '/training/v2/rl_targets_v7/rl_wall_eval.jsonl'
RE_T = re.compile(r't_dec_ms=(\d+)')

flow = {}
for p in ('/training/v2/reports/C11_FLOW_ENRICHMENT_WALL.jsonl',
          '/training/v2/reports/C11_FLOW_ENRICHMENT.jsonl'):
    for line in open(p):
        r = json.loads(line)
        flow.setdefault((r['mint'], r['t_dec_ms']), r)

FIELDS = ['entrants_60s', 'entrants_300s', 'net_flow_sol_300s',
          'fresh_wallet_share_300s', 'flow_lookback_d', 'sniper_share_300s', 'bot_uniform_share_300s',
          'smart_entrants_300s', 'smart_net_flow_sol_300s', 'coentry_wallets_300s',
          'creator_trading_own_mint', 'entrant_fee_p90_lamports', 'entrant_cu_p50']


def block(r):
    parts = []
    for f in FIELDS:
        v = r.get(f)
        if isinstance(v, float):
            v = round(v, 6)
        parts.append(f"{f}={v}")
    return "LIVE FLOW STATE (tape, strictly pre-decision): " + "  ".join(parts)


stats = {'rows': 0, 'enriched': 0, 'no_flow': 0, 'placed_dev': 0,
         'placed_curve': 0, 'placed_tail': 0}
with open(SRC) as fh, open(DST, 'w') as out:
    for line in fh:
        rec = json.loads(line)
        stats['rows'] += 1
        meta = rec.get('meta') or {}
        mint = rec.get('mint') or meta.get('mint')
        msgs = rec.get('messages') or []
        ui = next((i for i, x in enumerate(msgs) if x.get('role') == 'user'), None)
        u = msgs[ui]['content'] if ui is not None else ''
        m = RE_T.search(u)
        t = int(m.group(1)) if m else meta.get('decision_time_unix_ms')
        fr = flow.get((mint, int(t))) if (mint and t is not None and ui is not None) else None
        if fr is None:
            stats['no_flow'] += 1
            continue
        b = block(fr)
        lines = u.split('\n')
        idx = next((i for i, l in enumerate(lines) if l.startswith('DEV HISTORY')), None)
        if idx is not None:
            lines.insert(idx + 1, b)
            stats['placed_dev'] += 1
        else:
            idx = next((i for i, l in enumerate(lines)
                        if l.startswith('CURVE STATE')), None)
            if idx is not None:
                lines.insert(idx, b)
                stats['placed_curve'] += 1
            else:
                idx = next((i for i, l in enumerate(lines)
                            if l.startswith('Choose exactly one action')), len(lines))
                lines.insert(idx, b)
                stats['placed_tail'] += 1
        msgs[ui]['content'] = '\n'.join(lines)
        (rec.setdefault('meta', {}))['flow_enrichment'] = 'c11_tape_asof_v1'
        out.write(json.dumps(rec, ensure_ascii=False) + '\n')
        stats['enriched'] += 1

print(json.dumps(stats, indent=1))
sys.exit(0 if stats['no_flow'] == 0 else 1)
