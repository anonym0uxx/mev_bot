#!/usr/bin/env python3
"""research-004 terminal analysis — frozen BEFORE outcomes (manifest v1 + amendment v1.1). SHORTENED OBSERVATION.
Episode = first AMM-regime ask per mint (prompt_fields record; pricing_eligible) at or before the enrollment cutoff.
Outcomes by episode_pricer_v2 (exact PROD_HOLD_V16 + protections) at clips 0.05 and 0.15 SOL; both reported, none selected.
Comparisons: cash (0); frozen-Qwen BUY-intent episodes (disposition notrade:observation_only on the FIRST ask);
outcome-blind matched-participation abstention (2,000 random same-count subsets of all PRICED episodes). No C rule.
Usage: research004_analysis.py [--dry]   (--dry: pipeline integrity only, prints NO economics)."""
import json, sys, random, collections, statistics, time
sys.path.insert(0, '/training/mh_build/wt_attr8/analysis/normfix007')
import episode_pricer_v2 as P
D = '/training/paper/research-004'
DRY = '--dry' in sys.argv
ALPH = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58(hx):
    n = int(hx, 16); out = ''
    while n: n, r = divmod(n, 58); out = ALPH[r] + out
    return '1' * (len(hx) // 2 - len(bytes.fromhex(hx).lstrip(b'\0'))) + out
m = json.load(open(f'{D}/RESEARCH004_MANIFEST_v1.json'))
start_ms = m['start_unix_s'] * 1000; enroll_end_ms = m['amendment_v1_1']['enrollment_end_unix_s'] * 1000
followup_end_ms = m['amendment_v1_1']['followup_end_unix_s'] * 1000
# ── asks
first_amm = {}; first_disp = {}; asks_per_mint = collections.Counter(); all_dispatch_mints = set(); buy_intent = set(); reentry = 0; later_buy_mints = set()
for l in open(f'{D}/data/model_io.jsonl'):
    d = json.loads(l); mh = d.get('mint_hex')
    if d['ev'] == 'dispatch': all_dispatch_mints.add(mh); asks_per_mint[mh] += 1
    if d['ev'] == 'prompt_fields' and mh not in first_amm and d['clock_ms'] <= enroll_end_ms and str(d['fields'].get('pricing_eligible', '')).lower() == 'true':
        first_amm[mh] = d
    if d['ev'] == 'disposition' and mh in first_amm and mh not in first_disp and d['clock_ms'] >= first_amm[mh]['clock_ms']:
        first_disp[mh] = d['status']
        if 'observation_only' in d['status']: buy_intent.add(mh)
    if d['ev'] == 'disposition' and 'reentry' in str(d.get('status', '')): reentry += 1
    if d['ev'] == 'disposition' and 'observation_only' in d['status']: later_buy_mints.add(mh)
# ── pool lane counts
lane = P.load_pool_lane(f'{D}/data/feed_pool_lane.jsonl')
lane_ev = collections.Counter(ev for evs in lane.values() for (_, ev, _) in evs)
dropped_cap_mints = {mm for mm, evs in lane.items() if any(ev == 'dropped_cap' for (_, ev, _) in evs)}
# ── tape
mints_b58 = {mh: b58(mh) for mh in first_amm}
tape = P.load_tape(f'{D}/data/event_stream.jsonl', set(mints_b58.values()))
capture_end = max((s.t for rows in tape.values() for s in rows), default=start_ms)
n_sw = sum(len(r) for r in tape.values()); n_vq = sum(1 for r in tape.values() for s in r if s.vq is not None)
counts = {'label': 'SHORTENED OBSERVATION (2 h enrollment, 40 min follow-up)', 'start_pt': m['frozen_at_pt'], 'enrollment_end_pt': m['amendment_v1_1']['enrollment_end_pt'], 'followup_end_pt': m['amendment_v1_1']['followup_end_pt'],
          'capture_end_pt': time.strftime('%H:%M:%S PT', time.localtime(capture_end / 1000)),
          'dispatch_total': sum(asks_per_mint.values()), 'distinct_dispatched_mints_all_venues': len(all_dispatch_mints),
          'amm_episodes_enrolled': len(first_amm), 'buy_intent_first_ask': len(buy_intent), 'skip_first_ask': sum(1 for v in first_disp.values() if v == 'skip'),
          'other_first_disposition': collections.Counter(v.split('|')[0] for v in first_disp.values() if v != 'skip' and 'observation_only' not in v),
          'repeat_asks_on_enrolled': sum(asks_per_mint[mh] - 1 for mh in first_amm), 'reentry_dispositions': reentry,
          'DESCRIPTIVE_first_ask_buy_mints': len(buy_intent), 'DESCRIPTIVE_mints_with_any_buy_intention_any_ask': len(later_buy_mints),
          'DESCRIPTIVE_enrolled_mints_with_later_buy_intention_after_first_skip': len({mh for mh in first_amm if mh in later_buy_mints and mh not in buy_intent}),
          'scope_note': 'first-ask analysis evaluates Qwen\'s FIRST encounter with each mint, not the repeated-decision entry policy; descriptive counts only, no new outcome test',
          'pool_lane_events': dict(lane_ev), 'migrations_dropped_cap_not_enrolled': len(dropped_cap_mints),
          'amm_swaps_journaled_enrolled_mints': n_sw, 'with_state_matched_vq': n_vq}
print(json.dumps(counts, indent=1, default=str))
if DRY: print('DRY: pipeline OK; no outcomes computed'); sys.exit(0)
# ── outcomes
res = {}
for clip in (50_000_000, 150_000_000):
    rows_out = {}
    for mh, d in first_amm.items():
        mb = mints_b58[mh]; t0 = d['clock_ms']
        r = P.price_episode(tape.get(mb, []), t0, clip, capture_end, lane.get(mb, []), followup_cutoff_ms=followup_end_ms)
        r['mint'] = mb; r['buy_intent'] = mh in buy_intent; r['first_disposition'] = first_disp.get(mh, 'none'); rows_out[mb] = r
    res[clip] = rows_out
def summarize(rows, label):
    st = collections.Counter(r['status'] for r in rows)
    priced = [r for r in rows if r['status'] == 'closed']
    out = {'label': label, 'episodes': len(rows), 'status': dict(st), 'priced': len(priced)}
    if priced:
        nets = [r['net_lamports'] / 1e9 for r in priced]
        out.update(sum_net_sol=round(sum(nets), 6), mean_net_sol=round(statistics.mean(nets), 6), median_net_sol=round(statistics.median(nets), 6),
                   positive_share=round(sum(n > 0 for n in nets) / len(nets), 3), exit_reasons=dict(collections.Counter(r['exit_reason'] for r in priced)),
                   exit_delay_ms_median=statistics.median(r['exit_delay_ms'] for r in priced), attempts_max=max(r['exit_attempts'] for r in priced),
                   sens_protocol_fee_sum_sol=round(sum(r['SENSITIVITY_net_with_protocol_sell_fee'] for r in priced) / 1e9, 6))
        gp = [n for n in nets if n > 0]; out['top_mint_share_of_gross_positive'] = round(max(gp) / sum(gp), 3) if gp else None
        if len(priced) >= 2:
            random.seed(4); bs = []
            for _ in range(2000): bs.append(statistics.mean(random.choice(nets) for _ in nets))  # by-mint (one episode per mint) cluster bootstrap
            bs.sort(); out['bootstrap90_mean_net_sol_CONDITIONAL_ON_PRICED'] = [round(bs[100], 6), round(bs[1899], 6)]
    unres = [r for r in rows if r['status'].startswith('unresolved')]
    out['unresolved_cost_basis_sol'] = round(sum(r.get('inventory_cost_basis', 0) for r in unres) / 1e9, 6)
    sens = [r['SENSITIVITY_mark_last_verified_state_net'] for r in unres if 'SENSITIVITY_mark_last_verified_state_net' in r]
    out['SENSITIVITY_unresolved_marked_last_verified_state_sum_sol'] = round(sum(sens) / 1e9, 6) if sens else None
    out['SENSITIVITY_unresolved_without_any_mark'] = len(unres) - len(sens)
    return out
report = {'counts': counts, 'clips': {}}
for clip, rows_out in res.items():
    rows = list(rows_out.values()); bi = [r for r in rows if r['buy_intent']]; sk = [r for r in rows if not r['buy_intent']]
    rep = {'all_enrolled': summarize(rows, 'all enrolled episodes'), 'qwen_buy_intent': summarize(bi, 'frozen Qwen BUY-intent first asks'), 'qwen_skip_or_other': summarize(sk, 'SKIP/other first asks'), 'cash_benchmark_sol': 0.0}
    priced_all = [r['net_lamports'] / 1e9 for r in rows if r['status'] == 'closed']; k = sum(1 for r in bi if r['status'] == 'closed')
    if k >= 1 and len(priced_all) > k:
        random.seed(7); means = sorted(statistics.mean(random.sample(priced_all, k)) for _ in range(2000))
        rep['outcome_blind_matched_participation_abstention'] = {'k_priced_buy_intent': k, 'mean_net_90pct': [round(means[100], 6), round(means[1899], 6)], 'median': round(means[1000], 6),
            'qwen_buy_intent_mean': rep['qwen_buy_intent'].get('mean_net_sol'), 'note': 'random same-count subsets of all PRICED episodes; diagnostic only'}
    rep['diagnostic_share_above_2x_fees'] = round(sum(1 for n in priced_all if n > 2 * 0.0095 * clip / 1e9) / len(priced_all), 3) if priced_all else None
    report['clips'][f'{clip/1e9:.2f}_SOL'] = rep
report['credits'] = int(json.load(open(f'{D}/data/feed_budget_state.json'))['window_total_bytes'] / 1e6 * 20)
report['episodes_detail'] = {f'{clip/1e9:.2f}': [{k: v for k, v in r.items() if k not in ('entry_fee_parts',)} for r in rows_out.values()] for clip, rows_out in res.items()}
json.dump(report, open(f'{D}/RESEARCH004_RESULT.json', 'w'), indent=1, default=str)
print(json.dumps({k: v for k, v in report.items() if k != 'episodes_detail'}, indent=1, default=str))
