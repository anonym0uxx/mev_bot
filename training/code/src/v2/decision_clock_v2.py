#!/usr/bin/env python3
"""Decision clock over RE-NORMALIZED trades (corrected SOL/token legs).

The earlier clock ran over the stale encoder's events, where the SOL leg was
recovered wrongly 72% of the time, so its outcome statistics are void. This
builds the causal decision population from `trades.jsonl` instead.

Contract:
  * Predeclared clock: fixed interval per mint, anchored to capture start.
    The clock does NOT depend on any wallet's action -- that is the whole point.
  * Causal cutoff: state at tick t uses only trades strictly before t; the
    forward move uses only trades strictly after t.
  * Both legs are balance deltas, so price = |sol_lamports| / |tokens_raw|.

Output: decision points (JSONL) + a summary JSON.
"""
import json
import sys
from collections import defaultdict
from pathlib import Path

INTERVAL_S = 30
MIN_PRIOR = 20
# A trade whose token leg is below this many raw units is dust or a
# quantisation residual; its implied price is meaningless.
MIN_TOK = 1000
# Reject observations whose price is more than this factor from the mint's own
# median. Wide on purpose: real memecoins can 1000x. Degenerate points sit at
# 1e5-1e11x, so the band removes only the impossible.
PRICE_BAND = 10000.0
HORIZONS_S = (30, 300, 1800)


def load_trades(path):
    """mint -> sorted [(ts_ms, price_sol_per_raw, sol_lamports, is_buy, trader)]"""
    series = defaultdict(list)
    n = bad = 0
    with open(path) as fh:
        for line in fh:
            try:
                r = json.loads(line)
            except Exception:
                continue
            sol, tok = r.get('sol_lamports'), r.get('tokens_raw')
            ts = r.get('recv_unix_ms')
            if not sol or not tok or ts is None:
                bad += 1
                continue
            # A token leg of a few raw units carries no price information:
            # price = sol/token explodes toward 1e6 and poisons the whole
            # series. These are dust/quantisation residuals, not trades.
            if abs(float(tok)) < MIN_TOK:
                bad += 1
                continue
            n += 1
            series[r['mint']].append((
                ts,
                abs(float(sol)) / abs(float(tok)),
                abs(int(sol)),
                1 if r.get('side') == 'buy' else 0,
            ))
    # Per-mint robust filter: drop observations far outside the mint's own
    # median price. The band is deliberately wide (a memecoin can genuinely
    # 1000x) so it removes only physically impossible points.
    removed = 0
    for m in list(series):
        v = series[m]
        if len(v) < MIN_PRIOR:
            del series[m]
            continue
        pxs = sorted(x[1] for x in v)
        med = pxs[len(pxs) // 2]
        if med <= 0:
            del series[m]
            continue
        kept = [x for x in v if 1.0 / PRICE_BAND <= (x[1] / med) <= PRICE_BAND]
        removed += len(v) - len(kept)
        if len(kept) < MIN_PRIOR:
            del series[m]
        else:
            series[m] = kept
    for m in series:
        series[m].sort(key=lambda x: x[0])
    print(f'[clock] dust-filtered/outlier-filtered trades: {removed}', flush=True)
    return series, n, bad


def main():
    trades_path = Path(sys.argv[1])
    out_dir = Path(sys.argv[2])
    out_dir.mkdir(parents=True, exist_ok=True)

    series, n, bad = load_trades(trades_path)
    total_trades = sum(len(v) for v in series.values())

    dp_path = out_dir / 'decision_points.jsonl'
    n_dp = 0
    n_mints = 0
    fwd = {h: [] for h in HORIZONS_S}

    with dp_path.open('w') as out:
        for mint, pts in series.items():
            if len(pts) < MIN_PRIOR:
                continue
            n_mints += 1
            t0 = pts[0][0]
            # Predeclared grid, independent of any action.
            tick = t0 + INTERVAL_S * 1000
            i = 0                      # index of first trade strictly before tick
            prior = 0
            while i < len(pts) and tick <= pts[-1][0]:
                while i < len(pts) and pts[i][0] < tick:
                    i += 1
                    prior += 1
                if prior < MIN_PRIOR:
                    tick += INTERVAL_S * 1000
                    continue

                window = pts[max(0, i - prior):i]
                px = window[-1][1]
                buys = sum(w[3] for w in window)
                vol = sum(w[2] for w in window)

                # Forward outcomes strictly after the tick.
                row = {
                    'mint': mint, 'tick_ms': tick,
                    'n_prior': prior, 'price': px,
                    'prior_buy_share': buys / len(window),
                    'prior_sol_volume': vol,
                    'prior_span_s': (window[-1][0] - window[0][0]) / 1000.0,
                }
                for h in HORIZONS_S:
                    # Last observation carried forward: the last trade at or
                    # before the horizon. Taking the "next trade" instead makes
                    # h30/h300/h1800 collapse to the same observation whenever
                    # the mint is illiquid, which is most of them.
                    tgt = tick + h * 1000
                    j = i
                    last = None
                    while j < len(pts) and pts[j][0] <= tgt:
                        last = pts[j][1]
                        j += 1
                    row[f'h{h}_move'] = (
                        (last / px - 1.0) if last is not None and px > 0 else None
                    )
                    if row[f'h{h}_move'] is not None:
                        fwd[h].append(row[f'h{h}_move'])
                out.write(json.dumps(row) + '\n')
                n_dp += 1
                tick += INTERVAL_S * 1000

    def q(vals, p):
        if not vals:
            return None
        s = sorted(vals)
        return s[min(len(s) - 1, int(p * len(s)))]

    summary = {
        'schema': 'north_star_decision_clock_v2',
        'trades_read': total_trades,
        'trades_skipped_no_legs': bad,
        'mints_with_series': len(series),
        'mints_eligible': n_mints,
        'interval_seconds': INTERVAL_S,
        'min_prior_trades': MIN_PRIOR,
        'decision_points': n_dp,
        'forward_moves': {
            f'h{h}s': {
                'n': len(fwd[h]),
                'p10': q(fwd[h], 0.10), 'p50': q(fwd[h], 0.50),
                'p90': q(fwd[h], 0.90), 'p99': q(fwd[h], 0.99),
                'share_up': (sum(1 for v in fwd[h] if v > 0) / len(fwd[h])) if fwd[h] else None,
            } for h in HORIZONS_S
        },
    }
    (out_dir / 'decision_clock_v2_summary.json').write_text(
        json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2))


if __name__ == '__main__':
    main()
