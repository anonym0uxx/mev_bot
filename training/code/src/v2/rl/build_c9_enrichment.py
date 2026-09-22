"""c9 enrichment — the NEW per-candidate fields, computed causally from the tape.

Scope: the fields the corpus lacks (verified absent): holders, holder concentration, bundle
structure, flow quality, and creator history. mcap uses the canonical Rust formula
`vsol^2 / MCAP_DIVISOR_LAMPORTS` (32_190_000_000) so training and production agree.

Corrections carried over from the drawdown exercise:
  * guard every input (no negative prices, no zero divisors);
  * realistic magnitudes only -- a holder share is in [0,1], a count is > 0.

A bounded sample is run first; the full build follows only once the sample is validated.
"""
import json
import collections
import os

CORPUS = '/training/v2/candidate_sft_c8/train.jsonl'
TAPE = '/training/v2/canonical/renormalized_v7/trades.jsonl'
LAUNCHES = '/training/v2/canonical/renormalized_v7/launches.jsonl'
OUT = '/training/v2/reports/C9_ENRICHMENT_SAMPLE.jsonl'
STATS = '/training/v2/reports/C9_ENRICHMENT_SAMPLE_STATS.json'

MCAP_DIVISOR = 32_190_000_000
N_MINTS = 300
LAMPORTS = 1_000_000_000

# ---- 1. sample decisions + their curve state (already carried on the corpus row)
dec = collections.defaultdict(list)                 # mint -> [(t_dec, mcap_lamports)]
with open(CORPUS, encoding='utf-8') as fh:
    for line in fh:
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
        vsol = ((m.get('curve_reserves') or {}).get('virtual_sol_reserves_lamports'))
        cr = m.get('curve_reserves') or {}
        ar = m.get('amm_reserves') or {}
        # mcap: price-per-WHOLE-token x fixed 1e9 supply. The curve formula vsol^2/divisor
        # SATURATES at the graduated value (410.88 SOL) and stops measuring anything once the
        # curve completes, so prefer the AMM price on a graduated token.
        graduated = (cr.get('curve_regime') == 'graduated') or (cr.get('curve_progress') == 1.0)
        pw = ar.get('amm_price_sol_per_whole_token') if graduated else None
        src = 'amm'
        if pw is None:
            pw = cr.get('curve_price_sol_per_whole_token')
            src = 'curve'
        if pw is None and vsol and int(vsol) > 0:
            pw = (int(vsol) ** 2 // MCAP_DIVISOR) / LAMPORTS / 1e9
            src = 'curve_formula'
        mcap = (float(pw) * 1e9) if pw else None
        dec[mint].append((int(parts[2]), mcap, src))
        if len(dec) >= N_MINTS:
            break

mints = sorted(dec)
mset = set(mints)
n_dec = sum(len(v) for v in dec.values())
print(f"sample: mints={len(mints)} decisions={n_dec}", flush=True)

# ---- 2. creator history (causal, from launches.jsonl)
creator_of = {}
launch_time = {}
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


def prior_launches(creator, mint):
    """Launches by this creator STRICTLY BEFORE this mint's own launch.

    Counting up to the decision time is wrong: the mint's own launch precedes the decision, so
    it would be counted as its own history (off-by-one, and it made every mint look like a
    repeat launcher).
    """
    ts = by_creator.get(creator)
    if not ts:
        return 0
    own = launch_time.get(mint)
    if own is None:
        return 0
    lo, hi = 0, len(ts)
    while lo < hi:
        mid = (lo + hi) // 2
        if ts[mid] < own:
            lo = mid + 1
        else:
            hi = mid
    return lo


# ---- 3. one tape pass for the sampled mints
events = collections.defaultdict(list)              # mint -> [(t, trader, side, tok, sol, slot)]
rows = 0
with open(TAPE, encoding='utf-8') as fh:
    for line in fh:
        rows += 1
        if rows % 3_000_000 == 0:
            print(f"  scanned {rows:,}", flush=True)
        if '"mint"' not in line:
            continue
        try:
            r = json.loads(line)
        except Exception:
            continue
        mint = r.get('mint')
        if mint not in mset:
            continue
        t = r.get('recv_unix_ms')
        tok = r.get('tokens_raw')
        sol = r.get('sol_lamports')
        trader = r.get('trader')
        if not t or not trader or tok is None or sol is None:
            continue
        events[mint].append((int(t), trader, str(r.get('side') or ''),
                             int(tok), int(sol), r.get('slot')))
print(f"tape rows scanned: {rows:,}; mints with events: {len(events)}", flush=True)
for m in events:
    events[m].sort()

# ---- 4. per-decision causal snapshot
out = []
bad = 0
for mint in mints:
    ev = events.get(mint) or []
    if len(ev) < 3:
        continue
    creator = creator_of.get(mint)
    for (t_dec, mcap, msrc) in dec[mint]:
        cutoff = [e for e in ev if e[0] <= t_dec]
        if len(cutoff) < 2:
            continue
        # net token balance per wallet -> holders
        bal = collections.Counter()
        buys = sells = 0
        vol = 0
        slots_seen = {}
        wash_wallets = collections.Counter()
        for (t, trader, side, tok, sol, slot) in cutoff:
            bal[trader] += tok
            vol += abs(sol)
            if side == 'buy':
                buys += 1
            else:
                sells += 1
            if slot is not None:
                slots_seen.setdefault(slot, []).append(trader)
            if side == 'buy':
                wash_wallets[trader] += 1
            elif side == 'sell':
                wash_wallets[trader] -= 1
        holders = [v for v in bal.values() if v > 0]
        n_hold = len(holders)
        tot = sum(holders) or 1
        desc = sorted(holders, reverse=True)
        top1 = desc[0] / tot if desc else 0.0
        top5 = sum(desc[:5]) / tot if desc else 0.0
        hhi = sum((x / tot) ** 2 for x in holders)
        # bundle: wallets that bought in the same slot as another buyer, early
        bundle_wallets = 0
        bundle_slots = 0
        for slot, trs in slots_seen.items():
            uniq = set(trs)
            if len(uniq) >= 2:
                bundle_slots += 1
                bundle_wallets += len(uniq)
        round_trip = sum(1 for w, n in wash_wallets.items() if n == 0)
        # sanity gates -- refuse to emit a row whose numbers are not physically possible
        if not (0.0 <= top1 <= 1.0 and 0.0 <= top5 <= 1.0 and 0.0 <= hhi <= 1.0):
            bad += 1
            continue
        out.append({
            'mint': mint, 't_dec_ms': t_dec,
            'mcap_sol_at_t': round(mcap, 6) if mcap else None,
            'mcap_source': msrc,
            'holders_at_t': n_hold,
            'top1_holder_share': round(top1, 6),
            'top5_holder_share': round(top5, 6),
            'holder_hhi': round(hhi, 6),
            'bundle_slots': bundle_slots,
            'bundle_wallets': bundle_wallets,
            'n_buys_at_t': buys, 'n_sells_at_t': sells,
            'volume_sol_at_t': round(vol / LAMPORTS, 6),
            'round_trip_wallets': round_trip,
            'wash_ratio': round(round_trip / max(1, len(bal)), 6),
            'creator_past_launches': prior_launches(creator, mint) if creator else None,
            'creator_known': bool(creator),
        })

with open(OUT, 'w', encoding='utf-8') as fh:
    for r in out:
        fh.write(json.dumps(r) + '\n')

# ---- 5. validate the sample
def q(vals, f):
    v = sorted(vals)
    return v[min(len(v) - 1, int(f * len(v)))] if v else None

hold = [r['holders_at_t'] for r in out]
t1 = [r['top1_holder_share'] for r in out]
hh = [r['holder_hhi'] for r in out]
cp = [r['creator_past_launches'] for r in out if r['creator_past_launches'] is not None]
stats = {
    'rows_out': len(out), 'rows_rejected_insane': bad,
    'mcap_sol': {'p10': q([r['mcap_sol_at_t'] for r in out if r['mcap_sol_at_t']], .10),
                 'p50': q([r['mcap_sol_at_t'] for r in out if r['mcap_sol_at_t']], .50),
                 'p90': q([r['mcap_sol_at_t'] for r in out if r['mcap_sol_at_t']], .90)},
    'holders_at_t': {'p10': q(hold, .10), 'p50': q(hold, .50), 'p90': q(hold, .90), 'max': max(hold) if hold else None},
    'top1_holder_share': {'p50': q(t1, .50), 'p90': q(t1, .90)},
    'holder_hhi': {'p50': q(hh, .50), 'p90': q(hh, .90)},
    'creator_past_launches': {'n_known': len(cp), 'p50': q(cp, .50), 'max': max(cp) if cp else None,
                              'frac_with_prior': round(sum(1 for x in cp if x > 0) / max(1, len(cp)), 4)},
    'cache': {'tape_rows': rows, 'source_tape': TAPE},
}
json.dump(stats, open(STATS, 'w', encoding='utf-8'), indent=1)
print(json.dumps(stats, indent=1))
print("wrote", OUT, STATS)
