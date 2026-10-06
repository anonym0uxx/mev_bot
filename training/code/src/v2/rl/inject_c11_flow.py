#!/usr/bin/env python
"""C11 assembly — inject LIVE FLOW STATE into every c10 row (all 4 families).

USER message: insert one `LIVE FLOW STATE: k=v ...` line after the DEV HISTORY
line (fallbacks: after ENRICHED CANDIDATE STATE; before the 'Choose exactly one
action' line; end). ASSISTANT message: append one `FLOW CITATION: k=v ...` line
(B5 rule — a field the labels never cite is a field the model ignores).

Labels, actions, numbers, meta: byte-untouched. Built-in gate 18: stripping the
two injected lines must reproduce the c10 row byte-identically.

Spec: /training/v2/reports/C11_SYNTHESIS_SPEC.md
"""
import json
import collections
import os
import re
import sys

import argparse

ap = argparse.ArgumentParser()
ap.add_argument('--src', default='/training/v2/candidate_sft_c10')
ap.add_argument('--dst', default='/training/v2/candidate_sft_c11')
_a = ap.parse_args()
SRC = _a.src
DST = _a.dst
ENRICH = '/training/v2/reports/C11_FLOW_ENRICHMENT.jsonl'
RE_TDEC = re.compile(r"t_dec_ms=(\d+)")
RE_MINT = re.compile(r"^MINT:\s*(\S+)", re.M)

FIELDS = ("entrants_60s", "entrants_300s", "net_flow_sol_300s",
          "fresh_wallet_share_300s", "flow_lookback_d", "sniper_share_300s", "bot_uniform_share_300s",
          "smart_entrants_300s", "smart_net_flow_sol_300s", "coentry_wallets_300s",
          "creator_trading_own_mint", "entrant_fee_p90_lamports", "entrant_cu_p50")


def load_index():
    idx = {}
    with open(ENRICH) as fh:
        for line in fh:
            r = json.loads(line)
            idx[(r['mint'], int(r['t_dec_ms']))] = r
    return idx


def key_of(r):
    m = r.get('meta') or {}
    mint = r.get('mint') or m.get('mint')
    t = m.get('decision_time_unix_ms')
    u = r['messages'][1]['content']
    if t is None:
        mm = RE_TDEC.search(u)
        t = int(mm.group(1)) if mm else None
    if mint is None:
        mm = RE_MINT.search(u)
        mint = mm.group(1) if mm else None
    return (mint, int(t)) if mint and t is not None else None


def render(e):
    if e.get('no_prior_flow'):
        return "LIVE FLOW STATE: no_prior_flow=true"
    parts = []
    for k in FIELDS:
        v = e.get(k)
        parts.append(f"{k}={'None' if v is None else v}")
    return "LIVE FLOW STATE: " + " ".join(parts)


def cite(e):
    if e.get('no_prior_flow'):
        return "FLOW CITATION: no_prior_flow=true"
    parts = [f"{k}={'None' if e.get(k) is None else e.get(k)}" for k in FIELDS]
    return "FLOW CITATION: " + " ".join(parts)


def inject_user(u, block, stats):
    lines = u.split('\n')
    for pat, tag in (('DEV HISTORY: ', 'after_dev_history'),
                     ('ENRICHED CANDIDATE STATE: ', 'after_enriched')):
        for i, ln in enumerate(lines):
            if ln.startswith(pat):
                stats[tag] += 1
                return '\n'.join(lines[:i + 1] + [block] + lines[i + 1:])
    for i, ln in enumerate(lines):
        if ln.startswith('Choose exactly one action'):
            stats['before_choose'] += 1
            return '\n'.join(lines[:i] + [block] + lines[i:])
    stats['appended_end'] += 1
    return u + '\n' + block


def main():
    idx = load_index()
    os.makedirs(DST, exist_ok=True)
    stats = collections.Counter()
    fam_cited = collections.Counter()
    for fn in ('train.jsonl', 'validation.jsonl', 'examination.jsonl'):
        src, dst = os.path.join(SRC, fn), os.path.join(DST, fn)
        with open(src) as fi, open(dst, 'w') as fo:
            for line in fi:
                raw = line.rstrip('\n')
                r = json.loads(raw)
                fam = (r.get('meta') or {}).get('family') or '?'
                k = key_of(r)
                e = idx.get(k) if k else None
                if e is None:
                    print(f"FATAL: no flow enrichment for {k} in {fn}", file=sys.stderr)
                    sys.exit(2)
                block, cline = render(e), cite(e)
                u0 = r['messages'][1]['content']
                a0 = r['messages'][2]['content']
                u1 = inject_user(u0, block, stats)
                a1 = a0 + '\n' + cline
                # gate 18: strict reversibility
                if u1.replace('\n' + block, '', 1).replace(block + '\n', '', 1) != u0 \
                        or a1[: -len(cline) - 1] != a0:
                    print(f"FATAL: reversibility broken in {fn}", file=sys.stderr)
                    sys.exit(3)
                r['messages'][1]['content'] = u1
                r['messages'][2]['content'] = a1
                fo.write(json.dumps(r, ensure_ascii=False) + '\n')
                stats[f'rows_{fn}'] += 1
                fam_cited[fam] += 1
    # carry non-jsonl artifacts
    for extra in os.listdir(SRC):
        if not extra.endswith('.jsonl'):
            s = os.path.join(SRC, extra)
            if os.path.isfile(s):
                with open(s, 'rb') as fi, open(os.path.join(DST, extra), 'wb') as fo:
                    fo.write(fi.read())
                stats[f'copied_{extra}'] += 1
    print(json.dumps({"stats": dict(stats), "flow_cited_per_family": dict(fam_cited)},
                     indent=1))


if __name__ == '__main__':
    main()
