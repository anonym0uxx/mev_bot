#!/usr/bin/env python3
"""Stage 1 source census for North Star SFT v2.

Read-only. Answers the feasibility question the plan gates on:

  * which sources exist, their rights status and coverage;
  * whether trustworthy decision labels can be constructed NOW, or whether the
    gap requires prospective collection;
  * the actual negative/untraded opportunity population;
  * execution evidence needed for modeled action values;
  * censoring and outcome-completeness status.

Existence of a column proves nothing. This reports measured coverage, and where
coverage is absent it reports the gap rather than inferring a repaired truth.

Usage:
  python3 source_inventory.py --out /training/v2/reports
"""
import argparse
import json
import sqlite3
from collections import Counter
from pathlib import Path

E = Path('/mnt/data/mev_bot-artifacts/north_star/aggregation/north_star_training_dataset_v1/'
         'corpus_builder_v1/integrated_data_v1/full_history_closure_v1/economics_v1')

SOURCES = {
    'labels':     E / 'labels.sqlite',
    'roundtrips': E / 'roundtrips.sqlite',
    'economics':  E / 'economics.sqlite',
    'context':    E / 'context.sqlite',
    'prices':     E / 'prices.sqlite',
    'extract':    E / 'extract.sqlite',
    'anchor_econ': E / 'astra_review_v1/repair_v2/economics/ANCHOR_ECONOMICS.sqlite',
    'recovered':  E / 'astra_review_v1/repair_v2/recovered/RECOVERED_BALANCES.sqlite',
}


def connect(path):
    """Immutable read-only connection; never mutates the source."""
    if not path.is_file():
        return None
    return sqlite3.connect(f'file:{path}?immutable=1', uri=True)


def dist(db, sql, limit=25):
    try:
        rows = db.execute(sql).fetchall()
    except sqlite3.Error as exc:
        return {'error': str(exc)}
    total = sum(n for _, n in rows if n is not None)
    out = {}
    for k, n in rows[:limit]:
        out[str(k)] = n
    return {'total': total, 'values': out}


def one(db, sql):
    try:
        return db.execute(sql).fetchone()
    except sqlite3.Error as exc:
        return (f'ERROR: {exc}',)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--out', type=Path, default=Path('/training/v2/reports'))
    a = ap.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)

    report = {'schema': 'north_star_v2_source_census_v1', 'sources': {}}

    # ---------- availability ----------
    for name, p in SOURCES.items():
        report['sources'][name] = {
            'path': str(p),
            'exists': p.is_file(),
            'bytes': p.stat().st_size if p.is_file() else None,
        }

    # ---------- labels: the decision/outcome population ----------
    db = connect(SOURCES['labels'])
    if db:
        L = report['sources']['labels']
        L['examples_total'] = one(db, 'select count(*) from examples')[0]
        L['admitted'] = dist(db, 'select admitted, count(*) from examples group by admitted')
        L['split'] = dist(db, 'select split, count(*) from examples group by split')
        L['action_label'] = dist(db, 'select action_label, count(*) from examples group by action_label')
        L['group_outcome'] = dist(db, 'select group_outcome, count(*) from examples group by group_outcome')
        L['outcome_label'] = dist(db, 'select outcome_label, count(*) from examples group by outcome_label')
        # outcome coverage: how many rows actually carry each outcome family
        cov = {}
        for col in ('realized_lamports', 'outcome_label', 'rt', 'hold_seconds', 'episode_return_bp',
                    'mfe_bp', 'mae_bp', 'exit_quality', 'post_exit_max_bp',
                    'h30s_bp', 'h5m_bp', 'h30m_bp', 'h1h_bp'):
            try:
                n = db.execute(f'select count(*) from examples where {col} is not null').fetchone()[0]
            except sqlite3.Error:
                n = None
            cov[col] = n
        L['outcome_column_coverage'] = cov
        L['label_gate'] = [dict(gate=g, satisfied=s, detail=d) for g, s, d in
                           db.execute('select gate, satisfied, detail from label_gate').fetchall()]
        db.close()

    # ---------- roundtrips: horizons and excursions ----------
    db = connect(SOURCES['roundtrips'])
    if db:
        R = report['sources']['roundtrips']
        R['round_trips'] = one(db, 'select count(*) from round_trips')[0]
        R['horizon_returns'] = one(db, 'select count(*) from horizon_returns')[0]
        R['horizons'] = dist(db, 'select horizon, count(*) from horizon_returns group by horizon')
        R['horizon_null_ret'] = one(db, 'select count(*) from horizon_returns where ret_bp is null')[0]
        R['roundtrip_outcome_coverage'] = {
            col: one(db, f'select count(*) from round_trips where {col} is not null')[0]
            for col in ('pnl_lamports', 'return_bp', 'mfe_bp', 'mae_bp', 'exit_quality',
                        'post_exit_max_bp', 'tokens_raw', 'hold_seconds')
        }
        R['edge_balance'] = [dict(kind=k, value=v, n=n) for k, v, n in
                             db.execute('select kind, value, n from edge_balance').fetchall()]
        db.close()

    # ---------- group economics: censoring and reconciliation ----------
    db = connect(SOURCES['economics'])
    if db:
        G = report['sources']['economics']
        G['group_economics_total'] = one(db, 'select count(*) from group_economics')[0]
        G['outcome_status'] = dist(db, 'select outcome_status, count(*) from group_economics group by outcome_status')
        G['outcome_basis'] = dist(db, 'select outcome_basis, count(*) from group_economics group by outcome_basis')
        G['position_open'] = dist(db, 'select position_open, count(*) from group_economics group by position_open')
        G['left_censored'] = dist(db, 'select left_censored, count(*) from group_economics group by left_censored')
        G['has_transfer_out'] = dist(db, 'select has_transfer_out, count(*) from group_economics group by has_transfer_out')
        G['realized_coverage'] = one(db, 'select count(*) from group_economics where realized_lamports is not null')[0]
        G['row_labels'] = one(db, 'select count(*) from row_labels')[0]
        G['label_gate'] = [dict(gate=g, satisfied=s, detail=d) for g, s, d in
                           db.execute('select gate, satisfied, detail from label_gate').fetchall()]
        db.close()

    # ---------- execution evidence ----------
    db = connect(SOURCES['context'])
    if db:
        C = report['sources']['context']
        C['decision_context'] = one(db, 'select count(*) from decision_context')[0]
        C['context_status'] = dist(db, 'select context_status, count(*) from decision_context group by context_status')
        C['executable'] = one(db, 'select count(*) from executable')[0]
        C['lifecycle'] = dist(db, 'select lifecycle, count(*) from executable group by lifecycle')
        C['err'] = dist(db, 'select err, count(*) from executable group by err')
        C['priority_fee_coverage'] = one(db, 'select count(*) from executable where priority_fee_lamports is not null')[0]
        db.close()

    # ---------- price observations ----------
    db = connect(SOURCES['prices'])
    if db:
        P = report['sources']['prices']
        P['price_points'] = one(db, 'select count(*) from price_points')[0]
        P['distinct_mints'] = one(db, 'select count(distinct mint) from price_points')[0]
        P['distinct_wallets'] = one(db, 'select count(distinct wallet) from price_points')[0]
        P['time_span'] = one(db, 'select min(block_time), max(block_time) from price_points')
        P['points_per_mint'] = one(db, 'select min(c), avg(c), max(c) from '
                                       '(select count(*) c from price_points group by mint)')
        P['price_semantics'] = ('wallet trade observations (sol_lamports, tokens_raw) at signature '
                                'level -- NOT a continuous market price series')
        db.close()

    # ---------- negative / untraded opportunity population ----------
    report['population_findings'] = {
        'decision_population_available': 'observed_action_events_only',
        'untraded_or_no_action_opportunities_in_sources': 'NOT PRESENT as a sampled population',
        'note': ('Every decision row in the v1 family exists because a tracked wallet acted, and '
                 'closed-episode reviews exist because an episode closed. Neither is a live decision '
                 'stream, so WATCH/SKIP/no-entry states cannot be learned or evaluated from these '
                 'sources as they stand.'),
    }

    (a.out / 'SOURCE_RIGHTS_COVERAGE.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({'written': str(a.out / 'SOURCE_RIGHTS_COVERAGE.json'),
                      'sources_present': sum(1 for v in report['sources'].values() if v['exists']),
                      'sources_total': len(report['sources'])}, indent=2))
    return report


if __name__ == '__main__':
    main()
