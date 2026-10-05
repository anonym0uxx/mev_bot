#!/usr/bin/env python3
"""Generate byte-parity fixtures for the c11 `management_replay` user prompt.

The prompt prints qty_pre at `.6g`, mark at `.12g` and the pool depth at `.4g`, all
lossy. The row's own meta records the exact qty/depth, so the fixture's inputs are
recovered as: depth from meta.depth_sol, qty_pre by intersecting its own print
interval with the intervals implied by the printed product and the printed clip
impact, mark/entry/mfe/mae from their printed text.
"""
import json, sys, collections, re
sys.path.insert(0, '/tmp')
import p1_mgmt as M
import p1_ref2 as D

OUT = sys.argv[1] if len(sys.argv) > 1 else '/tmp/fixtures_management.jsonl'
LIMIT = int(sys.argv[2]) if len(sys.argv) > 2 else 600



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


def scal(v):
    kind, x = v
    return int(x) if kind == 'i' else (float(x) if kind == 'f' else bool(x))


def rust_mgmt(c, depth):
    return {
        "mint": c['mint'], "t_dec": c['t_dec'], "step": c['step'],
        "venue": c['venue'], "market": c['market'], "depth_sol": depth,
        "mark": c['mark'], "entry_px": c['entry_px'], "upnl_bp": c['upnl_bp'],
        "held_s": c['held_s'], "mfe_bp": c['mfe_bp'], "mae_bp": c['mae_bp'],
        "qty_pre": c['qty_pre'], "cash_pre": c['cash_pre'],
        "enriched": {k: scal(v) for k, v in c['enr'].items()},
        "dev": {k: scal(v) for k, v in c['dev'].items()},
        "flow": plain(c['flow']),
        "flow_no_prior": bool(c['flow']['no_prior_flow']),
    }


def recover(c, meta, want_line):
    """Intersect the print intervals of qty, the product and the clip impact."""
    depth = float(meta['depth_sol'])
    reg = M.regime_for(c['venue'])
    mi = re.search(r'clip impact \(([0-9.]+) bp/leg', want_line)
    i2 = float(mi.group(1))
    m = c['mark']
    # narrow the mark to the intersection of its own print interval with the two
    # derived prints (they are printed at the same precision from the same value).
    ml, mh = M.g_interval(c['mark_txt'], 12)
    al, ah = M.g_interval(c['mark_sol_raw'], 12)
    bl, bh = M.g_interval(c['mark_sol_whole'], 12)
    lo = max(ml, al * 1e9, bl * 1e9 / 1e6)
    hi = min(mh, ah * 1e9, bh * 1e9 / 1e6)
    if lo < hi:
        for i in range(2001):
            mm = lo + (hi - lo) * i / 2000.0
            if M.g(mm, 12) == c['mark_txt'] and M.g(mm / 1e9, 12) == c['mark_sol_raw'] \
                    and M.g(mm * 1e6 / 1e9, 12) == c['mark_sol_whole']:
                c['mark'] = mm
                break
    m = c['mark']
    qlo, qhi = M.g_interval(c['qty_txt'], 6)
    vlo, vhi = M.g_interval(c['value_txt'], 6)
    clo, chi = depth * (i2 - 0.005) / D.BPS_ONE, depth * (i2 + 0.005) / D.BPS_ONE
    lo = max(qlo, vlo / m, clo / m)
    hi = min(qhi, vhi / m, chi / m)
    if lo < hi:
        for i in range(201):
            q = lo + (hi - lo) * i / 200.0
            c['qty_pre'] = q
            if M.g(q, 6) == c['qty_txt'] and M.g(q * m, 6) == c['value_txt'] \
                    and M.ca_line(q * m, depth, reg) == want_line:
                return depth
    for j in range(401):                        # fall back: nudge the mark too
        mm = M.g_interval(c['mark_txt'], 12)[0] + (M.g_interval(c['mark_txt'], 12)[1]
                                                   - M.g_interval(c['mark_txt'], 12)[0]) * j / 400.0
        if M.g(mm, 12) != c['mark_txt']:
            continue
        lo = max(qlo, vlo / mm, clo / mm)
        hi = min(qhi, vhi / mm, chi / mm)
        if lo < hi:
            for i in range(201):
                q = lo + (hi - lo) * i / 200.0
                if M.g(q, 6) == c['qty_txt'] and M.g(q * mm, 6) == c['value_txt'] \
                        and M.ca_line(q * mm, depth, reg) == want_line:
                    c['qty_pre'], c['mark'] = q, mm
                    return depth
    return None


def main():
    stats = collections.Counter()
    rows = {}
    with open('/training/v2/candidate_sft_c11/train.jsonl') as f:
        for line in f:
            r = json.loads(line)
            if r.get('family') != 'management_replay':
                continue
            stats['total'] += 1
            u = r['messages'][1]['content']
            meta = r.get('meta') or {}
            if 'depth_sol' not in meta:
                stats['no_meta_depth'] += 1
                continue
            try:
                c = M.parse_management(u)
            except Exception as ex:
                stats['parse:' + str(ex)[:40]] += 1
                continue
            want = [ln[2:] for ln in u.split('\n') if ln.startswith('  execution cost:')][0]
            d = recover(c, meta, want)
            if d is None:
                stats['norecover'] += 1
                if stats['norecover'] < 4:
                    print('NORECOVER', c['venue'], meta['depth_sol'], want[-70:])
                continue
            if M.render_management(c, d) != u:
                stats['mismatch'] += 1
                if stats['mismatch'] < 4:
                    for i, (a, b) in enumerate(zip(M.render_management(c, d).split('\n'),
                                                   u.split('\n'))):
                        if a != b:
                            print('DIFF', i, '\n mine:', a[:190], '\n corpu:', b[:190])
                            break
                continue
            stats['ok'] += 1
            key = (c['venue'], c['market'], c['flow']['no_prior_flow'],
                   len(c['enr']), len(c['dev']))
            if key not in rows:
                rows[key] = (rust_mgmt(c, d), u)
            elif len(rows) < LIMIT:
                rows[key + (stats['ok'],)] = (rust_mgmt(c, d), u)
    with open(OUT, 'w') as f:
        for _k, (comp, exp) in rows.items():
            f.write(json.dumps({"expected": exp, "input": comp}, sort_keys=True) + '\n')
    print(json.dumps(dict(stats), indent=1))
    print('fixtures written', len(rows), '->', OUT)


main()
