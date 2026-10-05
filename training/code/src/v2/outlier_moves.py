#!/usr/bin/env python3
"""Are the extreme forward moves real, or residual price corruption?

p99 of the 30s forward move came back at ~3,711x. A memecoin can 100x on a
curve, but not in 30 seconds. This prints the largest moves alongside the raw
trades that produced them, so the cause is visible rather than assumed.

Usage: outlier_moves.py <trades.jsonl> [top_n]
"""
import json
import sys
from collections import defaultdict


def token_ok(t):
    return t and t.get('sol_lamports') and t.get('tokens_raw')


def main():
    path = sys.argv[1]
    top_n = int(sys.argv[2]) if len(sys.argv) > 2 else 12

    series = defaultdict(list)
    with open(path) as f:
        for line in f:
            try:
                t = json.loads(line)
            except Exception:
                continue
            if t.get('status') != 'success':
                continue
            sol, tok = t.get('sol_lamports'), t.get('tokens_raw')
            if not sol or not tok:
                continue
            # Same dust filter the clock applies: a token leg of a few raw
            # units is a quantisation residual whose implied price is garbage.
            if abs(tok) < 1000:
                continue
            px = abs(sol) / abs(tok)
            if px <= 0:
                continue
            series[t['mint']].append(
                (t['recv_unix_ms'], px, abs(sol), abs(tok), t['side'], t.get('venue'))
            )

    rows = []
    for mint, pts in series.items():
        pts.sort(key=lambda r: r[0])
        for i, (ts, px, sol, tok, side, venue) in enumerate(pts):
            if i < 20:
                continue
            for h in (30_000,):
                last = None
                for j in range(i, len(pts)):
                    if pts[j][0] <= ts + h:
                        last = pts[j]
                    else:
                        break
                if last is None:
                    continue
                mv = last[1] / px
                if mv > 50:
                    rows.append({
                        'move': mv, 'mint': mint, 'at': px,
                        'sol': sol, 'tok': tok, 'side': side, 'venue': venue,
                        'later_px': last[1], 'later_sol': last[2],
                        'later_tok': last[3], 'later_venue': last[5],
                    })
    rows.sort(key=lambda r: -r['move'])
    print(f'{len(rows)} moves > 50x at h30s\n')
    for r in rows[:top_n]:
        print(json.dumps(r, indent=2))
        print('-' * 60)

    # Distribution by venue pair -- venue switching is a prime suspect.
    pairs = defaultdict(int)
    for r in rows:
        pairs[(r['venue'], r['later_venue'])] += 1
    print('\nvenue pairs among >50x movers:', dict(pairs))


if __name__ == '__main__':
    main()
