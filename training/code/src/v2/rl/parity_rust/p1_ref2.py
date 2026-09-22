#!/usr/bin/env python3
"""P1 reference renderer for the c11 decision + management_replay user prompts.

This is the format table that gets ported 1:1 to Rust. It parses a corpus row into
typed components and re-renders, so any byte difference is a bug in the table.

Number model: the corpus was written by Python, where a value's TYPE (int vs float)
decides its rendering: str(0) == '0' but str(0.0) == '0.0'. Every "python number"
field is therefore modelled as PyNum = int | float and re-rendered accordingly.
Fixed-format fields (%.9f / %.18f / %.6f / %.Ng / {:.10g}) are modelled as f64.
"""
import json
import re
import sys

# ------------------------------------------------------------------ formatters
def pn(pnum):
    """PyNum -> text: int plain, bool True/False, float python-repr (.0 kept)."""
    kind, v = pnum
    if kind == 'i':
        return str(v)
    if kind == 'b':
        return 'True' if v else 'False'
    return pf(v)


def pf(x):
    """Python str(float) — Rust `{}` drops the `.0`, Python keeps it."""
    s = repr(float(x))
    if 'e' in s or 'E' in s or '.' in s or s in ('inf', '-inf', 'nan'):
        return s
    return s + '.0'


def g(x, n):
    return '%.*g' % (n, float(x))


def opt(prefix, value, fmt):
    return 'n/a' if value is None else fmt(value)


# ------------------------------------------------------------------- decision
DEC_HEAD = re.compile(
    r"DECISION CLOCK — assess this opportunity\.\n"
    r"t_dec_ms=(\d+)  age_s=(\S+)  last_trade_age_s=(\S+)\n"
    r"venue=(\S+)  curve_present=(\S+)  evidence_status=(\S+)\n"
    r"n_prior_trades=(\d+)  buy_count=(\d+)  sell_count=(\d+)  unique_traders=(\d+)\n"
    r"price_lamports_per_raw_token=(\S+)  ret_5s_bp=(\S+)  ret_30s_bp=(\S+)  vol_30s_bp=(\S+)\n"
    r"buy_volume_lamports=(-?\d+)  sell_volume_lamports=(-?\d+)  net_flow_lamports=(-?\d+)\n"
    r"top1_trader_share=(\S+)  top5_trader_share=(\S+)  buyer_seller_ratio=(\S+)\n")

FLOW_FIELDS = ("entrants_60s", "entrants_300s", "net_flow_sol_300s",
               "fresh_wallet_share_300s", "flow_lookback_d", "sniper_share_300s",
               "bot_uniform_share_300s", "smart_entrants_300s", "smart_net_flow_sol_300s",
               "coentry_wallets_300s", "creator_trading_own_mint",
               "entrant_fee_p90_lamports", "entrant_cu_p50")
FLOW_FLOAT = {"net_flow_sol_300s", "fresh_wallet_share_300s", "flow_lookback_d",
              "sniper_share_300s", "bot_uniform_share_300s", "smart_net_flow_sol_300s"}
FLOW_INT = {"entrants_60s", "entrants_300s", "smart_entrants_300s", "coentry_wallets_300s"}
FLOW_OPT_INT = {"entrant_fee_p90_lamports", "entrant_cu_p50"}


def pnum_or_none(t):
    if t in ('None', 'n/a', 'na'):
        return None
    if re.fullmatch(r'-?\d+', t):
        return ('i', int(t))
    return ('f', float(t))


def parse_flow(s):
    s = s.split('FLOW STATE: ', 1)[1] if 'FLOW STATE: ' in s else s
    if s.startswith('no_prior_flow=true'):
        return {'no_prior_flow': True}
    d = {'no_prior_flow': False}
    for tok in s.split(' '):
        k, _, v = tok.partition('=')
        assert k in FLOW_FIELDS, k
        if k == 'creator_trading_own_mint':
            d[k] = (v == 'True')
        elif k in FLOW_INT:
            d[k] = int(v)
        elif k in FLOW_OPT_INT:
            d[k] = None if v == 'None' else int(v)
        elif k in FLOW_FLOAT:
            d[k] = pnum_or_none(v)
        else:
            raise AssertionError(k)
    assert len(d) == 14, d
    return d


def parse_kv_line(s, order):
    parts = []
    for tok in s.split(' '):
        k, _, v = tok.partition('=')
        parts.append((k, v))
    assert [k for k, _ in parts] == list(order), (parts, order)
    return dict(parts)


def lam_from_sol_text(t):
    """A `.9f` SOL print -> the exact integer lamports (no float round-trip)."""
    import decimal
    d = decimal.Decimal(t).scaleb(9)
    return int(d.to_integral_value(rounding=decimal.ROUND_HALF_EVEN))


def parse_curve(s):
    if s.startswith('curve_reserves=absent'):
        return {'kind': 'absent', 'reason': s.split('reason=', 1)[1]}
    a = re.fullmatch(
        r"curve_reserves=present src=laserstream "
        r"staleness_ms=(\d+) pricing_eligible=(true|false) v_sol_reserves_sol=(\S+) "
        r"v_tokens_reserves=(\d+) real_sol_reserves_sol=(\S+) real_tokens_reserves=(\d+) "
        r"curve_price_sol_per_raw_token=(\S+) curve_k=(\d+) curve_progress=(\S+) "
        r"curve_regime=(\S+)", s)
    assert a, 'curve-regex'
    return {'kind': 'present', 'src': 'laserstream', 'staleness_ms': int(a.group(1)),
            'pricing_eligible': a.group(2) == 'true',
            'v_sol_reserves_lamports': lam_from_sol_text(a.group(3)),
            'v_sol_reserves_sol': float(a.group(3)),
            'real_sol_reserves_lamports': lam_from_sol_text(a.group(5)),
            'v_tokens_reserves': int(a.group(4)), 'real_sol_reserves_sol': float(a.group(5)),
            'real_tokens_reserves': int(a.group(6)), 'curve_price_sol_per_raw_token': float(a.group(7)),
            'curve_k': a.group(8), 'curve_progress': float(a.group(9)), 'curve_regime': a.group(10)}


def parse_amm(s):
    if s.startswith('amm_reserves=absent'):
        return {'kind': 'absent', 'reason': s.split('reason=', 1)[1]}
    a = re.fullmatch(
        r"amm_reserves=present src=helius_amm_backfill pool=(\S+) quote=WSOL "
        r"staleness_ms=(\d+) pricing_eligible=(true|false) base_reserves_raw=(\d+) "
        r"quote_reserves_lamports=(\d+) quote_reserves_sol=(\S+) "
        r"amm_price_sol_per_raw_token=(\S+) reserve_slot=(\S+)", s)
    assert a, 'amm-regex'
    return {'kind': 'present', 'src': 'helius_amm_backfill', 'pool': a.group(1),
            'staleness_ms': int(a.group(2)), 'pricing_eligible': a.group(3) == 'true',
            'base_reserves_raw': int(a.group(4)), 'quote_reserves_lamports': int(a.group(5)),
            'quote_reserves_sol': float(a.group(6)),
            'amm_price_sol_per_raw_token': float(a.group(7)), 'reserve_slot': a.group(8)}


SIZE_RE = re.compile(r"^  (SMALL|MID|FULL) = (\S+) SOL - (\d+) bp round trip \(pool depth ([0-9.]+) SOL\)$")


def render_flow(f):
    if f['no_prior_flow']:
        return 'LIVE FLOW STATE: no_prior_flow=true'
    out = []
    for k in FLOW_FIELDS:
        v = f[k]
        if k == 'creator_trading_own_mint':
            s = 'True' if v else 'False'
        elif isinstance(v, tuple):
            s = pn(v)
        else:
            s = str(v)
        out.append('%s=%s' % (k, s))
    return 'LIVE FLOW STATE: ' + ' '.join(out)


def render_curve(c):
    if c['kind'] == 'absent':
        return 'CURVE STATE (at decision time): curve_reserves=absent reason=%s' % c['reason']
    return ('CURVE STATE (at decision time): curve_reserves=present src=laserstream '
            'staleness_ms=%d pricing_eligible=%s v_sol_reserves_sol=%.9f v_tokens_reserves=%d '
            'real_sol_reserves_sol=%.9f real_tokens_reserves=%d '
            'curve_price_sol_per_raw_token=%.18f curve_k=%s curve_progress=%.6f '
            'curve_regime=%s' % (
                c['staleness_ms'],
                'true' if c['pricing_eligible'] else 'false',
                c['v_sol_reserves_sol'], c['v_tokens_reserves'],
                c['real_sol_reserves_sol'], c['real_tokens_reserves'],
                c['curve_price_sol_per_raw_token'], c['curve_k'], c['curve_progress'],
                c['curve_regime']))


def render_amm(a):
    if a['kind'] == 'absent':
        return 'AMM POOL STATE (at decision time): amm_reserves=absent reason=%s' % a['reason']
    return ('AMM POOL STATE (at decision time): amm_reserves=present src=helius_amm_backfill '
            'pool=%s quote=WSOL staleness_ms=%d pricing_eligible=%s base_reserves_raw=%d '
            'quote_reserves_lamports=%d quote_reserves_sol=%.9f '
            'amm_price_sol_per_raw_token=%.18f reserve_slot=%s' % (
                a['pool'], a['staleness_ms'],
                'true' if a['pricing_eligible'] else 'false', a['base_reserves_raw'],
                a['quote_reserves_lamports'], a['quote_reserves_sol'],
                a['amm_price_sol_per_raw_token'], a['reserve_slot']))


def render_price_units(px):
    if not px or px[0] == 'i' and px[1] == 0 or px[0] == 'f' and float(px[1]) == 0.0:
        return 'PRICE UNITS: price_lamports_per_raw_token=absent reason=no_supplied_price'
    v = float(px[1])
    return ('PRICE UNITS: price_lamports_per_raw_token=%s '
            'price_sol_per_raw_token=%s price_sol_per_whole_token=%s' % (
                g(v, 10), g(v / 1e9, 10), g(v * 1e6 / 1e9, 10)))


# --------------------------------------------------------------- cost authority
BPS_ONE = 10_000
LAMPORTS_PER_SOL = 1_000_000_000
AMM_VENUE_FEE_BPS_PER_LEG = 30
BONDING_FEE_BPS_MEASURED = 94.63
FIXED_LAMPORTS_PER_LEG_P50 = 10_000
DEPLOY_SOL_CANONICAL = 1.0


def py_round_half_even(x, nd):
    import decimal
    return float(decimal.Decimal(x).quantize(decimal.Decimal(1).scaleb(-nd),
                                             rounding=decimal.ROUND_HALF_EVEN))


def cost_floor_bps(regime="amm", notional_sol=DEPLOY_SOL_CANONICAL, expected_impact_bps=0,
                   fee_quantile="p50"):
    lam = {"p50": FIXED_LAMPORTS_PER_LEG_P50, "p90": 45_000, "p99": 1_005_000}[fee_quantile]
    notional_lamports = max(1.0, float(notional_sol) * LAMPORTS_PER_SOL)
    fixed_bps_per_leg = lam / notional_lamports * BPS_ONE
    venue_bps = BONDING_FEE_BPS_MEASURED if regime == "bonding_curve" else AMM_VENUE_FEE_BPS_PER_LEG
    floor = 2.0 * (venue_bps + fixed_bps_per_leg) + 2.0 * int(expected_impact_bps)
    return int(py_round_half_even(floor, 0))


def decompose(clip_sol=DEPLOY_SOL_CANONICAL, depth_sol=None, regime="bonding_curve",
              fee_quantile="p50", uniform_venue_fee=False):
    lam = FIXED_LAMPORTS_PER_LEG_P50
    notional = max(1e-9, float(clip_sol))
    fixed_bp_per_leg = lam / (notional * LAMPORTS_PER_SOL) * BPS_ONE
    venue_bp = BONDING_FEE_BPS_MEASURED
    floor_regime = "bonding_curve"
    if not uniform_venue_fee:
        venue_bp = BONDING_FEE_BPS_MEASURED if regime == "bonding_curve" else AMM_VENUE_FEE_BPS_PER_LEG
        floor_regime = regime
    impact_bp_per_leg = (float(clip_sol) / depth_sol * BPS_ONE) if depth_sol else 0.0
    total = cost_floor_bps(floor_regime, notional_sol=clip_sol,
                           expected_impact_bps=int(py_round_half_even(impact_bp_per_leg, 0)),
                           fee_quantile=fee_quantile)
    return {"clip_sol": float(clip_sol), "depth_sol": depth_sol, "regime": regime,
            "venue_bp_per_leg": venue_bp, "impact_bp_per_leg": impact_bp_per_leg,
            "fixed_tx_bp_per_leg": fixed_bp_per_leg, "round_trip_bp": int(total)}


def size_options(regime, depth_sol, tiers=(("SMALL", 0.25), ("MID", 0.50), ("FULL", 1.00)),
                 uniform_venue_fee=False):
    out = ["SIZE OPTIONS (choose one on BUY; round-trip cost from the cost authority):"]
    sizes = {}
    for name, clip in tiers:
        d = decompose(clip, depth_sol, regime, uniform_venue_fee=uniform_venue_fee)
        sizes[name] = d
        depth = ("pool depth %.1f SOL" % depth_sol) if depth_sol else "pool depth unknown"
        out.append("  %s = %.2f SOL - %d bp round trip (%s)" % (name, clip, d["round_trip_bp"], depth))
    out.append("  Size is a judgement: a bigger clip pays more impact in a thinner book.")
    return {"block": "\n".join(out), "sizes": sizes}


ENR_DEC = ("mcap_sol_at_t", "mcap_source", "holders_at_t", "top1_float_share",
           "top5_float_share", "holder_hhi", "bundle_slots", "bundle_wallets",
           "volume_sol_at_t", "wash_ratio")


def parse_decision(u):
    m = DEC_HEAD.match(u)
    assert m, 'head'
    c = {'t_dec_ms': int(m.group(1)), 'age_s': pnum_or_none(m.group(2)),
         'last_trade_age_s': pnum_or_none(m.group(3)), 'venue': m.group(4),
         'curve_present': m.group(5) == 'True', 'evidence_status': m.group(6),
         'n_prior_trades': int(m.group(7)), 'buy_count': int(m.group(8)),
         'sell_count': int(m.group(9)), 'unique_traders': int(m.group(10)),
         'price_lamports_per_raw_token': pnum_or_none(m.group(11)),
         'ret_5s_bp': pnum_or_none(m.group(12)), 'ret_30s_bp': pnum_or_none(m.group(13)),
         'vol_30s_bp': pnum_or_none(m.group(14)),
         'buy_volume_lamports': int(m.group(15)), 'sell_volume_lamports': int(m.group(16)),
         'net_flow_lamports': int(m.group(17)), 'top1_trader_share': pnum_or_none(m.group(18)),
         'top5_trader_share': pnum_or_none(m.group(19)),
         'buyer_seller_ratio': pnum_or_none(m.group(20))}
    rest = u[m.end():].split('\n')
    i = 0
    assert rest[i].startswith('ENRICHED CANDIDATE STATE (strictly causal at t_dec): '), rest[i][:60]
    c['enriched'] = parse_kv_line(rest[i][len('ENRICHED CANDIDATE STATE (strictly causal at t_dec): '):], ENR_DEC)
    i += 1
    assert rest[i].startswith('DEV HISTORY: '), rest[i][:60]
    c['dev'] = parse_kv_line(rest[i][len('DEV HISTORY: '):], ('creator_past_launches', 'creator_known'))
    i += 1
    assert rest[i].startswith('LIVE FLOW STATE: '), rest[i][:60]
    c['flow'] = parse_flow(rest[i][len('LIVE FLOW STATE: '):])
    i += 1
    assert rest[i].startswith('CURVE STATE (at decision time): '), rest[i][:60]
    c['curve'] = parse_curve(rest[i][len('CURVE STATE (at decision time): '):])
    i += 1
    assert rest[i].startswith('AMM POOL STATE (at decision time): '), rest[i][:60]
    c['amm'] = parse_amm(rest[i][len('AMM POOL STATE (at decision time): '):])
    i += 1
    assert rest[i].startswith('PRICE UNITS: '), rest[i][:60]
    c['price_units'] = rest[i][len('PRICE UNITS: '):]
    i += 1
    assert rest[i] == '', repr(rest[i])
    i += 1
    assert rest[i].startswith('Choose exactly one action: BUY, WATCH, SKIP. Respect the stated round-trip cost.'), rest[i][:80]
    i += 1
    assert rest[i].startswith('SIZE OPTIONS (choose one on BUY; round-trip cost from the cost authority):'), rest[i][:80]
    i += 1
    c['sizes'] = []
    for _ in range(3):
        sm = SIZE_RE.match(rest[i])
        assert sm, rest[i][:60]
        c['sizes'].append((sm.group(1), sm.group(2), int(sm.group(3)), sm.group(4)))
        i += 1
    # remaining size rows parsed in render
    c['size_tail'] = rest[i:]
    return c


def render_decision(c, depth_override=None, regime_override=None):
    if c.get('dev'):
        pass
    p = []
    p.append('DECISION CLOCK — assess this opportunity.')
    p.append('t_dec_ms=%d  age_s=%s  last_trade_age_s=%s' % (
        c['t_dec_ms'], pn(c['age_s']), pn(c['last_trade_age_s'])))
    p.append('venue=%s  curve_present=%s  evidence_status=%s' % (
        c['venue'], 'True' if c['curve_present'] else 'False', c['evidence_status']))
    p.append('n_prior_trades=%d  buy_count=%d  sell_count=%d  unique_traders=%d' % (
        c['n_prior_trades'], c['buy_count'], c['sell_count'], c['unique_traders']))
    p.append('price_lamports_per_raw_token=%s  ret_5s_bp=%s  ret_30s_bp=%s  vol_30s_bp=%s' % (
        pn(c['price_lamports_per_raw_token']), opt('', c['ret_5s_bp'], pn),
        opt('', c['ret_30s_bp'], pn), opt('', c['vol_30s_bp'], pn)))
    p.append('buy_volume_lamports=%d  sell_volume_lamports=%d  net_flow_lamports=%d' % (
        c['buy_volume_lamports'], c['sell_volume_lamports'], c['net_flow_lamports']))
    p.append('top1_trader_share=%s  top5_trader_share=%s  buyer_seller_ratio=%s' % (
        pn(c['top1_trader_share']), pn(c['top5_trader_share']),
        opt('', c['buyer_seller_ratio'], pn)))
    e = c['enriched']
    mc = 'na' if e['mcap_sol_at_t'] in ('na', 'None') else pn(pnum_or_none(e['mcap_sol_at_t']))
    p.append('ENRICHED CANDIDATE STATE (strictly causal at t_dec): mcap_sol_at_t=%s '
             'mcap_source=%s holders_at_t=%s top1_float_share=%s top5_float_share=%s '
             'holder_hhi=%s bundle_slots=%s bundle_wallets=%s volume_sol_at_t=%s wash_ratio=%s' % (
                 mc, e['mcap_source'], e['holders_at_t'], pn(pnum_or_none(e['top1_float_share'])),
                 pn(pnum_or_none(e['top5_float_share'])), pn(pnum_or_none(e['holder_hhi'])),
                 e['bundle_slots'], e['bundle_wallets'],
                 pn(pnum_or_none(e['volume_sol_at_t'])), pn(pnum_or_none(e['wash_ratio']))))
    d = c['dev']
    p.append('DEV HISTORY: creator_past_launches=%s creator_known=%s' % (
        d['creator_past_launches'], d['creator_known']))
    p.append(render_flow(c['flow']))
    p.append(render_curve(c['curve']))
    p.append(render_amm(c['amm']))
    p.append(render_price_units(c['price_lamports_per_raw_token']))
    txt = '\n'.join(p) + '\n\nChoose exactly one action: BUY, WATCH, SKIP. ' \
        'Respect the stated round-trip cost. Answer in the fixed format.\n'
    regime = regime_override or ('amm' if c['venue'] == 'pumpswap' else 'bonding_curve')
    depth = depth_override
    if depth is None:
        if c['amm']['kind'] == 'present':
            depth = c['amm']['quote_reserves_sol']
        elif c['curve']['kind'] == 'present':
            depth = c['curve']['v_sol_reserves_sol']
    if c['sizes']:
        # the 1dp depth printed in the block is lossy; the 9dp reserve value is used
        # to reproduce the corpus's cost arithmetic exactly.
        pass
    # This reference reimplementation's local default IS the legacy three-tier menu, so it
    # keeps reproducing the trained corpus (<= c12), which still offered MID. The row builder's
    # authority (`cost_authority.size_options`) now defaults to the RULED two-tier menu — c13.
    txt += size_options(regime, depth)['block'] + '\n'
    return txt


if __name__ == '__main__':
    fam = sys.argv[1] if len(sys.argv) > 1 else 'decision'
    limit = int(sys.argv[2]) if len(sys.argv) > 2 else 2000
    n = ok = 0
    bad = {}
    with open('/training/v2/candidate_sft_c11/train.jsonl') as f:
        for line in f:
            r = json.loads(line)
            if r.get('family') != fam:
                continue
            n += 1
            u = r['messages'][1]['content']
            try:
                c = parse_decision(u)
                got = render_decision(c)
            except Exception as e:
                err = type(e).__name__ + ':' + str(e)[:80]
                bad[err] = bad.get(err, 0) + 1
                if bad[err] <= 1:
                    print('ERR', err)
                continue
            if got == u:
                ok += 1
            else:
                bad['MISMATCH'] = bad.get('MISMATCH', 0) + 1
                if bad['MISMATCH'] <= 3:
                    gl, cl = got.split('\n'), u.split('\n')
                    for i in range(max(len(gl), len(cl))):
                        a = gl[i] if i < len(gl) else '<none>'
                        b = cl[i] if i < len(cl) else '<none>'
                        if a != b:
                            print('DIFF line', i, '\n mine:', a[:220], '\n corpu:', b[:220])
                            break
            if n >= limit:
                break
    print('rows', n, 'ok', ok, 'bad', bad)
