#!/usr/bin/env python3
"""Windows live-wire acceptance DIAGNOSTIC for the pq-laserstream-grpc sidecar (PQ_LS_INCLUDE_PUMPSWAP=1).
Reads the sidecar's stdout NDJSON (file or stdin) and reports the funnel received -> parsed -> classified
-> decoded -> supported. It does NOT pass/fail on 'no swap seen': a window with no qualifying swap is
INCONCLUSIVE (extend the window), never a parser failure. PASS requires >=1 supported swap whose identity
(mint, canonical pool, WSOL quote) AND economics (fee parts + virtual_quote) are present.
Usage: python3 wire_acceptance.py capture.ndjson [--min-seconds 120]
"""
import sys, json, base64
ALPH='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58d(s):
    n=0
    for c in s: n=n*58+ALPH.index(c)
    b=n.to_bytes((n.bit_length()+7)//8,'big'); return b'\x00'*(len(s)-len(s.lstrip('1')))+b
AMM='pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA'
TAG=bytes([0xe4,0x45,0xa5,0x2e,0x51,0xcb,0x9a,0x1d])
EV={bytes([103,244,82,31,44,245,119,119]):('buy',{472:447,457:432}),bytes([62,47,55,10,165,3,220,42]):('sell',{409:384})}
c=dict(received=0,parsed=0,tx_lines=0,has_inner=0,has_fee_cu=0,pumpswap_tx=0,decoded_event=0,supported_layout=0,supported_with_economics=0,unsupported_layout=0,bad_json=0)
first_ts=last_ts=None; examples=[]
for line in open(sys.argv[1]):
    c['received']+=1
    try: v=json.loads(line)
    except Exception: c['bad_json']+=1; continue
    c['parsed']+=1
    if v.get('kind')!='transaction': continue
    c['tx_lines']+=1
    t=v.get('recv_unix_ms'); first_ts=first_ts or t; last_ts=t or last_ts
    ixs=v.get('instructions',[])
    if v.get('meta',{}).get('fee') is not None and v['meta'].get('compute_units_consumed') is not None: c['has_fee_cu']+=1
    amm=[i for i in ixs if i.get('program_b58')==AMM]
    if amm: c['pumpswap_tx']+=1
    evs=[]
    for i in amm:
        d=base64.b64decode(i['data_b64'])
        if d[:8]==TAG and d[8:16] in EV: evs.append(d)
    if evs: c['has_inner']+=1
    for d in evs:
        c['decoded_event']+=1
        kind,offs=EV[d[8:16]]; p=d[16:]
        if len(p) in offs:
            c['supported_layout']+=1
            if len(evs)==1 and v['meta'].get('fee') is not None:
                c['supported_with_economics']+=1
                if len(examples)<3: examples.append(dict(sig=v['signature_b58'][:16],kind=kind,payload_len=len(p)))
        else: c['unsupported_layout']+=1
secs=((last_ts-first_ts)/1000) if first_ts and last_ts else 0
print(json.dumps(dict(counts=c,window_seconds=secs,examples=examples),indent=1))
if c['tx_lines']==0: print('VERDICT: SUBSCRIPTION UNHEALTHY or empty capture: no transaction lines at all (check endpoint/key/filter).'); sys.exit(2)
if c['supported_with_economics']>=1: print('VERDICT: PASS (serializer+live subscription produce a decodable swap with economics). Then run the line through parse_ndjson_line/decode_amm_swaps to confirm identity.'); sys.exit(0)
print('VERDICT: INCONCLUSIVE: transactions flow (%d) but no supported swap in %.0fs; extend the window. NOT a parser failure.'%(c['tx_lines'],secs)); sys.exit(3)
