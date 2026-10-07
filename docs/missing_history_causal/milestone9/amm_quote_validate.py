"""AMM quote validation against ALL captured PumpSwap Buy/Sell events (both sessions, parts 0..19), separately for BUY and SELL.
Per event the pre-trade reserves + fee bps are in the event; the quote function is run on those inputs and compared with the
event's OWN outcome. Pools restricted to canonical WSOL pools (token-oriented), as the execution scope; non-canonical counted apart.
BUY : buy_exact_quote_in(base_res, quote_vault, virtual_quote, user_quote_amount_in, lp,prot,creator) -> base_out  vs base_amount_out
SELL: gross = eff*base_in/(base_res+base_in); net = gross - ceil(gross*lp/1e4) - ceil(gross*prot/1e4) - ceil(gross*creator/1e4)  vs user_quote_amount_out
      (candidate rule, TESTED here, not assumed). Both reported with exact-match counts and the error distribution of misses.
Reads wire lines (inner instruction data = the event CPI)."""
import json,sys,base64,collections,glob
EV={'buy':bytes([0xe4,0x45,0xa5,0x2e,0x51,0xcb,0x9a,0x1d])+bytes([103,244,82,31,44,245,119,119]),
    'sell':bytes([0xe4,0x45,0xa5,0x2e,0x51,0xcb,0x9a,0x1d])+bytes([62,47,55,10,165,3,220,42])}
def u64(b,o): return int.from_bytes(b[o:o+8],'little')
def ceil_div(a,b): return -(-a//b)
res={k:collections.Counter() for k in ('buy','sell')}
err={'buy':[], 'sell':[]}
ex={'buy':[], 'sell':[]}
for sess in sys.argv[1:]:
    for f in sorted(glob.glob(sess+'/wire_*.ndjson')):
        for l in open(f):
            if 'pAMMBay6' not in l: continue
            d=json.loads(l)
            if d.get('kind')!='transaction' or d['meta'].get('tx_ok')!=1: continue
            for ix in d['instructions']:
                if ix['program_b58']!='pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA': continue
                b=base64.b64decode(ix['data_b64'])
                for side,pre in EV.items():
                    if b[:16]==pre:
                        p=b[16:]
                        if len(p)<336+16: res[side]['short_payload']+=1; continue
                        lp=u64(p,64); prot=u64(p,80); cr=u64(p,336); crfee=u64(p,344)
                        # virtual quote: IDL tail after fixed + creator fields; take the repo rule (last u64 of the documented tail)
                        base_res=u64(p,40); quote_vault=u64(p,48)
                        vq=None
                        if len(p)>=376: vq=u64(p,360)
                        if vq is None: res[side]['no_virtual_quote']+=1; continue
                        if side=='buy':
                            gross_in=u64(p,104); out=u64(p,8)
                            lo,hi=0,gross_in
                            fee=lambda n: n+ceil_div(n*lp,10000)+ceil_div(n*prot,10000)+ceil_div(n*cr,10000)
                            while lo<hi:
                                mid=lo+(hi-lo+1)//2
                                if fee(mid)<=gross_in: lo=mid
                                else: hi=mid-1
                            net=lo
                            if net<2: res[side]['net_lt2']+=1; continue
                            q=base_res*(net-1)//(quote_vault+vq+net-1)
                            res[side]['match' if q==out else 'mismatch']+=1
                            if q!=out: err[side].append(q-out)
                        else:
                            base_in=u64(p,8); user_out=u64(p,104)
                            eff=quote_vault+vq
                            gross=eff*base_in//(base_res+base_in)
                            net=gross-ceil_div(gross*lp,10000)-ceil_div(gross*prot,10000)-ceil_div(gross*cr,10000)
                            res[side]['match' if net==user_out else 'mismatch']+=1
                            if net!=user_out:
                                err[side].append(net-user_out)
                                if len(ex[side])<5: ex[side].append({'gross':gross,'event_quote_out':u64(p,48),'net':net,'user_out':user_out,'lp':lp,'prot':prot,'cr':cr})
out={}
for s in ('buy','sell'):
    e=sorted(err[s]); n=len(e)
    out[s]={'counts':dict(res[s]),'miss_error_min':e[0] if e else None,'miss_error_median':e[n//2] if e else None,'miss_error_max':e[-1] if e else None,'examples':ex[s]}
print(json.dumps(out,indent=1))
