#!/usr/bin/env python3
"""Reference renderer for the c11 `management_replay` user prompt (format table for Rust).

Mirrors build_management_c10.render_user + inject_enrichment.(block,dev) +
inject_c11_flow + cost_authority.(line,decompose).
"""
import re, math, sys

sys.path.insert(0, '/tmp')
import p1_ref2 as D          # decision-family helpers: flow, cost authority, formatters


def g_interval(txt, n):
    """The f64 interval that Python's `%.<n>g` maps to `txt`."""
    import math
    v = float(txt)
    if v == 0:
        return -1e-300, 1e-300
    u = 10.0 ** (math.floor(math.log10(abs(v))) - (n - 1))
    return v - u * 0.5, v + u * 0.5


def refine_qty_mark(c, printed_value, qty_txt, mark_txt):
    """The `.6g`/`.12g` prints are lossy: pick a qty_pre inside its own print interval
    whose product with the printed mark renders as the printed product.

    The product's interval fixes the admissible qty interval directly, so this is a
    1-D intersection, not a search.
    """
    import math
    qlo, qhi = g_interval(qty_txt, 6)
    vlo, vhi = g_interval(printed_value, 6)
    m = c['mark']
    if m <= 0:
        return True
    qlo2, qhi2 = max(qlo, vlo / m), min(qhi, vhi / m)
    if qlo2 < qhi2:
        for i in range(101):
            q = qlo2 + (qhi2 - qlo2) * i / 100.0
            if g(q, 6) == qty_txt and g(q * m, 6) == printed_value:
                c['qty_pre'] = q
                return True
    mlo, mhi = g_interval(mark_txt, 12)
    for j in range(201):
        mm = mlo + (mhi - mlo) * j / 200.0
        if g(mm, 12) != mark_txt:
            continue
        a, b2 = max(qlo, vlo / mm), min(qhi, vhi / mm)
        if a < b2:
            for i in range(101):
                q = a + (b2 - a) * i / 100.0
                if g(q, 6) == qty_txt and g(q * mm, 6) == printed_value:
                    c['qty_pre'], c['mark'] = q, mm
                    return True
    return False


def g(v, n):
    return '%.*g' % (n, float(v))


def f(v, n):
    return '%.*f' % (n, float(v))


VENUE_TO_REGIME = {"pumpfun": "bonding_curve", "pumpswap": "amm",
                   "bonding_curve": "bonding_curve", "amm": "amm"}


def regime_for(venue):
    return VENUE_TO_REGIME.get((venue or "").strip().lower(), "bonding_curve")


def darkish(v):
    return v is None or (isinstance(v, str) and v.strip().lower() in ("", "n/a", "none", "null"))


def part(keys, vals):
    out = [f"{k}={D.pn(vals[k])}" for k in keys if k in vals and not darkish(vals[k])]
    return " ".join(out)


# ------------------------------------------------------------------ cost line
def ca_line(clip, depth, regime, uniform_venue_fee=False):
    d = D.decompose(clip, depth, regime, uniform_venue_fee=uniform_venue_fee)
    if uniform_venue_fee or regime not in ("bonding_curve", "amm"):
        venue = "94.63 bp/side, measured - the rate our own fills paid, charged on every venue"
    else:
        venue = ("94.63 bp/side measured on the bonding curve, 30 bp/side on the PumpSwap "
                 "AMM (its own LP + protocol schedule)")
    dep = ('%s SOL' % g(d['depth_sol'], 4)) if d['depth_sol'] else 'unknown'
    return ("execution cost: round trip %d bp = 2 x venue (%s) + 2 x clip impact "
            "(%.2f bp/leg for %.2f SOL into %s) + 2 x tx fee "
            "(%.3f bp/leg, measured)" % (
                d['round_trip_bp'], venue, d['impact_bp_per_leg'], d['clip_sol'], dep,
                d['fixed_tx_bp_per_leg']))


def recover_depth(clip, regime, want_rt, want_impact, want_depth_txt, clip_txt=None):
    """The pool depth the corpus cost line was computed from (it is not printed exactly).

    impact = clip/depth is printed at `.2f`, the clip at `.2f` and the depth at `.4g`,
    so intersect those three intervals, then confirm by rendering the whole line.
    """
    d0 = float(want_depth_txt)
    u = 10.0 ** (math.floor(math.log10(d0)) - 3)
    lo, hi = max(1e-6, d0 - u * 0.5), d0 + u * 0.5
    clo, chi = (float(clip_txt) - 0.005, float(clip_txt) + 0.005) if clip_txt else (clip, clip)
    if chi <= 0:
        chi = clo = clip
    a = clo * D.BPS_ONE / (want_impact + 0.005)
    b = chi * D.BPS_ONE / (want_impact - 0.005)
    lo, hi = max(lo, a), min(hi, b)
    if lo >= hi:
        lo, hi = d0 - u, d0 + u
    for k in range(401):
        d = lo + (hi - lo) * k / 400.0
        if ca_line(clip, d, regime) == want_rt[1]:
            return d
    mid = (lo + hi) / 2.0
    for k in range(1, 61):                      # widen once, in case of print drift
        for d in (mid - u * k / 20.0, mid + u * k / 20.0):
            if d > 0 and ca_line(clip, d, regime) == want_rt[1]:
                return d
    return None


MGMT_HEAD = re.compile(
    r"Decide the next action for a position you already hold\. Only causal information "
    r"is shown\.\nMINT: (\S+)\nDECISION TIME \(unix ms\): (\d+)\nSTEP: (\d+)\n\n"
    r"MARKET STATE \(at decision time\):\n"
    r"  venue: (\S+)  market: (\S+)\n"
    r"  pool depth: (\S+) SOL\n"
    r"  mark price \(lamports per raw token\): (\S+)\n"
    r"  mark price \(SOL per raw token\): (\S+)\n"
    r"  mark price \(SOL per whole token\): (\S+)\n"
    r"  (execution cost: .*)\n\n"
    r"POSITION STATE \(at the decision instant, before any action\):\n"
    r"  entry price \(lamports per raw token\): (\S+)\n"
    r"  unrealized PnL: (\S+) bp\n"
    r"  holding time: (\S+) s\n"
    r"  max favourable so far: (\S+) bp\n"
    r"  max adverse so far: (\S+) bp\n"
    r"  inventory: (\S+) raw tokens\n"
    r"  cash: (\S+) SOL\n"
    r"  position value at mark: (\S+) SOL\n")

MGMT_ENR = re.compile(r"ENRICHED CANDIDATE STATE: (.*)\nDEV HISTORY: (.*)\n"
                      r"(LIVE FLOW STATE: .*)\n\n"
                      r"Choose exactly one action: HOLD, ADD, REDUCE, EXIT\. "
                      r"Answer in the fixed format\.\n$")

ENR_KEYS = ("holders_at_t", "top1_float_share", "holder_hhi", "mcap_sol_at_t",
            "bundle_wallets", "round_trip_wallets", "creator_past_launches",
            "creator_known", "wash_ratio")
DEV_KEYS = ("creator_past_launches", "creator_known", "bundle_wallets", "wash_ratio")


def kv(s):
    out = {}
    for tok in s.split(' '):
        k, _, v = tok.partition('=')
        out[k] = v
    return out


def parse_pynum(t):
    if t == 'True':
        return ('b', True)
    if t == 'False':
        return ('b', False)
    return ('i', int(t)) if re.fullmatch(r'-?\d+', t) else ('f', float(t))


def parse_management(u):
    m = MGMT_HEAD.match(u)
    assert m, 'mgmt-head:' + u[:80]
    c = {'mint': m.group(1), 't_dec': int(m.group(2)), 'step': int(m.group(3)),
         'venue': m.group(4), 'market': m.group(5), 'depth_txt': m.group(6),
         'mark': float(m.group(7)), 'cost_line': m.group(10),
         'entry_px': float(m.group(11)), 'upnl_bp': float(m.group(12)),
         'held_s': float(m.group(13)), 'mfe_bp': float(m.group(14)),
         'mae_bp': float(m.group(15)), 'qty_pre': float(m.group(16)),
         'cash_pre': float(m.group(17))}
    for nm, tok in (('mark_sol_raw', 8), ('mark_sol_whole', 9)):
        c[nm] = m.group(tok)
    c['qty_txt'], c['mark_txt'], c['value_txt'] = m.group(16), m.group(7), m.group(18)
    e = MGMT_ENR.match(u[m.end():])
    assert e, 'mgmt-enr:' + u[m.end():m.end() + 80]
    c['enr'] = {k: parse_pynum(v) for k, v in kv(e.group(1)).items()}
    c['dev'] = {k: parse_pynum(v) for k, v in kv(e.group(2)).items()}
    c['flow'] = D.parse_flow(e.group(3))
    c['enr_text'], c['dev_text'] = e.group(1), e.group(2)
    return c


def render_management(c, depth):
    """Only the cost line depends on the recovered depth; everything else is printed."""
    regime = regime_for(c['venue'])
    clip = c['qty_pre'] * c['mark']
    p = ["Decide the next action for a position you already hold. Only causal "
         "information is shown.",
         "MINT: %s" % c['mint'],
         "DECISION TIME (unix ms): %d" % c['t_dec'],
         "STEP: %d" % c['step'],
         "",
         "MARKET STATE (at decision time):",
         "  venue: %s  market: %s" % (c['venue'], c['market']),
         "  pool depth: %s SOL" % g(depth, 4),
         "  mark price (lamports per raw token): %s" % g(c['mark'], 12),
         "  mark price (SOL per raw token): %s" % g(c['mark'] / 1e9, 12),
         "  mark price (SOL per whole token): %s" % g(c['mark'] * 1e6 / 1e9, 12),
         "  %s" % ca_line(clip, depth, regime),
         "",
         "POSITION STATE (at the decision instant, before any action):",
         "  entry price (lamports per raw token): %s" % g(c['entry_px'], 12),
         "  unrealized PnL: %s bp" % f(c['upnl_bp'], 1),
         "  holding time: %s s" % f(c['held_s'], 0),
         "  max favourable so far: %s bp" % f(c['mfe_bp'], 1),
         "  max adverse so far: %s bp" % f(c['mae_bp'], 1),
         "  inventory: %s raw tokens" % g(c['qty_pre'], 6),
         "  cash: %s SOL" % g(c['cash_pre'], 6),
         "  position value at mark: %s SOL" % g(c['qty_pre'] * c['mark'], 6),
         "ENRICHED CANDIDATE STATE: %s" % part(ENR_KEYS, c['enr']),
         "DEV HISTORY: %s" % part(DEV_KEYS, c['dev']),
         D.render_flow(c['flow']),
         "",
         "Choose exactly one action: HOLD, ADD, REDUCE, EXIT. Answer in the fixed format.",
         ""]
    return '\n'.join(p)
