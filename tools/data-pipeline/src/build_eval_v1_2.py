#!/usr/bin/env python
"""Build qwen_eval_v1.2 — corrected-serializer eval on the FROZEN v1.1 partition.

BINDING RULES
  * v1/v1.1 files are NEVER touched (lineage).
  * SAME held-out entities: every panel, mint_id, state_id comes from
    qwen_eval_v1_1.jsonl. Zero resampling.
  * The 29 rust_gold_v1 repair records are carried over BYTE-IDENTICAL.
  * Only derived quantities are recomputed: l3_compressed (corrected
    _bp -> fraction economics), continuous_utility, decision, panel_rank,
    and the model-visible serialization (columnar input + live_action /
    full_rank_aux target via sft_cross_exporter_v2).
  * causal_state / l2_evidence stay frozen from v1.1 (they were never
    affected by the serializer bug) — except NaN scrubbing for utility math.
  * Future outcomes (L2/L3) may shape TARGET labels/rankings but never
    appear in model-visible input or live_action rationale text
    (enforced by assert_no_outcome_leakage at export time + re-checked here).
"""
import copy
import hashlib
import json
import math
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import build_qwen_curriculum_v1 as B          # noqa: E402
import cpt_sft_exporters as X                 # noqa: E402
import sft_cross_exporter_v2 as V2            # noqa: E402

OUT_DIR = os.path.join(B.OUT, 'eval_v1_2')
V1_1_PATH = os.path.join(B.EVAL_DIR_V1_1, 'qwen_eval_v1_1.jsonl')
V1_2_PATH = os.path.join(OUT_DIR, 'qwen_eval_v1_2.jsonl')
MANIFEST_PATH = os.path.join(OUT_DIR, 'EVAL_MANIFEST_V1_2.json')
SFT_V2_PATH = os.path.join(B.OUT, 'sft', 'qwen_sft_v2.jsonl')
TOK_PROV_PATH = os.path.join(B.OUT, 'tokenizer_provenance.json')


def _nn(v):
    """NaN -> None normalizer for frozen numeric fields."""
    if isinstance(v, float) and math.isnan(v):
        return None
    return v


_L2_FIELDS = ('ret_5s_bp', 'ret_30s_bp', 'ret_300s_bp', 'mfe_bp', 'mae_bp',
              'collapsed_50pct_within_300s', 'survived_60s',
              'graduated_after_state')


def load_slinky_l2_by_state(state_ids):
    """CORRECTED L2 join for TARGET evidence (frozen entities, fixed lookup).

    Three defects in the v1/v1.1 evidence path:
      1. by-mint join used the mint's LATEST outcome row — terminal state of a
         dead token, so survived_60s was False for every candidate (no BUY
         could ever gate through: degenerate distributions).
      2. the state-matched outcome rows for these frozen state_ids are
         DEFAULT-INITIALIZED artifacts (all outcome fields null/False,
         has_trade_within_300s=False even in dense trade windows).
      3. genuine computed outcomes live on ~26% of the mint's rows.

    Fix: stage 1 resolves each frozen state_id -> (mint, timestamp_ms) via its
    state-matched row; stage 2 collects the mint's GENUINE outcome rows
    (mfe/mae computed or trade activity observed) and assigns each state the
    nearest genuine row at/after its timestamp (fallback: nearest before).
    Future outcomes remain TARGET-side only. Returns state_id -> l2_row and
    prints join telemetry.
    """
    import glob as _glob
    import pyarrow.parquet as _pq
    sids = list({s for s in state_ids if s})
    files = sorted(_glob.glob(os.path.join(
        B.SLINKY, 'pump_outcome_v3', '*.parquet')))
    want = ['state_id', 'mint', 'timestamp_ms', 'mfe_bp', 'mae_bp',
            'survived_60s', 'survived_300s', 'collapsed_50pct_within_300s',
            'ret_5s_bp', 'ret_30s_bp', 'ret_300s_bp', 'right_censored_300s',
            'observed_through_300s', 'has_trade_within_300s',
            'graduated_after_state']

    def read(fp, filt):
        avail = set(_pq.ParquetFile(fp).schema_arrow.names)
        return _pq.read_table(fp, columns=[c for c in want if c in avail],
                              filters=filt).to_pylist()

    # stage 1: state_id -> (mint, ts)
    anchor = {}
    for fp in files:
        for row in read(fp, [('state_id', 'in', sids)]):
            anchor[row['state_id']] = (row.get('mint'), row.get('timestamp_ms'))
    mints = list({m for m, _ in anchor.values() if m})

    def genuine(row):
        return (row.get('mfe_bp') is not None
                or bool(row.get('has_trade_within_300s'))
                or bool(row.get('observed_through_300s')))

    # stage 2: genuine rows per mint
    by_mint = {}
    for fp in files:
        for row in read(fp, [('mint', 'in', mints)]):
            if genuine(row):
                by_mint.setdefault(row['mint'], []).append(row)
    for m in by_mint:
        by_mint[m].sort(key=lambda r: r.get('timestamp_ms') or 0)

    out, tele = {}, {'exact': 0, 'forward': 0, 'backward': 0, 'unmatched': 0,
                     'dt_ms': []}
    for sid, (m, ts) in anchor.items():
        rows = by_mint.get(m) or []
        if not rows:
            tele['unmatched'] += 1
            continue
        fwd = [r for r in rows if (r.get('timestamp_ms') or 0) >= (ts or 0)]
        if fwd:
            r = min(fwd, key=lambda r: (r.get('timestamp_ms') or 0) - (ts or 0))
            kind = 'exact' if r.get('timestamp_ms') == ts else 'forward'
        else:
            r = rows[-1]
            kind = 'backward'
        tele[kind] += 1
        tele['dt_ms'].append(abs((r.get('timestamp_ms') or 0) - (ts or 0)))
        out[sid] = r
    dts = sorted(tele['dt_ms'])
    tele['dt_ms'] = {'p50': dts[len(dts) // 2] if dts else None,
                     'p90': dts[int(len(dts) * 0.9)] if dts else None,
                     'max': dts[-1] if dts else None}
    print(f'  L2 genuine-row join: {tele}', flush=True)
    return out


def recompute_slinky_candidate(cand, cf_lookup, l2_by_state):
    """Recompute economics for one frozen Slinky candidate."""
    sid = (cand.get('provenance_ids') or {}).get('state_id')
    cf = cf_lookup.get(sid, {}) if (sid and cf_lookup) else {}
    l3c = B.compute_slinky_cf_compressed(dict(cf)) if cf else None

    # CORRECTED state-matched L2 evidence (fallback: frozen v1.1 row)
    l2_row = l2_by_state.get(sid)
    if l2_row is not None:
        cand['l2_evidence'] = {k: _nn(l2_row.get(k)) for k in _L2_FIELDS}
    l2 = cand.get('l2_evidence') or {}
    mae_bp = _nn(l2.get('mae_bp'))
    mfe_bp = _nn(l2.get('mfe_bp'))
    collapsed = bool(_nn(l2.get('collapsed_50pct_within_300s')) or False)
    if l2_row is not None and 'right_censored_300s' in l2_row:
        cens_adj = 1.0 - (1.0 if _nn(l2_row['right_censored_300s']) else 0.0)
    else:
        prev_cu = cand.get('continuous_utility') or {}
        cens_adj = _nn(prev_cu.get('censoring_adjustment'))
        cens_adj = 1.0 if cens_adj is None else cens_adj

    feasibility = _nn(l3c['feasible_ratio']) if l3c else 0.0
    net_econ = _nn(l3c['median_net_return_pct']) if l3c else None
    downside = (mae_bp / 10000) if mae_bp is not None else None
    upside = (mfe_bp / 10000) if mfe_bp is not None else None
    size_rob = _nn(l3c.get('size_robustness')) if l3c else 0.0

    utility = B.compute_robust_utility_v1(
        feasibility, net_econ, downside, upside, size_rob, None, cens_adj)
    decision = B.derive_buy_watch_skip(utility, feasibility, net_econ, collapsed)

    cand['l3_compressed'] = l3c
    cand['continuous_utility'] = {
        'robust_executable_utility_v1': utility,
        'feasibility_score': feasibility,
        'net_sol_economics': net_econ,
        'downside_risk': downside,
        'upside_potential': upside,
        'size_robustness': size_rob,
        'latency_robustness': None,
        'censoring_adjustment': cens_adj,
    }
    cand['decision'] = decision
    return cf != {}


def recompute_ls_candidate(cand, l3_map, l2_map):
    sid = (cand.get('provenance_ids') or {}).get('state_id')
    l3_scen = l3_map.get(sid, [])
    l3c = B.compute_l3_compressed(l3_scen) if l3_scen else None

    l2 = l2_map.get(sid) or cand.get('l2_evidence') or {}
    mae_bp = _nn(l2.get('mae_bp'))
    mfe_bp = _nn(l2.get('mfe_bp'))
    collapsed = bool(_nn(l2.get('collapsed_50pct')) or False)
    right_cens = bool(_nn(l2.get('right_censored_300s')) or False)

    feasibility = _nn(l3c['feasible_ratio']) if l3c else 0.0
    net_econ = _nn(l3c['median_net_return_pct']) if l3c else None
    downside = (mae_bp / 10000) if mae_bp is not None else None
    upside = (mfe_bp / 10000) if mfe_bp is not None else None
    size_rob = _nn(l3c.get('size_robustness')) if l3c else 0.0
    lat_rob = _nn(l3c.get('latency_robustness')) if l3c else None
    cens_adj = 1.0 - (1.0 if right_cens else 0.0)
    feasibility = feasibility if feasibility is not None else 0.0
    size_rob = size_rob if size_rob is not None else 0.0

    utility = B.compute_robust_utility_v1(
        feasibility, net_econ, downside, upside, size_rob, lat_rob, cens_adj)
    decision = B.derive_buy_watch_skip(utility, feasibility, net_econ, collapsed)

    cand['l3_compressed'] = l3c
    cand['continuous_utility'] = {
        'robust_executable_utility_v1': utility,
        'feasibility_score': feasibility,
        'net_sol_economics': net_econ,
        'downside_risk': downside,
        'upside_potential': upside,
        'size_robustness': size_rob,
        'latency_robustness': lat_rob,
        'censoring_adjustment': cens_adj,
    }
    cand['decision'] = decision
    return bool(l3_scen)


def main():
    t0 = time.time()
    os.makedirs(OUT_DIR, exist_ok=True)
    print('=== BUILD qwen_eval_v1.2 (frozen v1.1 partition, corrected economics) ===', flush=True)

    raw_lines = [l.rstrip('\n') for l in open(V1_1_PATH, encoding='utf-8') if l.strip()]
    recs = [json.loads(l) for l in raw_lines]
    cross = [(i, r) for i, r in enumerate(recs) if r.get('candidates')]
    rust = [(i, r, raw_lines[i]) for i, r in enumerate(recs) if not r.get('candidates')]
    print(f'  v1.1: {len(recs)} records = {len(cross)} cross panels + {len(rust)} rust repairs', flush=True)
    assert len(rust) == 29, f'expected 29 rust repairs, got {len(rust)}'

    slinky_panels = [(i, r) for i, r in cross if r.get('panel_source') == 'slinky']
    ls_panels = [(i, r) for i, r in cross if r.get('panel_source') == 'laserstream']

    # ── preload CF gold (Slinky) ──
    B.preload_slinky_counterfactuals()
    cf_lookup = B._SLINKY_CF_BY_STATE

    # ── CORRECTED Slinky L2 join by frozen state_id ──
    sl_sids = {(c.get('provenance_ids') or {}).get('state_id')
               for _, r in slinky_panels for c in r['candidates']}
    sl_sids.discard(None)
    print(f'  loading state-matched Slinky L2 for {len(sl_sids)} state_ids...', flush=True)
    sl_l2 = load_slinky_l2_by_state(sl_sids)
    print(f'  Slinky L2 by-state: hit={len(sl_l2)} miss={len(sl_sids) - len(sl_l2)}', flush=True)

    # ── LaserStream L2/L3 for the frozen state_ids ──
    ls_sids = {(c.get('provenance_ids') or {}).get('state_id')
               for _, r in ls_panels for c in r['candidates']}
    ls_sids.discard(None)
    l3_map, l2_map = {}, {}
    if ls_sids:
        l3_df = B.load_laserstream_l3(ls_sids)
        for _, row in l3_df.iterrows():
            l3_map.setdefault(row['state_id'], []).append(row.to_dict())
        l2_df = B.load_laserstream_l2(ls_sids)
        for _, row in l2_df.iterrows():
            l2_map[row['state_id']] = row.to_dict()

    # ── recompute panels ──
    out_records = []
    cf_hit = cf_miss = 0
    for i, r in cross:
        r = copy.deepcopy(r)
        for c in r['candidates']:
            if r.get('panel_source') == 'slinky':
                ok = recompute_slinky_candidate(c, cf_lookup, sl_l2)
            else:
                ok = recompute_ls_candidate(c, l3_map, l2_map)
            cf_hit += 1 if ok else 0
            cf_miss += 0 if ok else 1
        # re-rank by corrected utility
        r['candidates'].sort(
            key=lambda c: c['continuous_utility']['robust_executable_utility_v1'],
            reverse=True)
        for rank, c in enumerate(r['candidates']):
            c['panel_rank'] = rank + 1
        r['has_buy'] = any(V2.decide_v2(c) == 'BUY' for c in r['candidates'])
        r['no_buy_flag'] = not r['has_buy']

        # model-visible serialization (corrected v2 exporter)
        panel_key = r['id']
        export = V2.export_cross_v2(r, panel_key)
        export['token_count'] = X.estimate_tokens(json.dumps(export))
        r['export_v1_2'] = {
            'instruction': export['instruction'],
            'input': export['input'],
            'output': export['output'],
            'cross_mode': export['cross_mode'],
            'token_count': export['token_count'],
            'label_governance': export['label_governance'],
        }
        r['eval_version'] = 'v1.2'
        r['serializer'] = 'sft_cross_exporter_v2'
        out_records.append(('cross', r, None))
    print(f'  recomputed {len(cross)} panels; CF/L3 hit={cf_hit} miss={cf_miss}', flush=True)

    for i, r, raw in rust:
        out_records.append(('rust', r, raw))

    # ══ CERTIFICATION ══
    cert = {'checks': {}, 'failures': []}

    def check(name, ok, detail):
        cert['checks'][name] = {'pass': bool(ok), 'detail': detail}
        if not ok:
            cert['failures'].append(name)
        print(f'  [{"PASS" if ok else "FAIL"}] {name}: {detail}', flush=True)

    panels = [r for kind, r, _ in out_records if kind == 'cross']
    all_cands = [c for r in panels for c in r['candidates']]

    # 1. nonempty/unique mint_ids (unique within panel; nonempty everywhere)
    empty_hash = hashlib.sha256(b'').hexdigest()[:16]
    bad_mint = sum(1 for c in all_cands if not c.get('mint_id') or c['mint_id'] == empty_hash)
    dup_panels = sum(1 for r in panels
                     if len({c['mint_id'] for c in r['candidates']}) != len(r['candidates']))
    check('mint_ids_nonempty_unique', bad_mint == 0 and dup_panels == 0,
          f'{len(all_cands)} candidates, {bad_mint} empty/degenerate, {dup_panels} panels with dup mints')

    # 2. causal_state genuinely populated
    def causal_pop(c):
        cs = c.get('causal_state') or {}
        return sum(1 for v in cs.values() if v is not None and v == v)
    pops = [causal_pop(c) for c in all_cands]
    n_thin = sum(1 for p in pops if p < 10)
    check('causal_state_populated', n_thin == 0,
          f'min populated fields={min(pops)}, median={sorted(pops)[len(pops)//2]}, thin(<10)={n_thin}')

    # 3. corrected _bp -> return economics
    slinky_cands = [c for r in panels if r['panel_source'] == 'slinky' for c in r['candidates']]
    with_cf = [c for c in slinky_cands if c.get('l3_compressed')]
    nonnull_econ = [c for c in with_cf
                    if c['l3_compressed'].get('median_net_return_pct') is not None]
    frac_mag_ok = all(abs(c['l3_compressed']['median_net_return_pct']) < 10
                      for c in nonnull_econ)
    check('corrected_bp_economics', len(nonnull_econ) > 0 and frac_mag_ok,
          f'{len(with_cf)}/{len(slinky_cands)} slinky candidates have CF; '
          f'{len(nonnull_econ)} nonnull median_net_return_pct; fraction-scale ok={frac_mag_ok}')

    # 4. zero future L2/L3 fields in live inputs/rationales
    leak_fail = 0
    for r in panels:
        ex = r['export_v1_2']
        try:
            V2.assert_no_outcome_leakage(json.dumps(ex['input'], ensure_ascii=False), r['id'])
            V2.assert_no_outcome_leakage(ex['instruction'], r['id'])
            if ex['cross_mode'] == 'live_action':
                V2.assert_no_outcome_leakage(json.dumps(ex['output'], ensure_ascii=False), r['id'])
        except AssertionError as e:
            leak_fail += 1
            print(f'    LEAK: {e}', flush=True)
    check('zero_future_fields_in_live_text', leak_fail == 0, f'{leak_fail} leaking records')

    # 5. zero eval<->train overlap (vs qwen_sft_v2.jsonl)
    eval_mints = {c['mint_id'] for c in all_cands}
    eval_mints |= {c.get('mint_id_raw') for c in all_cands if c.get('mint_id_raw')}
    eval_sids = {(c.get('provenance_ids') or {}).get('state_id') for c in all_cands}
    eval_sids.discard(None)
    train_mints, train_sids, n_train = set(), set(), 0
    with open(SFT_V2_PATH, encoding='utf-8') as f:
        for line in f:
            if not line.strip():
                continue
            rec = json.loads(line)
            n_train += 1
            inp = rec.get('input')
            if isinstance(inp, dict) and 'candidates' in inp and 'fields' in inp:
                fi = inp['fields'].index('mint_id') if 'mint_id' in inp['fields'] else 0
                for row in inp['candidates']:
                    if isinstance(row, list) and row:
                        train_mints.add(row[fi])
            lg = rec.get('label_governance') or {}
            for m in (lg.get('per_candidate_evidence') or {}):
                train_mints.add(m)
            prov = rec.get('provenance') or {}
            for k, v in prov.items():
                if 'state_id' in str(k).lower() and isinstance(v, str):
                    train_sids.add(v)
    mint_overlap = eval_mints & train_mints
    sid_overlap = eval_sids & train_sids
    check('zero_eval_train_overlap', not mint_overlap and not sid_overlap,
          f'train records={n_train}; mint overlap={len(mint_overlap)}, state_id overlap={len(sid_overlap)}')

    # 6. no empty/default utility artifacts
    def util_bad(c):
        u = (c.get('continuous_utility') or {}).get('robust_executable_utility_v1')
        return u is None or (isinstance(u, float) and math.isnan(u))
    n_bad_util = sum(1 for c in all_cands if util_bad(c))
    utils = [c['continuous_utility']['robust_executable_utility_v1'] for c in all_cands
             if not util_bad(c)]
    n_distinct = len({round(u, 4) for u in utils})
    check('no_empty_utility_artifacts', n_bad_util == 0 and n_distinct > 10,
          f'NaN/None utilities={n_bad_util}; distinct utility values={n_distinct}; '
          f'range=[{min(utils):.4f},{max(utils):.4f}]')

    # 7. BUY/WATCH/SKIP/NO_BUY nondegenerate
    from collections import Counter
    dec = Counter(V2.decide_v2(c) for c in all_cands)
    panel_action = Counter(
        'NO_BUY' if r['export_v1_2']['output'].get('action') == 'NO_BUY'
        or r['export_v1_2']['output'].get('no_buy_flag') else 'BUY_PRESENT'
        for r in panels)
    nondegen = all(dec.get(k, 0) > 0 for k in ('BUY', 'WATCH', 'SKIP')) and \
        panel_action.get('NO_BUY', 0) > 0 and panel_action.get('BUY_PRESENT', 0) > 0
    check('decision_distributions_nondegenerate', nondegen,
          f'candidate decisions={dict(dec)}; panel actions={dict(panel_action)}')

    # 8. category counts
    cats = Counter(r.get('eval_category', 'unknown') for r in panels)
    cats['rust_repair'] = len(rust)
    cert['category_counts'] = dict(cats)
    print(f'  categories: {dict(cats)}', flush=True)

    mode_counts = Counter(r['export_v1_2']['cross_mode'] for r in panels)
    cert['mode_counts'] = dict(mode_counts)

    # ── write output: cross panels (recomputed) + rust (byte-identical) ──
    with open(V1_2_PATH, 'w', encoding='utf-8', newline='\n') as f:
        for kind, r, raw in out_records:
            f.write((raw if kind == 'rust' else json.dumps(r, ensure_ascii=False)) + '\n')

    sha_v12 = hashlib.sha256(open(V1_2_PATH, 'rb').read()).hexdigest()
    sha_v11 = hashlib.sha256(open(V1_1_PATH, 'rb').read()).hexdigest()
    tok_prov = json.load(open(TOK_PROV_PATH, encoding='utf-8'))

    manifest = {
        'eval_version': 'v1.2',
        'built_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
        'lineage': {
            'source': 'qwen_eval_v1_1.jsonl (frozen partition, byte-hash below)',
            'v1_1_sha256': sha_v11,
            'partition_reuse': 'same mints/state_ids/panels; zero resampling',
            'rust_repairs_carried_byte_identical': len(rust),
        },
        'serializer': 'sft_cross_exporter_v2 (corrected _bp->fraction economics)',
        'records': {'total': len(out_records), 'cross_panels': len(panels),
                    'rust_repairs': len(rust),
                    'slinky': len(slinky_panels), 'laserstream': len(ls_panels)},
        'certification': cert,
        'sha256': sha_v12,
        'tokenizer_provenance': tok_prov,
        'label_governance': {
            'action_label_version': V2.ACTION_LABEL_VERSION,
            'policy': 'future outcomes generate TARGET labels/rankings only; '
                      'never live evidence; loss weights task-identity only',
        },
    }
    with open(MANIFEST_PATH, 'w', encoding='utf-8', newline='\n') as f:
        json.dump(manifest, f, indent=1, ensure_ascii=False)

    status = 'CERTIFIED' if not cert['failures'] else f"FAILED: {cert['failures']}"
    print(f'\n=== qwen_eval_v1.2 {status} ===', flush=True)
    print(f'  {V1_2_PATH}\n  sha256={sha_v12}\n  ({time.time()-t0:.0f}s)', flush=True)
    return 0 if not cert['failures'] else 1


if __name__ == '__main__':
    sys.exit(main())
