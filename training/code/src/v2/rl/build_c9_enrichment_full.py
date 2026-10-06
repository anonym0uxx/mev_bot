"""c9 FULL enrichment — every decision row, all three splits.

Same causal derivation as the validated sample (`build_c9_enrichment.py`), with the three fixes
locked in:
  * mcap uses the AMM price once the curve is complete (the curve formula saturates at 410.88 SOL)
  * creator history counts strictly before the mint's OWN launch
  * holder shares are FLOAT shares (curve inventory is not in the denominator)

Output is keyed by (split, row_index) so the assembler can join it back onto the corpus rows
without relying on prompt text matching.
"""
import json
import collections
import os

BASE = '/training/v2/candidate_sft_c8'
SPLITS = {'train': 'train.jsonl', 'validation': 'validation.jsonl', 'examination': 'examination.jsonl'}
TAPE = '/training/v2/canonical/renormalized_v7/trades.jsonl'
LAUNCHES = '/training/v2/canonical/renormalized_v7/launches.jsonl'
OUT = '/training/v2/reports/C9_ENRICHMENT_FULL.jsonl'

MCAP_DIVISOR = 32_190_000_000
LAMPORTS = 1_000_000_000

# ---- 1. every decision row, per split, with its curve/amm state
rows = []                                   # (split, idx, mint, t_dec, mcap_sol, mcap_src)
mints = set()
for split, fn in SPLITS.items():
    p = os.path.join(BASE, fn)
    if not os.path.isfile(p):
        continue
    with open(p, encoding='utf-8') as fh:
        for idx, line in enumerate(fh):
            r = json.loads(line)
            m = r.get('meta') or {}
            if m.get('task') != 'decision_action':
                continue
            mint = m.get('mint')
            sa = (m.get('source_associations') or [])
            if not mint or not sa:
                continue
            parts = ((sa[0] or {}).get('source') or {}).get('row', '').split(':')
            if len(parts) != 3 or not parts[2].isdigit():
                continue
            cr = m.get('curve_reserves') or {}
            ar = m.get('amm_reserves') or {}
            graduated = (cr.get('curve_regime') == 'graduated') or (cr.get('curve_progress') == 1.0)
            pw, src = None, 'absent'
            if graduated and ar.get('amm_price_sol_per_whole_token'):
                pw, src = ar['amm_price_sol_per_whole_token'], 'amm'
            elif cr.get('curve_price_sol_per_whole_token'):
                pw, src = cr['curve_price_sol_per_whole_token'], 'curve'
            mcap = (float(pw) * 1e9) if pw else None
            rows.append((split, idx, mint, int(parts[2]), mcap, src))
            mints.add(mint)

print(f"decision rows: {len(rows):,} | distinct mints: {len(mints):,}", flush=True)

# ---- 2. creator history
creator_of, launch_time = {}, {}
by_creator = collections.defaultdict(list)
with open(LAUNCHES, encoding='utf-8') as fh:
    for line in fh:
        try:
            r = json.loads(line)
        except Exception:
            continue
        c, mi, t = r.get('creator'), r.get('mint'), r.get('recv_unix_ms')
        if c and mi and t:
            creator_of[mi] = c
            launch_time[mi] = int(t)
            by_creator[c].append(int(t))
for c in by_creator:
    by_creator[c].sort()


def prior_launches(mint):
    c = creator_of.get(mint)
    if not c or mint not in launch_time:
        return None
    ts = by_creator.get(c) or []
    own = launch_time[mint]
    lo, hi = 0, len(ts)
    while lo < hi:
        mid = (lo + hi) // 2
        if ts[mid] < own:
            lo = mid + 1
        else:
            hi = mid
    return lo


# ---- 3. one tape pass
events = collections.defaultdict(list)
scanned = 0
with open(TAPE, encoding='utf-8') as fh:
    for line in fh:
        scanned += 1
        if scanned % 4_000_000 == 0:
            print(f"  scanned {scanned:,}", flush=True)
        if '"mint"' not in line:
            continue
        try:
            r = json.loads(line)
        except Exception:
            continue
        mint = r.get('mint')
        if mint not in mints:
            continue
        t, trader = r.get('recv_unix_ms'), r.get('trader')
        tok, sol = r.get('tokens_raw'), r.get('sol_lamports')
        if not t or not trader or tok is None or sol is None:
            continue
        events[mint].append((int(t), trader, str(r.get('side') or ''), int(tok), int(sol), r.get('slot')))
print(f"tape scanned: {scanned:,}; mints with events: {len(events):,}", flush=True)
for m in events:
    events[m].sort()

# ---- 4. snapshot per decision
n_out = 0
n_bad = 0
with open(OUT, 'w', encoding='utf-8') as out:
    for (split, idx, mint, t_dec, mcap, msrc) in rows:
        ev = events.get(mint) or []
        if len(ev) < 2:
            continue
        cutoff = [e for e in ev if e[0] <= t_dec]
        if len(cutoff) < 2:
            continue
        bal = collections.Counter()
        buys = sells = vol = 0
        slots = collections.defaultdict(set)
        rt = collections.Counter()
        for (t, trader, side, tok, sol, slot) in cutoff:
            bal[trader] += tok
            vol += abs(sol)
            if side == 'buy':
                buys += 1
                rt[trader] += 1
            else:
                sells += 1
                rt[trader] -= 1
            if slot is not None:
                slots[slot].add(trader)
        hs = [v for v in bal.values() if v > 0]
        tot = sum(hs) or 1
        desc = sorted(hs, reverse=True)
        top1 = desc[0] / tot if desc else 0.0
        top5 = sum(desc[:5]) / tot if desc else 0.0
        hhi = sum((x / tot) ** 2 for x in hs)
        bundle_slots = sum(1 for s, trs in slots.items() if len(trs) >= 2)
        bundle_wallets = sum(len(trs) for trs in slots.values() if len(trs) >= 2)
        if not (0.0 <= top1 <= 1.0 and 0.0 <= top5 <= 1.0 and 0.0 <= hhi <= 1.0):
            n_bad += 1
            continue
        rec = {
            'split': split, 'row_index': idx, 'mint': mint, 't_dec_ms': t_dec,
            'mcap_sol_at_t': round(mcap, 6) if mcap else None, 'mcap_source': msrc,
            'holders_at_t': len(hs),
            'top1_float_share': round(top1, 6), 'top5_float_share': round(top5, 6),
            'holder_hhi': round(hhi, 6),
            'bundle_slots': bundle_slots, 'bundle_wallets': bundle_wallets,
            'n_buys_at_t': buys, 'n_sells_at_t': sells,
            'volume_sol_at_t': round(vol / LAMPORTS, 6),
            'round_trip_wallets': sum(1 for w, n in rt.items() if n == 0),
            'wash_ratio': round(sum(1 for w, n in rt.items() if n == 0) / max(1, len(bal)), 6),
            'creator_past_launches': prior_launches(mint),
            'creator_known': mint in creator_of,
        }
        out.write(json.dumps(rec) + '\n')
        n_out += 1

print(json.dumps({
    'decision_rows_in': len(rows), 'rows_out': n_out, 'rows_rejected': n_bad,
    'mints': len(mints), 'out': OUT,
}, indent=1))
