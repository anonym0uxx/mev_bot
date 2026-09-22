#!/usr/bin/env python3
"""Generate byte-parity fixtures for the Rust proposal renderer.

For every corpus row we recover the typed inputs (and, for the SIZE OPTIONS block,
the pool depth the cost authority must have been given) and record the exact corpus
user prompt as the expectation. The Rust test re-renders and must match byte-for-byte.
"""
import json
import random
import sys

sys.path.insert(0, '/tmp')
import p1_ref2 as R

# These fixtures reproduce the TRAINED corpus's prompts (<= c12), whose SIZE OPTIONS block
# still offered MID. `cost_authority.size_options` now defaults to the RULED two-tier menu, so
# the legacy menu is pinned explicitly here: this generator must follow the corpus it serves,
# and it moves to the ruled default when c13 (the rebuilt prompt) is the corpus in hand.
CLIPS = (("SMALL", 0.25), ("MID", 0.50), ("FULL", 1.00))


def bps3(regime, depth):
    # `p1_ref2.size_options` is this directory's own reference reimplementation and its default
    # IS the legacy three-tier menu, so it keeps reproducing the trained corpus (<= c12).
    # `cost_authority.size_options` — the row builder's authority — now defaults to the RULED
    # two-tier menu, which is what c13 will render.
    d = R.size_options(regime, depth)["sizes"]
    return [d["SMALL"]["round_trip_bp"], d["MID"]["round_trip_bp"], d["FULL"]["round_trip_bp"]]


def fixed_bp(clip):
    return R.FIXED_LAMPORTS_PER_LEG_P50 / (clip * R.LAMPORTS_PER_SOL) * R.BPS_ONE


def find_depth(regime, want, printed=None):
    """Recover the pool depth the corpus SIZE OPTIONS block was computed from.

    total_k = round_half_even(2*(venue_bp + fixed_tx_bp(clip_k)) + 2*impact_k) with
    impact_k = round_half_even(clip_k/depth*1e4). Invert each tier for its impact,
    intersect the three admissible depth intervals (and the interval implied by the
    1-dp depth the block prints), then verify by re-rendering.
    """
    venue = R.BONDING_FEE_BPS_MEASURED if regime == 'bonding_curve' else R.AMM_VENUE_FEE_BPS_PER_LEG
    import itertools
    cands = []
    for k, (_name, clip) in enumerate(CLIPS):
        est = int(R.py_round_half_even((want[k] - 2.0 * (venue + fixed_bp(clip))) / 2.0, 0))
        cands.append([i for i in range(est - 2, est + 3) if i >= 0])
    for i_s, i_m, i_f in itertools.product(*cands):
        lo, hi = 0.0, 1e12
        if printed is not None:
            lo, hi = max(lo, printed - 0.05), min(hi, printed + 0.05)
        ok = True
        for i, (_name, clip) in zip((i_s, i_m, i_f), CLIPS):
            a = (clip * R.BPS_ONE) / (i + 0.5)              # exclusive lower bound
            b = (clip * R.BPS_ONE) / (i - 0.5) if i > 0 else 1e12   # inclusive upper bound
            lo, hi = max(lo, a), min(hi, b)
            if lo >= hi:
                ok = False
                break
        if not ok:
            continue
        d = (lo * hi) ** 0.5
        if bps3(regime, d) == want:
            return d
    return None



def plain(v):
    """PyNum tuple -> plain JSON scalar, recursively (fixture encoding)."""
    if isinstance(v, tuple):
        kind, x = v
        return int(x) if kind == 'i' else (float(x) if kind == 'f' else bool(x))
    if isinstance(v, dict):
        return {k: plain(x) for k, x in v.items()}
    if isinstance(v, list):
        return [plain(x) for x in v]
    return v


def pynum(v):
    if v is None:
        return None
    kind, x = v
    return int(x) if kind == 'i' else float(x)


def regime_and_depth(c):
    """The SIZE OPTIONS regime/depth the corpus used (patch_entry_cost.regime_of / depth_of).

    The regime is decided by whether OUR next fill lands on a live PumpSwap pool, i.e. by
    the AMM reserve annotation, NOT by the venue label; the depth is that pool's own
    reserve in SOL. For a mint with no AMM pool the curve carries the fill.
    """
    if c['amm']['kind'] == 'present':
        return 'amm', c['amm']['quote_reserves_lamports'] / R.LAMPORTS_PER_SOL
    return 'bonding_curve', c['curve']['v_sol_reserves_lamports'] / R.LAMPORTS_PER_SOL


def resolve_size(c):
    """The (regime, depth) the corpus SIZE OPTIONS block was computed from.

    `patch_entry_cost.regime_of` reads meta.market_structure.amm.status, which is NOT
    always the same object the rendered AMM POOL STATE line describes, so the regime is
    recovered by asking which one reproduces the block. The block itself is the authority
    the renderer must match.
    """
    want = [s[2] for s in c['sizes']]
    printed = float(c['sizes'][0][3])
    amm_d = c['amm']['quote_reserves_lamports'] / R.LAMPORTS_PER_SOL if c['amm']['kind'] == 'present' else None
    cur_d = c['curve']['v_sol_reserves_lamports'] / R.LAMPORTS_PER_SOL if c['curve']['kind'] == 'present' else None
    regs = ['amm', 'bonding_curve']
    deps = {'amm': amm_d if amm_d is not None else cur_d,
            'bonding_curve': cur_d if cur_d is not None else amm_d}
    for reg in regs:
        if deps[reg] is not None and bps3(reg, deps[reg]) == want:
            return reg, deps[reg]
    for reg in regs:
        d = find_depth(reg, want, printed)
        if d is not None:
            return reg, d
    return None, None


def rust_decision(c, regime, depth):
    e = c['enriched']
    return {
        "t_dec_ms": c['t_dec_ms'], "age_s": pynum(c['age_s']),
        "last_trade_age_s": pynum(c['last_trade_age_s']),
        "venue": c['venue'], "curve_present": c['curve_present'],
        "evidence_status": c['evidence_status'],
        "n_prior_trades": c['n_prior_trades'], "buy_count": c['buy_count'],
        "sell_count": c['sell_count'], "unique_traders": c['unique_traders'],
        "price_lamports_per_raw_token": pynum(c['price_lamports_per_raw_token']),
        "ret_5s_bp": pynum(c['ret_5s_bp']), "ret_30s_bp": pynum(c['ret_30s_bp']),
        "vol_30s_bp": pynum(c['vol_30s_bp']),
        "buy_volume_lamports": c['buy_volume_lamports'],
        "sell_volume_lamports": c['sell_volume_lamports'],
        "net_flow_lamports": c['net_flow_lamports'],
        "top1_trader_share": pynum(c['top1_trader_share']),
        "top5_trader_share": pynum(c['top5_trader_share']),
        "buyer_seller_ratio": pynum(c['buyer_seller_ratio']),
        "enriched": {
            "mcap_sol_at_t": None if e['mcap_sol_at_t'] in ('na', 'None') else pynum(R.pnum_or_none(e['mcap_sol_at_t'])),
            "mcap_source": e['mcap_source'],
            "holders_at_t": int(e['holders_at_t']),
            "top1_float_share": pynum(R.pnum_or_none(e['top1_float_share'])),
            "top5_float_share": pynum(R.pnum_or_none(e['top5_float_share'])),
            "holder_hhi": pynum(R.pnum_or_none(e['holder_hhi'])),
            "bundle_slots": int(e['bundle_slots']),
            "bundle_wallets": int(e['bundle_wallets']),
            "volume_sol_at_t": pynum(R.pnum_or_none(e['volume_sol_at_t'])),
            "wash_ratio": pynum(R.pnum_or_none(e['wash_ratio'])),
        },
        "dev": {"creator_past_launches": (None if str(c['dev']['creator_past_launches']) == 'unknown' else int(c['dev']['creator_past_launches'])),
                "creator_known": int(c['dev']['creator_known'])},
        "flow": plain(c['flow']),
        "flow_no_prior": bool(c['flow']['no_prior_flow']),
        "curve": c['curve'],
        "amm": c['amm'],
        "size_depth_sol": depth,
        "size_amm": regime == 'amm',
    }

def main():
    limit = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    out = sys.argv[2] if len(sys.argv) > 2 else '/tmp/fixtures_decision.jsonl'
    rows = {}
    stats = {'total': 0, 'parsed': 0, 'rendered': 0, 'nondepth': 0, 'err': 0}
    variants = {}
    sample = random.Random(20260917)
    with open('/training/v2/candidate_sft_c11/train.jsonl') as f:
        for line in f:
            r = json.loads(line)
            if r.get('family') != 'decision':
                continue
            stats['total'] += 1
            u = r['messages'][1]['content']
            try:
                c = R.parse_decision(u)
                stats['parsed'] += 1
            except Exception as ex:
                stats['err'] += 1
                if stats['err'] < 4:
                    print('PARSE-ERR', type(ex).__name__, str(ex)[:60])
                continue
            reg, depth = resolve_size(c)
            if reg is None:
                stats['nondepth'] += 1
                if stats['nondepth'] < 4:
                    print('NODEPTH', c['venue'], depth, [s[2] for s in c['sizes']],
                          u.split('\n')[-5:])
                continue
            if R.render_decision(c, depth_override=depth, regime_override=reg) != u:
                stats['nondepth'] += 1
                if stats['nondepth'] < 4:
                    for a, b in zip(R.render_decision(c, depth, reg).split('\n'), u.split('\n')):
                        if a != b:
                            print('NONDEPTH', a[:150], '||', b[:150])
                            break
                continue
            stats['rendered'] += 1
            key = (c['venue'], c['curve']['kind'], c['amm']['kind'],
                   c['flow']['no_prior_flow'], c['evidence_status'],
                   c['buyer_seller_ratio'] is None, c['enriched']['mcap_source'])
            variants.setdefault(key, 0)
            variants[key] += 1
            if key not in rows:
                rows[key] = (rust_decision(c, reg, depth), u)   # one per variant
            elif len(rows) < limit:
                rows[key + (stats['rendered'],)] = (rust_decision(c, reg, depth), u)
    with open(out, 'w') as f:
        for _k, (comp, exp) in rows.items():
            f.write(json.dumps({"expected": exp, "input": comp}, sort_keys=True) + '\n')
    print(json.dumps(stats, indent=1))
    print('variants', len(variants))
    for k, v in sorted(variants.items(), key=lambda kv: -kv[1])[:20]:
        print(' ', k, v)
    print('fixtures written', len(rows), '->', out)


if __name__ == '__main__':
    main()
