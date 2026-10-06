
import json,os,sys,time,base64,struct,urllib.request,threading,concurrent.futures as cf
OUT="/tmp/atoo_full.jsonl"
D=json.load(open('/tmp/atoo_amm.json'))
sigs=[r['signature'] for r in D['amm']]
done=set()
if os.path.exists(OUT):
    for l in open(OUT):
        try: done.add(json.loads(l)['sig'])
        except Exception: pass
todo=[s for s in sigs if s not in done]
print("todo",len(todo),flush=True)
ALPH='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58d(s):
    n=0
    for c in s: n=n*58+ALPH.index(c)
    b=n.to_bytes((n.bit_length()+7)//8,'big'); return b'\x00'*(len(s)-len(s.lstrip('1')))+b
AMM='pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA'
TAG=bytes([0xe4,0x45,0xa5,0x2e,0x51,0xcb,0x9a,0x1d])
lock=threading.Lock()
def rpc(sig):
    body=json.dumps({"jsonrpc":"2.0","id":1,"method":"getTransaction","params":[sig,{"encoding":"json","maxSupportedTransactionVersion":0,"commitment":"confirmed"}]}).encode()
    for a in range(8):
        try:
            req=urllib.request.Request("https://api.mainnet-beta.solana.com",data=body,headers={"Content-Type":"application/json"})
            with urllib.request.urlopen(req,timeout=30) as r: j=json.loads(r.read())
            if 'result' in j and j['result']: return j['result']
            if j.get('error',{}).get('code')==429: time.sleep(2+a*2); continue
            return None
        except Exception as e:
            time.sleep(1.5+a)
    return None
def work(sig):
    t=rpc(sig)
    if not t: return dict(sig=sig,err="no_tx")
    m=t['transaction']['message']; keys=list(m['accountKeys'])+list(t['meta'].get('loadedAddresses',{}).get('writable',[]))+list(t['meta'].get('loadedAddresses',{}).get('readonly',[]))
    ixs=[(None,i) for i in m['instructions']]
    for g in t['meta'].get('innerInstructions') or []:
        for i in g['instructions']: ixs.append((g['index'],i))
    swap=None; ev=None
    for _,i in ixs:
        if keys[i['programIdIndex']]!=AMM: continue
        d=b58d(i['data'])
        if d[:8]==TAG:
            if d[8:16] in (bytes([103,244,82,31,44,245,119,119]),bytes([62,47,55,10,165,3,220,42])): ev=d
        elif len(i['accounts'])>=5 and swap is None and d[:8] in (bytes.fromhex('66063d1201daebea'),bytes.fromhex('33e685a4017f83ad'),bytes.fromhex('c62e1552b4d9e870')):
            swap=i
    out=dict(sig=sig,fee=t['meta']['fee'],cu=t['meta'].get('computeUnitsConsumed'),err=t['meta'].get('err') is not None)
    if swap:
        a=swap['accounts']; out.update(pool=keys[a[0]],base=keys[a[3]],quote=keys[a[4]])
    if ev:
        p=ev[16:]; g=lambda o:struct.unpack_from('<Q',p,o)[0]
        out.update(ev='buy' if ev[8:16][0]==103 else 'sell')
        if ev[8:16][0]==103: out.update(lp_bps=g(64),prot_bps=g(80),cr_bps=g(336) if len(p)>=344 else None)
        else: out.update(lp_bps=g(64),prot_bps=g(80),cr_bps=g(336) if len(p)>=344 else None)
    return out
n=0
with cf.ThreadPoolExecutor(4) as ex, open(OUT,"a") as f:
    for r in ex.map(work,todo):
        f.write(json.dumps(r)+"\n"); f.flush(); n+=1
        if n%100==0: print(n,flush=True)
print("DONE",flush=True)
