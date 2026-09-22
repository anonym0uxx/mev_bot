#!/usr/bin/env python
"""C11 flow enrichment builder — LIVE FLOW STATE features, strictly as-of-t.

Single streaming pass over the time-sorted canonical tape. For every clock
(mint, t_dec_ms) in C11_CLOCKS.jsonl, emit 12 flow features computed ONLY from
tape events with recv_unix_ms < t. Clocks are served BEFORE the first event with
recv >= t is applied to state, which enforces causality mechanically.

Spec: /training/v2/reports/C11_SYNTHESIS_SPEC.md §3.
"""
import json
import collections
import time
import argparse

_ap = argparse.ArgumentParser()
_ap.add_argument('--clocks', default='/training/v2/reports/C11_CLOCKS.jsonl')
_ap.add_argument('--out', default='/training/v2/reports/C11_FLOW_ENRICHMENT.jsonl')
_args = _ap.parse_args()

TAPE = '/training/v2/canonical/renormalized_v7/trades.sorted.jsonl'  # time-sorted copy (raw tape has 9.57M out-of-order rows; verified 0 after sort)
LAUNCHES = '/training/v2/canonical/renormalized_v7/launches.jsonl'
CLOCKS = _args.clocks
OUT = _args.out

LAMPORTS = 1_000_000_000
W300 = 300_000
W60 = 60_000
FRESH_MS = 86_400_000
# Freshness lookback is BOUNDED at 7d so the definition is reproducible by a
# live collector with a rolling window (unbounded first-seen drifts as the live
# stream outgrows the 17.6d tape). fresh = wallet's first activity within the
# trailing LOOKBACK_MS occurred < FRESH_MS ago.
LOOKBACK_MS = 604_800_000
SMART_SOL = 5 * LAMPORTS
SMART_MINTS = 5
EARLY_N = 20          # first-N buyers of a mint form co-entry candidates
COENTRY_MIN = 2       # >=2 shared early co-entries -> linked pair
SNIPER_SLOTS = 2

# ---- clocks, grouped and globally time-sorted
clocks = []
clock_mints = set()
with open(CLOCKS) as fh:
    for line in fh:
        r = json.loads(line)
        clocks.append((int(r['t_dec_ms']), r['mint']))
        clock_mints.add(r['mint'])
clocks.sort()
n_clocks = len(clocks)
print(f"clocks: {n_clocks:,} | clock mints: {len(clock_mints):,}", flush=True)

# ---- creator per mint (for creator_trading_own_mint)
creator_of = {}
with open(LAUNCHES) as fh:
    for line in fh:
        r = json.loads(line)
        m = r.get('mint'); c = r.get('creator') or r.get('deployer') or r.get('user')
        if m and c:
            creator_of[m] = c
print(f"launches with creator: {len(creator_of):,}", flush=True)

# ---- global streaming state (updated only AFTER clocks at <= t are served)
first_seen = {}                                    # wallet -> rolling-window first-activity recv_ms (reset when idle > LOOKBACK_MS)
wallet_last = {}                                   # wallet -> last activity recv_ms
tape_t0 = [None]                                   # first tape event recv_ms (for lookback_d field)
extracted = collections.defaultdict(int)           # wallet -> cum sol_lamports (buys<0, sells>0)
wallet_mints = collections.defaultdict(set)        # wallet -> distinct mints traded
early_buyers = collections.defaultdict(list)       # mint -> first-N buyer list (ordered, unique)
copartners = collections.defaultdict(dict)         # wallet -> {partner: co-entry count}
mint_first_slot = {}                               # mint -> slot of first tape event
mint_snipers = collections.defaultdict(set)        # clock-mint -> wallets first-trading within SNIPER_SLOTS
mint_wallet_seen = collections.defaultdict(set)    # clock-mint -> wallets already seen (for sniper first-trade test)
creator_traded = set()                             # clock-mints whose creator traded them
windows = collections.defaultdict(collections.deque)  # clock-mint -> deque of (recv, trader, side, lamports, fee, cu)


def pct(sorted_vals, q):
    if not sorted_vals:
        return None
    i = min(len(sorted_vals) - 1, max(0, int(round(q * (len(sorted_vals) - 1)))))
    return sorted_vals[i]


def serve(t, mint, out_fh, stats):
    dq = windows.get(mint)
    if dq:
        while dq and dq[0][0] < t - W300:
            dq.popleft()
    events = [e for e in dq if e[0] < t] if dq else []
    rec = {"mint": mint, "t_dec_ms": t}
    if not events and mint not in mint_first_slot:
        rec["no_prior_flow"] = True
        stats["no_prior_flow"] += 1
        out_fh.write(json.dumps(rec) + "\n")
        return
    buys = [e for e in events if e[2] == 'buy']
    buyers300 = {}
    for e in buys:
        buyers300.setdefault(e[1], []).append(e)
    entrants = list(buyers300)
    entrants60 = {e[1] for e in buys if e[0] >= t - W60}
    net_flow = -sum(e[3] for e in events)  # buys negative spend, sells positive receive
    fresh = sum(1 for w in entrants if t - first_seen.get(w, t) < FRESH_MS)
    # observed lookback depth at decision time, capped at LOOKBACK_MS (days, 1dp);
    # rendered so the model conditions on warm-up states (short live lookback ≡ early tape)
    lb_ms = min(t - tape_t0[0], LOOKBACK_MS) if tape_t0[0] is not None else 0
    lb_d = round(lb_ms / 86_400_000, 1)
    snipers = mint_snipers.get(mint) or ()
    snip = sum(1 for w in entrants if w in snipers)
    amt_counts = collections.Counter(e[3] for e in buys)
    uni = sum(1 for e in buys if amt_counts[e[3]] >= 3)
    smart = [w for w in entrants
             if extracted[w] >= SMART_SOL and len(wallet_mints[w]) >= SMART_MINTS]
    smart_set = set(smart)
    smart_flow = -sum(e[3] for e in events if e[1] in smart_set)
    ent_set = set(entrants)
    coent = 0
    for w in entrants:
        cp = copartners.get(w)
        if cp and any(p in ent_set and c >= COENTRY_MIN for p, c in cp.items()):
            coent += 1
    fees = sorted(e[4] for e in buys)
    cus = sorted(e[5] for e in buys if e[5] is not None)
    rec.update({
        "entrants_60s": len(entrants60),
        "entrants_300s": len(entrants),
        "net_flow_sol_300s": round(net_flow / LAMPORTS, 6),
        "fresh_wallet_share_300s": round(fresh / len(entrants), 6) if entrants else None,
        "flow_lookback_d": lb_d,
        "sniper_share_300s": round(snip / len(entrants), 6) if entrants else None,
        "bot_uniform_share_300s": round(uni / len(buys), 6) if buys else None,
        "smart_entrants_300s": len(smart),
        "smart_net_flow_sol_300s": round(smart_flow / LAMPORTS, 6),
        "coentry_wallets_300s": coent,
        "creator_trading_own_mint": mint in creator_traded,
        "entrant_fee_p90_lamports": pct(fees, 0.90),
        "entrant_cu_p50": pct(cus, 0.50),
    })
    stats["served_with_flow"] += 1
    out_fh.write(json.dumps(rec) + "\n")


def main():
    stats = collections.Counter()
    ci = 0
    n = 0
    t0 = time.time()
    out_fh = open(OUT, 'w')
    with open(TAPE) as fh:
        for line in fh:
            e = json.loads(line)
            if e.get('status') != 'success':
                continue
            t = int(e['recv_unix_ms'])
            # serve all clocks strictly before/at this event's timestamp
            while ci < n_clocks and clocks[ci][0] <= t:
                serve(clocks[ci][0], clocks[ci][1], out_fh, stats)
                ci += 1
            mint = e['mint']; w = e['trader']; side = e['side']
            slot = e['slot']; lam = int(e['sol_lamports'])
            # ---- global wallet state
            if w not in first_seen or t - wallet_last.get(w, t) > LOOKBACK_MS:
                first_seen[w] = t   # rolling-window first activity (idle>7d ⇒ fresh again)
            wallet_last[w] = t
            if tape_t0[0] is None:
                tape_t0[0] = t
            extracted[w] += lam
            wallet_mints[w].add(mint)
            if mint not in mint_first_slot:
                mint_first_slot[mint] = slot
            # ---- early-buyer co-entry graph (all mints)
            if side == 'buy':
                eb = early_buyers[mint]
                if len(eb) < EARLY_N and w not in eb:
                    for other in eb:
                        copartners[w][other] = copartners[w].get(other, 0) + 1
                        copartners[other][w] = copartners[other].get(w, 0) + 1
                    eb.append(w)
            # ---- clock-mint-only state
            if mint in clock_mints:
                if w not in mint_wallet_seen[mint]:
                    mint_wallet_seen[mint].add(w)
                    if slot <= mint_first_slot[mint] + SNIPER_SLOTS:
                        mint_snipers[mint].add(w)
                if creator_of.get(mint) == w:
                    creator_traded.add(mint)
                dq = windows[mint]
                dq.append((t, w, side, lam, int(e.get('fee_lamports') or 0),
                           e.get('cu_consumed')))
                while dq and dq[0][0] < t - W300:
                    dq.popleft()
            n += 1
            if n % 2_000_000 == 0:
                print(f"...{n:,} events, {ci:,}/{n_clocks:,} clocks, "
                      f"{time.time()-t0:.0f}s", flush=True)
    # clocks after the last tape event
    while ci < n_clocks:
        serve(clocks[ci][0], clocks[ci][1], out_fh, stats)
        ci += 1
    out_fh.close()
    stats["wallets"] = len(first_seen)
    stats["clock_mints_with_flow"] = len(windows)
    print(json.dumps({"events": n, "clocks_served": ci, **stats,
                      "elapsed_s": round(time.time() - t0, 1)}), flush=True)


if __name__ == '__main__':
    main()
