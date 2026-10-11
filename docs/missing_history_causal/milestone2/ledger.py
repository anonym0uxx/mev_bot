"""Deterministic discrepancy ledger: pump.fun curve TradeEvents vs the corpus tape (renormalize_raw.py output).
Usage: ledger.py SESSION [nparts=20]   -> /tmp/mh_recon2/ledger/SESSION.{jsonl,summary.json}
Per successful tx touching the pump program: every TradeEvent (key = signature, instruction ordinal in the flattened
outer+inner list) and every corpus-tape row (same signature) are matched on (side, |token qty|). Each leftover/pair is
classified with raw-tx evidence: user token delta, user SOL(+WSOL) delta, event fees, tx fee, rent-sized residuals.
"""
import sys, json, base64, struct, subprocess, collections
from multiprocessing import Pool
PUMP = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
TR = bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
WSOL = 'So11111111111111111111111111111111111111112'
BUYS = {bytes([102,6,61,18,1,218,235,234]), bytes([184,23,238,97,103,197,211,61]), bytes([56,252,116,8,158,223,205,95])}
SELLS = {bytes([51,230,133,164,1,127,131,173]), bytes([93,246,130,60,231,233,64,178])}
RENT_ATA = 2039280
A = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58(b):
    n = int.from_bytes(b, 'big'); s = ''
    while n: n, r = divmod(n, 58); s = A[r] + s
    return '1' * (len(b) - len(b.lstrip(b'\0'))) + s
sess = sys.argv[1]; NP = int(sys.argv[2]) if len(sys.argv) > 2 else 20
TAPE = collections.defaultdict(list)
for l in open(f'/tmp/mh_recon2/ref/{sess}/pf.jsonl'):
    d = json.loads(l); TAPE[d['signature']].append(d)
def amt(entries, owner, mint):
    t = 0
    for e in entries or []:
        if e.get('owner') == owner and e.get('mint') == mint:
            try: t += int((e.get('ui_token_amount') or {}).get('amount') or 0)
            except Exception: pass
    return t
def part(pn):
    f = f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_{sess}_part{pn:04d}.ndjson.zst'
    p = subprocess.Popen(['zstd', '-dc', f], stdout=subprocess.PIPE, bufsize=1 << 20)
    rows = []
    for l in p.stdout:
        if b'"transaction"' not in l[:60]: continue
        d = json.loads(l); pl = d['payload']; m = pl['meta']; msg = pl['message']
        if not m['err_is_none']: continue
        ks = msg['account_keys_b58'] + m['loaded_writable_addresses_b58'] + m['loaded_readonly_addresses_b58']
        if PUMP not in ks: continue
        sig = pl['signature_b58']; slot = d['slot']
        ixs = []   # flattened outer then inner, same as the wire
        for ix in msg['instructions']: ixs.append(('o', ix))
        for g in (m['inner_instructions'] or []):
            for ix in g['instructions']: ixs.append(('i', ix))
        evs = []; curve_ix = []
        for o, (kind, ix) in enumerate(ixs):
            if ks[ix['program_id_index']] != PUMP: continue
            b = base64.b64decode(ix['data_b64'])
            if b[:8] in BUYS: curve_ix.append((o, 'buy'))
            elif b[:8] in SELLS: curve_ix.append((o, 'sell'))
            if b[:16] == TR and len(b) >= 137:
                e = dict(ord=o, mint=b58(b[16:48]), user=b58(b[65:97]), buy=bool(b[64]),
                         sol=struct.unpack('<Q', b[48:56])[0], tok=struct.unpack('<Q', b[56:64])[0],
                         vs=struct.unpack('<Q', b[105:113])[0], rs=struct.unpack('<Q', b[121:129])[0], n=len(b))
                if len(b) >= 233:
                    e['fee'] = struct.unpack('<Q', b[177:185])[0]; e['cfee'] = struct.unpack('<Q', b[225:233])[0]
                evs.append(e)
        tape = TAPE.get(sig, [])
        if not evs and not tape and not curve_ix: continue
        pre, post = m['pre_balances'], m['post_balances']; ptb, qtb = m['pre_token_balances'], m['post_token_balances']
        def user_delta(user, mint):
            sd = None
            if user in ks:
                i = ks.index(user)
                if i < len(pre) and i < len(post): sd = post[i] - pre[i]
            w = amt(qtb, user, WSOL) - amt(ptb, user, WSOL)
            td = amt(qtb, user, mint) - amt(ptb, user, mint)
            return (None if sd is None else sd + w), td
        used = set()
        for e in evs:
            side = 'buy' if e['buy'] else 'sell'
            hit = None
            for k, t in enumerate(tape):
                if k in used: continue
                if t['side'] == side and abs(t['tokens_raw']) == e['tok'] and t['mint'] == e['mint']:
                    hit = k; break
            sd, td = user_delta(e['user'], e['mint'])
            r = dict(sig=sig, slot=slot, ord=e['ord'], side=side, mint=e['mint'], user=e['user'], ev_sol=e['sol'], ev_tok=e['tok'],
                     vs=e['vs'], fee=e.get('fee'), cfee=e.get('cfee'), txfee=m['fee'], u_sd=sd, u_td=td, n_ev=len(evs), n_tape=len(tape),
                     n_curve_ix=len(curve_ix), payer=(ks[0] == e['user']))
            if hit is None:
                r['cls'] = 'event_only'
                if e['vs'] == 0: r['reason'] = 'zero_reserve_event'
                elif td != (e['tok'] if e['buy'] else -e['tok']): r['reason'] = 'user_token_delta_differs_from_event_qty'
                elif sd is None: r['reason'] = 'user_not_in_balance_arrays'
                elif (side == 'buy' and not sd < 0) or (side == 'sell' and not sd > 0): r['reason'] = 'user_sol_sign_test_fails'
                else: r['reason'] = 'resolver_rejected_other(router/net_position conservation?)'
            else:
                used.add(hit); t = tape[hit]
                r['tape_sol'] = t['sol_lamports']; r['tape_trader'] = t['trader']; r['res'] = t['resolution']
                if t['trader'] != e['user']: r['cls'] = 'trader_mismatch'
                else:
                    r['cls'] = 'matched'
                    exp = -(e['sol'] + e.get('fee', 0) + e.get('cfee', 0)) if e['buy'] else (e['sol'] - e.get('fee', 0) - e.get('cfee', 0))
                    res = t['sol_lamports'] - exp
                    r['resid'] = res
                    if e.get('fee') is None: r['amt'] = 'event_has_no_fee_fields'
                    elif res == 0: r['amt'] = 'tape==swap_qty_net_of_curve_fees'
                    elif t['sol_lamports'] == (-e['sol'] if e['buy'] else e['sol']): r['amt'] = 'tape==gross_swap_qty'
                    elif r['payer'] and res == -m['fee']: r['amt'] = 'resid==-txfee(payer)'
                    elif r['payer'] and res < 0 and (res + m['fee']) % RENT_ATA == 0: r['amt'] = 'resid==-txfee-k*ATA_rent'
                    elif res % RENT_ATA == 0: r['amt'] = 'resid==k*ATA_rent'
                    else: r['amt'] = 'other_transfer_or_multi_ix'
            rows.append(r)
        for k, t in enumerate(tape):
            if k in used: continue
            r = dict(sig=sig, slot=slot, cls='tape_only', side=t['side'], mint=t['mint'], trader=t['trader'], tape_sol=t['sol_lamports'],
                     tape_tok=t['tokens_raw'], res=t['resolution'], n_ev=len(evs), n_tape=len(tape), n_curve_ix=len(curve_ix))
            r['reason'] = 'tx_has_no_trade_event' if not evs else 'tx_has_events_but_none_matches_qty'
            rows.append(r)
        if not evs and curve_ix and not tape:
            rows.append(dict(sig=sig, slot=slot, cls='curve_ix_without_event_or_tape', n_curve_ix=len(curve_ix)))
    p.wait(); return rows
if __name__ == '__main__':
    with Pool(20) as P: res = P.map(part, range(NP))
    rows = [r for x in res for r in x]
    rows.sort(key=lambda r: (r['slot'], r['sig'], r.get('ord', -1), r['cls']))
    import os; os.makedirs('/tmp/mh_recon2/ledger', exist_ok=True)
    with open(f'/tmp/mh_recon2/ledger/{sess}.jsonl', 'w') as o:
        for r in rows: o.write(json.dumps(r, sort_keys=True) + '\n')
    S = collections.Counter(); ex = {}
    for r in rows:
        k = (r['cls'], r.get('reason') or r.get('amt') or ''); S[k] += 1; ex.setdefault(k, r)
    out = {'session': sess, 'parts': NP, 'rows': len(rows), 'counts': {f'{a}|{b}': n for (a, b), n in sorted(S.items())},
           'examples': {f'{a}|{b}': v for (a, b), v in sorted(ex.items())}}
    json.dump(out, open(f'/tmp/mh_recon2/ledger/{sess}.summary.json', 'w'), indent=1, sort_keys=True)
    print(json.dumps(out['counts'], indent=1))
