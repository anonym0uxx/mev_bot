#!/usr/bin/env python3
"""Measure the yield of a predeclared decision clock over a capture.

Purpose: convert "we might be able to build judgement examples" into a number.
This is a YIELD MEASUREMENT, not a corpus builder. It emits no training data.

Design (predeclared, action-independent):
  * clock: fixed interval anchored to capture start, sampled per mint;
  * a decision point exists at tick t if the mint had >= MIN_PRIOR trailing
    swaps strictly before t (so the state is non-degenerate);
  * state uses only events strictly before t (causal cutoff);
  * outcome attaches only the forward price change, used as a LABEL;
  * a tick is a NEGATIVE if no swap we made occurred in the forward window --
    here our wallet is absent from the capture entirely, so every tick is an
    observed-and-did-not-act point.

  python3 decision_clock_yield.py --session <dir> --out <dir> [--interval 30]
"""
import argparse
import json
import subprocess
from collections import defaultdict
from pathlib import Path

LAMPORTS = 1_000_000_000


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--session', type=Path, required=True)
    ap.add_argument('--out', type=Path, default=Path('/training/v2/reports'))
    ap.add_argument('--interval', type=int, default=30, help='clock seconds')
    ap.add_argument('--min-prior', type=int, default=20, help='min trailing swaps')
    ap.add_argument('--min-events', type=int, default=100, help='mint eligibility')
    a = ap.parse_args()

    ev = next(a.session.glob('*events_v1_*.ndjson.zst'))

    # ---- pass 1: per-mint swap timeline ----
    t0 = None
    series = defaultdict(list)
    n = 0
    p = subprocess.Popen(['zstd', '-dc', str(ev)], stdout=subprocess.PIPE)
    for line in p.stdout:
        try:
            o = json.loads(line)
        except Exception:
            continue
        n += 1
        ts = o.get('recv_unix_ms')
        mint = o.get('mint_b58')
        if not ts or not mint:
            continue
        ai, ao = o.get('amount_in'), o.get('amount_out')
        side = o.get('trade_side') or o.get('event_type')
        # Amounts are single-sided: a sell carries the token amount in
        # `amount_in`, a buy carries it in `amount_out`. The SOL leg is never a
        # scalar -- it comes from the trader's own balance delta.
        tok = ai if side == 'sell' else (ao if side == 'buy' else None)
        if not tok:
            continue
        # `pre/post_sol_balances` are indexed over account_keys PLUS any
        # address-table-lookup addresses, so they are routinely longer than
        # account_keys_b58. Align on the combined key list, and fall back to the
        # swap's own SOL bound when the trader still cannot be located.
        ak = list(o.get('account_keys_b58') or [])
        ak += list(o.get('loaded_writable_addresses_b58') or [])
        ak += list(o.get('loaded_readonly_addresses_b58') or [])
        pre = o.get('pre_sol_balances') or []
        post = o.get('post_sol_balances') or []
        trader = o.get('trader_b58')
        sol = None
        if trader and len(pre) == len(post) and trader in ak:
            i = ak.index(trader)
            if i < len(pre):
                sol = post[i] - pre[i]
        if not sol:
            sol = o.get('min_amount_out') if side == 'sell' else o.get('max_amount_in')
        if not sol:
            continue
        try:
            price = abs(float(sol)) / float(tok)          # SOL per raw token unit
        except ZeroDivisionError:
            continue
        series[mint].append((ts, price, abs(int(sol)), 1 if side == 'buy' else 0))
        if t0 is None or ts < t0:
            t0 = ts
    p.stdout.close()
    p.wait()

    eligible = {m: v for m, v in series.items() if len(v) >= a.min_events}
    step = a.interval * 1000

    # ---- pass 2: clock sampling ----
    total_ticks = 0
    ticks_with_state = 0
    mints_contributing = 0
    horizon_ok = 0
    per_horizon = {'h30s': 30_000, 'h5m': 300_000, 'h30m': 1_800_000}
    horizon_ok_by = {k: 0 for k in per_horizon}
    price_move_stats = {k: [] for k in per_horizon}
    mint_tick_counts = {}

    for mint, evs in eligible.items():
        evs.sort(key=lambda x: x[0])
        first, last = evs[0][0], evs[-1][0]
        idx = 0
        prior = []
        ticks = 0
        tick = t0 + ((first - t0) // step) * step
        while tick <= last:
            while idx < len(evs) and evs[idx][0] < tick:
                prior.append(evs[idx])
                idx += 1
            if len(prior) >= a.min_prior:
                ticks += 1
                ticks_with_state += 1
                cur = prior[-1][1]
                for hname, hms in per_horizon.items():
                    # forward window strictly after tick
                    fut = None
                    j = idx
                    while j < len(evs) and evs[j][0] <= tick + hms:
                        fut = evs[j][1]
                        j += 1
                    if fut is not None and cur:
                        horizon_ok_by[hname] += 1
                        price_move_stats[hname].append((fut - cur) / cur)
            total_ticks += 1
            tick += step
        if ticks:
            mints_contributing += 1
            mint_tick_counts[mint] = ticks

    def summarize(xs):
        if not xs:
            return None
        xs2 = sorted(xs)
        n_ = len(xs2)
        return {
            'n': n_,
            'mean': sum(xs2) / n_,
            'p10': xs2[int(0.10 * n_)],
            'p50': xs2[n_ // 2],
            'p90': xs2[int(0.90 * n_)],
            'share_up': sum(1 for x in xs2 if x > 0) / n_,
        }

    report = {
        'schema': 'north_star_decision_clock_yield_v1',
        'session': a.session.name,
        'events_scanned': n,
        'mints_total': len(series),
        'mints_eligible_ge_min_events': len(eligible),
        'min_events': a.min_events,
        'interval_seconds': a.interval,
        'min_prior_swaps': a.min_prior,
        'clock_ticks_total': total_ticks,
        'decision_points_with_state': ticks_with_state,
        'mints_contributing': mints_contributing,
        'horizon_observed': horizon_ok_by,
        'forward_price_move_summary': {k: summarize(v) for k, v in price_move_stats.items()},
        'note': ('Causal cutoff enforced: state uses events strictly before the tick, outcome '
                 'strictly after. Our wallet had zero capture events, so no tick can be '
                 'positive-by-construction; every tick is an observed non-action point.'),
    }
    a.out.mkdir(parents=True, exist_ok=True)
    out = a.out / f'DECISION_CLOCK_YIELD_{a.session.name}.json'
    out.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({k: v for k, v in report.items() if k != 'forward_price_move_summary'}, indent=2))
    print('forward moves:', json.dumps(report['forward_price_move_summary'], indent=1))


if __name__ == '__main__':
    main()