#!/usr/bin/env python3
"""Episode outcome pricer v2 — EXACT frozen-engine policy (PROD_HOLD_V16 + independent Rust AMM protection).
Earlier v1 named a "50 % hard stop": that was a FIXTURE value, not the engine; v2 matches the engine (constants read
from the frozen source, cited inline):
  entry   : order at decision clock t0; lands on the first canonical WSOL AmmSwap with recv >= t0+400 ms
            (MODEL_FILL_LANDING_MS) and recv <= t0+5,000 ms (MODEL_ORDER_TTL_MS); fill = pumpswap_event::buy_exact_quote_in
            on THAT swap's pre-trade reserves + its own virtual_quote with ITS fee parts (ceil per component on the net
            input); entry price = ceil(clip*1e9/tokens) (all-in, model_admit.rs:1545); entry cost = clip + 10,000 fixed
            + 2,039,280 ATA rent (model_admit.rs:1627-1634). No landing → `entry_not_landed` (the engine's
            fill_none:no_landing_state). Landing swap without vq/fee parts → `entry_economics_not_on_landing_event`.
  protect : on every LATER swap with a verified vq (recv >= fill_ms; equal clocks accepted, older skipped), spot =
            (quote_pre+vq)/token_pre; hard stop if spot <= entry*(1-0.35) (lc_hard_sl_bps=3500) else rug precursor if
            prev>0 and (prev-spot)/prev >= 0.30 (lc_precursor_drop_bps=3000); hard stop checked first; fires ONCE; a
            state without vq is unavailable for protection and does not move `prev` (model_admit.rs:491-540, engine.rs).
  barrier : PROD_HOLD_V16 barrier 1,800,000 ms after the FILL (fill_ms, not decision time).
  exit    : standing intent at the trigger time; order lands on the first swap with recv >= intent+400 ms and recv >
            intent, carrying a vq (econ on the landing event) within MODEL_AMM_EXIT_TTL_MS=60,000; else the order
            expires and is retried when NEW state arrives (the swap that triggers the retry cannot be the landing: the
            new order needs a later swap) or on the backoff schedule 30 s*2^min(n-1,5) capped 15 min
            (model_manage.rs:296-360). Exit price = sell_gross_quote_out / tokens; proceeds = gross*(1-0.0095)
            (position::realize fallback venue fee VENUE_FEE_BPS_CURVE=95 — the engine does NOT apply the protocol's
            125 bps sell fee; recorded as an engine convention) - 10,000 fixed; ATA close refunds 2,039,280-5,000 on a
            full exit (engine.rs:5826-5830). NO RPC stuck-exit snapshots (research-004 is zero-RPC); the paper books HAD
            them — difference disclosed.
  unresolved: inventory kept at cost; classified by WHY no landing was observed, using the pool-lane journal:
            `capture_end_censored` (needed horizon beyond capture end), `pool_lane_expired_idle` (unsubscribed after
            idle TTL → absence ≠ no trading), `pool_lane_dropped_cap`, `no_landing_swap_observed_while_subscribed`
            (genuinely observed lack of trading up to the follow-up cutoff), `vq_unverified_on_all_candidate_states`.
            A valuation SENSITIVITY (inventory marked on the last verified state, labelled) is reported separately and
            is never the outcome. Virtual quote is a pricing input, never spendable liquidity (vault cap).
Short-horizon markouts (30 s / 120 s, decision-time clip) remain DIAGNOSTICS and are provided by episode_pricer.py.
"""
import json, bisect, collections
LANDING_MS = 400; ENTRY_TTL_MS = 5_000; EXIT_TTL_MS = 60_000; BARRIER_MS = 1_800_000
HARD_SL_BPS = 3_500; PRECURSOR_BPS = 3_000; FIXED = 10_000; ATA_RENT = 2_039_280; ATA_CLOSE = 5_000
EXIT_VENUE_FEE_BPS = 95  # engine convention (fallback tier); protocol sell fee is reported as a sensitivity
PROTOCOL_SELL_FEE_BPS_SENS = 125

def ceil_div(a, b): return -(-a // b)

def buy_exact_quote_in(base, vault, vq, gross_in, lp, pr, cr):
    """Port of pump_quant_protocol::pumpswap_event::buy_exact_quote_in (binary search for the net input whose
    ceil-per-component fees fit inside gross_in; out = base*(net-1)/(eff+net-1))."""
    def fee(n): return n + ceil_div(n * lp, 10_000) + ceil_div(n * pr, 10_000) + ceil_div(n * cr, 10_000)
    if gross_in == 0: return None
    lo, hi = 0, gross_in
    while lo < hi:
        mid = lo + ceil_div(hi - lo, 2)
        if fee(mid) <= gross_in: lo = mid
        else: hi = mid - 1
    net = lo
    if net < 2: return None
    eff = vault + vq
    if eff <= 0: return None
    n1 = net - 1
    out = base * n1 // (eff + n1)
    return (net, out) if out > 0 else None

def sell_gross_quote_out(base, vault, vq, base_in):
    eff = vault + vq
    if eff <= 0 or base_in <= 0: return None
    return eff * base_in // (base + base_in)

class Swap:
    __slots__ = ('t', 'q', 'b', 'vq', 'fee_parts', 'ok')
    def __init__(self, t, q, b, vq, fee_parts, ok): self.t, self.q, self.b, self.vq, self.fee_parts, self.ok = t, q, b, vq, fee_parts, ok

def load_tape(path, mints=None):
    tape = collections.defaultdict(list)
    with open(path) as fh:
        for l in fh:
            if '"AmmSwap"' not in l: continue
            d = json.loads(l)
            if mints is not None and d['mint'] not in mints: continue
            f = d['fields']
            if 'recv_unix_ms' not in f: continue
            vq = f.get('virtual_quote', 'absent'); vq = None if vq in ('absent', None) else int(vq)
            fp = f.get('fee_parts'); fp = tuple(fp) if fp else None
            tape[d['mint']].append(Swap(f['recv_unix_ms'], int(f['quote_reserve_pre']), int(f['token_reserve_pre']), vq, fp,
                                        bool(f.get('pool_is_canonical', True)) and bool(f.get('quote_is_wsol', True))))
    for m in tape: tape[m].sort(key=lambda s: s.t)
    return tape

def load_pool_lane(path):
    """mint_b58 -> list of (unix_ms, ev) with ev in {request,set_sent?,expire,dropped_cap}; only mint-bearing records."""
    out = collections.defaultdict(list)
    with open(path) as fh:
        for l in fh:
            d = json.loads(l)
            if 'mint_b58' in d: out[d['mint_b58']].append((d['unix_ms'], d['ev'], d.get('why')))
    for m in out: out[m].sort()
    return out

def _first_in(rows, lo, hi, need_vq=True, strict_after=None):
    ts = [s.t for s in rows]; i = bisect.bisect_left(ts, lo)
    while i < len(rows) and rows[i].t <= hi:
        s = rows[i]
        if (strict_after is None or s.t > strict_after) and (not need_vq or s.vq is not None): return s
        i += 1
    return None

def price_episode(rows, t0, clip, capture_end_ms, lane_events=(), followup_cutoff_ms=None):
    """Returns dict with status in {closed, unresolved:<why>, unavailable:<why>} and the accounting."""
    rows = [s for s in rows if s.ok]
    res = {'t0': t0, 'clip': clip}
    if not rows: res['status'] = 'unavailable:noncanonical_or_non_wsol_only'; return res
    # entry landing
    land = _first_in(rows, t0 + LANDING_MS, t0 + ENTRY_TTL_MS, need_vq=False, strict_after=t0)
    if land is None: res['status'] = 'unavailable:entry_not_landed'; return res
    if land.vq is None or land.fee_parts is None: res['status'] = 'unavailable:entry_economics_not_on_landing_event'; return res
    fill = buy_exact_quote_in(land.b, land.q, land.vq, clip, *land.fee_parts)
    if fill is None: res['status'] = 'unavailable:entry_unpriceable'; return res
    net_in, tokens = fill
    entry_px = ceil_div(clip * 10**9, tokens)
    fill_ms = land.t
    entry_cost = clip + FIXED + ATA_RENT
    res.update(fill_ms=fill_ms, tokens=tokens, entry_px_fp=entry_px, entry_cost=entry_cost, entry_vault=land.q, entry_vq=land.vq, entry_fee_parts=land.fee_parts)
    # protection walk (fresh states only, after the fill)
    hard_level = entry_px * (10_000 - HARD_SL_BPS) // 10_000
    prev = 0; last_state_ms = fill_ms; intent_t = None; reason = None; unverified_states = 0
    for s in rows:
        if s.t < fill_ms or s.t < last_state_ms: continue
        if s.t >= fill_ms + BARRIER_MS: break
        if s.vq is None: unverified_states += 1; continue
        eff = s.q + s.vq
        if eff <= 0: unverified_states += 1; continue
        last_state_ms = s.t
        spot = eff * 10**9 // s.b
        if spot <= hard_level: intent_t, reason = s.t, 'amm_hard_stop'; break
        if prev > 0 and spot < prev and (prev - spot) * 10_000 // prev >= PRECURSOR_BPS: intent_t, reason = s.t, 'amm_rug_precursor'; break
        prev = spot
    if intent_t is None: intent_t, reason = fill_ms + BARRIER_MS, 'barrier'
    res.update(exit_intent_ms=intent_t, exit_reason=reason, protection_unverified_states_skipped=unverified_states)
    # standing exit: attempts until a landing or the end of observation
    horizon_end = followup_cutoff_ms if followup_cutoff_ms is not None else capture_end_ms
    attempts = 1; order_t = intent_t; landing = None
    while True:
        landing = _first_in(rows, order_t + LANDING_MS, order_t + EXIT_TTL_MS, need_vq=True, strict_after=order_t)
        if landing is not None: break
        expiry = order_t + EXIT_TTL_MS
        if expiry > horizon_end or attempts > 10_000: break
        # engine (model_manage.rs:296-360): no new attempt while an order is pending; after expiry retry at once if
        # NEW state arrived since the last attempt or the backoff 30 s*2^min(n-1,5) (cap 15 min) has elapsed, else at
        # the first new state or the schedule, whichever comes first.
        backoff = min(30_000 << min(attempts - 1, 5), 900_000)
        sched = max(expiry, order_t + backoff)
        if any(order_t < s.t <= expiry for s in rows): nxt_t = expiry
        else:
            nxt = _first_in(rows, expiry + 1, sched, need_vq=False)
            nxt_t = nxt.t if nxt is not None else sched
        if nxt_t > horizon_end: break
        order_t = nxt_t; attempts += 1
    res['exit_attempts'] = attempts
    if landing is None:
        # classify why
        subscribed_until = None; dropped = False
        for (t, ev, why) in lane_events:
            if ev == 'dropped_cap': dropped = True
            if ev == 'expire' and t >= t0: subscribed_until = t if subscribed_until is None else min(subscribed_until, t)
        if intent_t + LANDING_MS > capture_end_ms or horizon_end < intent_t + LANDING_MS: why = 'capture_end_censored'
        elif subscribed_until is not None and subscribed_until < horizon_end: why = 'pool_lane_expired_idle'
        elif dropped: why = 'pool_lane_dropped_cap'
        elif any(s.t > intent_t for s in rows) and all(s.vq is None for s in rows if s.t > intent_t): why = 'vq_unverified_on_all_candidate_states'
        else: why = 'no_landing_swap_observed_while_subscribed'
        res['status'] = 'unresolved:' + why
        # labelled valuation sensitivity on the last VERIFIED state (never the outcome)
        last_ok = next((s for s in reversed(rows) if s.vq is not None and s.t <= horizon_end), None)
        if last_ok is not None:
            g = sell_gross_quote_out(last_ok.b, last_ok.q, last_ok.vq, tokens)
            if g is not None:
                res['SENSITIVITY_mark_last_verified_state_net'] = min(g, last_ok.q) * (10_000 - EXIT_VENUE_FEE_BPS) // 10_000 - FIXED - entry_cost + (ATA_RENT - ATA_CLOSE)
                res['SENSITIVITY_mark_state_ms'] = last_ok.t
        res['inventory_cost_basis'] = entry_cost
        return res
    gross = sell_gross_quote_out(landing.b, landing.q, landing.vq, tokens)
    if gross is None: res['status'] = 'unavailable:exit_unpriceable'; return res
    if gross > landing.q: res['status'] = 'unresolved:not_spendable_from_vault'; res['inventory_cost_basis'] = entry_cost; return res
    proceeds = gross * (10_000 - EXIT_VENUE_FEE_BPS) // 10_000
    net = proceeds - FIXED - entry_cost + (ATA_RENT - ATA_CLOSE)
    res.update(status='closed', exit_landing_ms=landing.t, exit_delay_ms=landing.t - intent_t, gross_exit=gross, net_lamports=net,
               SENSITIVITY_net_with_protocol_sell_fee=gross * (10_000 - PROTOCOL_SELL_FEE_BPS_SENS) // 10_000 - FIXED - entry_cost + (ATA_RENT - ATA_CLOSE),
               exit_vault=landing.q, exit_vq=landing.vq)
    return res

# ───────────────────────────── focused proof ─────────────────────────────
def _selftest():
    def sw(t, q, b, vq, fp=(20, 5, 100), ok=True): return Swap(t, q, b, vq, fp, ok)
    Q, B = 100_000_000_000, 200_000_000_000_000; VQ = 17_000_000_000
    t0 = 10_000_000; END = t0 + 10_000_000
    # A) entry lands on the first swap >= t0+400 ms (not the one at +100 ms); fee parts from the landing swap.
    rows = [sw(t0 - 1000, Q, B, VQ), sw(t0 + 100, Q, B, VQ), sw(t0 + 700, Q, B, VQ, (20, 5, 100)), sw(t0 + 1_900_000, Q, B, VQ), sw(t0 + 1_900_600, Q, B, VQ)]
    r = price_episode(rows, t0, 50_000_000, END)
    assert r['fill_ms'] == t0 + 700 and r['exit_reason'] == 'barrier' and r['status'] == 'closed', r
    # intent at fill+1800 s; no swap for 99 s → attempt 1 expires (60 s), scheduled retry at +30 s expires, the retry at
    # +90 s (order t = intent+90 s) lands on the swap at +99.3 s (>= order+400 ms). Engine semantics, 3 attempts.
    # intent at fill+1800 s; attempt 1 pending 60 s with no swap → at expiry the 30 s backoff has elapsed → attempt 2 at
    # +60 s, which lands on the swap at +99.3 s (>= order+400 ms). Engine semantics, 2 attempts.
    assert r['exit_intent_ms'] == t0 + 700 + BARRIER_MS and r['exit_landing_ms'] == t0 + 1_900_000 and r['exit_attempts'] == 2, r
    net, out = buy_exact_quote_in(B, Q, VQ, 50_000_000, 20, 5, 100); assert r['tokens'] == out
    # flat market: loss == round-trip costs only (impact + 95 bps exit fee + 2×fixed + ATA close)
    assert -3_000_000 < r['net_lamports'] < 0, r['net_lamports']
    # B) hard stop: spot <= 65 % of the ALL-IN entry price fires at that state; lands on the next swap >= +400 ms
    rows = [sw(t0 - 1000, Q, B, VQ), sw(t0 + 500, Q, B, VQ), sw(t0 + 60_000, Q * 6 // 10, B, VQ), sw(t0 + 60_300, Q * 6 // 10, B, VQ), sw(t0 + 61_000, Q * 6 // 10, B, VQ)]
    r = price_episode(rows, t0, 50_000_000, END)
    assert r['exit_reason'] == 'amm_hard_stop' and r['exit_intent_ms'] == t0 + 60_000 and r['exit_landing_ms'] == t0 + 61_000, r
    assert r['status'] == 'closed' and r['net_lamports'] < -15_000_000, r
    # C) precursor: a single-state drop >= 30 % vs the previous VERIFIED state (not vs entry); an unverified state neither fires nor moves prev
    # spot path vs entry: 1.256 (Q×1.3) → [unverified Q×0.2 skipped] → 1.214 (Q×1.25) → 0.786 (Q×0.75): drop 35 % vs prev, still > 65 % of entry
    rows = [sw(t0 - 1000, Q, B, VQ), sw(t0 + 500, Q, B, VQ), sw(t0 + 5_000, Q * 13 // 10, B, VQ), sw(t0 + 6_000, Q * 2 // 10, B, None), sw(t0 + 7_000, Q * 125 // 100, B, VQ), sw(t0 + 8_000, Q * 75 // 100, B, VQ), sw(t0 + 9_000, Q * 75 // 100, B, VQ)]
    r = price_episode(rows, t0, 50_000_000, END)
    assert r['exit_reason'] == 'amm_rug_precursor' and r['exit_intent_ms'] == t0 + 8_000 and r['protection_unverified_states_skipped'] == 1, r
    # D) hard stop takes precedence when both conditions hold on the same state
    rows = [sw(t0 - 1000, Q, B, VQ), sw(t0 + 500, Q, B, VQ), sw(t0 + 5_000, Q * 5 // 10, B, VQ), sw(t0 + 6_000, Q * 5 // 10, B, VQ)]
    assert price_episode(rows, t0, 50_000_000, END)['exit_reason'] == 'amm_hard_stop'
    # E) exit TTL + retry: no swap within 60 s of the barrier intent; a swap at +100 s triggers a retry and the NEXT swap lands it
    rows = [sw(t0 - 1000, Q, B, VQ), sw(t0 + 500, Q, B, VQ), sw(t0 + 500 + BARRIER_MS + 100_000, Q, B, VQ), sw(t0 + 500 + BARRIER_MS + 100_900, Q, B, VQ)]
    r = price_episode(rows, t0, 50_000_000, END)
    # attempt 1 expires at +60 s; attempt 2 placed at +60 s lands on the +100 s swap
    assert r['status'] == 'closed' and r['exit_attempts'] == 2 and r['exit_landing_ms'] == t0 + 500 + BARRIER_MS + 100_000, r
    # with the first post-intent swap at +61 s (after expiry, before the +90 s schedule): it triggers a retry and the next swap lands
    rows = [sw(t0 - 1000, Q, B, VQ), sw(t0 + 500, Q, B, VQ), sw(t0 + 500 + BARRIER_MS + 61_000, Q, B, VQ), sw(t0 + 500 + BARRIER_MS + 61_900, Q, B, VQ)]
    r = price_episode(rows, t0, 50_000_000, END)
    assert r['exit_attempts'] == 2 and r['exit_landing_ms'] == t0 + 500 + BARRIER_MS + 61_000, r
    # long quiet gap: attempts 1,2,3 back-to-back (60 s each), then waits of 60 s, 180 s, 420 s, 840 s... → a swap at +700 s
    # arrives during the 420 s wait (attempt 5 scheduled at +660 s... ) — verify monotone attempts and a landing
    rows = [sw(t0 - 1000, Q, B, VQ), sw(t0 + 500, Q, B, VQ), sw(t0 + 500 + BARRIER_MS + 700_000, Q, B, VQ), sw(t0 + 500 + BARRIER_MS + 700_900, Q, B, VQ)]
    r = price_episode(rows, t0, 50_000_000, END)
    assert r['status'] == 'closed' and r['exit_attempts'] >= 5 and r['exit_landing_ms'] in (t0 + 500 + BARRIER_MS + 700_000, t0 + 500 + BARRIER_MS + 700_900), r
    # F) a landing candidate WITHOUT vq cannot land (engine: amm_exit_wait|virtual_quote_unsupported_layout); the next verified one does
    rows = [sw(t0 - 1000, Q, B, VQ), sw(t0 + 500, Q, B, VQ), sw(t0 + 500 + BARRIER_MS + 1_000, Q, B, None), sw(t0 + 500 + BARRIER_MS + 2_000, Q, B, VQ)]
    r = price_episode(rows, t0, 50_000_000, END); assert r['exit_landing_ms'] == t0 + 500 + BARRIER_MS + 2_000, r
    # G) unresolved classifications: capture end; idle expiry; observed no trading while subscribed; sensitivity labelled, status not closed
    rows = [sw(t0 - 1000, Q, B, VQ), sw(t0 + 500, Q, B, VQ)]
    r = price_episode(rows, t0, 50_000_000, capture_end_ms=t0 + 1_000_000); assert r['status'] == 'unresolved:capture_end_censored', r
    r = price_episode(rows, t0, 50_000_000, END, lane_events=[(t0 + 700_000, 'expire', 'idle')]); assert r['status'] == 'unresolved:pool_lane_expired_idle', r
    r = price_episode(rows, t0, 50_000_000, END); assert r['status'] == 'unresolved:no_landing_swap_observed_while_subscribed' and 'SENSITIVITY_mark_last_verified_state_net' in r and 'net_lamports' not in r, r
    # H) entry not landed within 5 s → unavailable (engine fill_none:no_landing_state); landing swap without fee parts → unavailable
    assert price_episode([sw(t0 - 1000, Q, B, VQ), sw(t0 + 9_000, Q, B, VQ)], t0, 50_000_000, END)['status'] == 'unavailable:entry_not_landed'
    assert price_episode([sw(t0 - 1000, Q, B, VQ), sw(t0 + 500, Q, B, VQ, None)], t0, 50_000_000, END)['status'] == 'unavailable:entry_economics_not_on_landing_event'
    # I) vault cap: proceeds cannot exceed the real vault even if vault+vq would
    rows = [sw(t0 - 1000, Q, B, VQ), sw(t0 + 500, Q, B, VQ), sw(t0 + 500 + BARRIER_MS + 1_000, 1_000, B, 10**12), sw(t0 + 500 + BARRIER_MS + 2_000, 1_000, B, 10**12)]
    assert price_episode(rows, t0, 50_000_000, END)['status'] == 'unresolved:not_spendable_from_vault'
    print('episode_pricer_v2 selftest OK: landing 400 ms/5 s TTL, all-in entry px, hard stop 35 % (fill-anchored) > precursor 30 % (single verified state), barrier 1,800 s from fill, exit TTL 60 s + retry on new state/backoff, vq-less states cannot land/trigger, 95 bps exit convention + fixed + ATA, unresolved classified, vault cap')

if __name__ == '__main__':
    _selftest()
