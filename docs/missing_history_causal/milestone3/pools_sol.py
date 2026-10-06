"""SOL discovery evidence from raw captures (read-only).
Pools: every PumpSwap Pool account record (owner pAMM, len>=211): base/quote mint, vault accounts, canonical check (PDA, pure python),
vault mint verification through tx token balances (account_index -> mint), orientation, quote class.
Curves: every pump BondingCurve account (len 151 etc) -> quote class from the emitting instruction accounts (USDC/WSOL/none).
Output: /tmp/mh_recon2/sol/<sess>.pools.json (+ per-pool sample rows)"""
import sys,json,base64,struct,subprocess,collections,hashlib,os
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; AMM='pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA'
WSOL='So11111111111111111111111111111111111111112'; USDC='EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v'
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58(b):
    n=int.from_bytes(b,'big'); o=''
    while n: n,r=divmod(n,58); o=A[r]+o
    return '1'*(len(b)-len(b.lstrip(b'\0')))+o
def b58d(s):
    n=0
    for ch in s: n=n*58+A.index(ch)
    return n.to_bytes(32,'big')
P=2**255-19; Dc=(-121665*pow(121666,P-2,P))%P
def on_curve(b):
    y=int.from_bytes(b,'little')&((1<<255)-1)
    if y>=P: return False
    u=(y*y-1)%P; v=(Dc*y*y+1)%P
    x2=u*pow(v,P-2,P)%P
    return x2==0 or pow(x2,(P-1)//2,P)==1
def pda(seeds,prog):
    for bump in range(255,-1,-1):
        h=hashlib.sha256(b''.join(seeds)+bytes([bump])+prog+b'ProgramDerivedAddress').digest()
        if not on_curve(h): return h
sess=sys.argv[1]; NP=int(sys.argv[2]); D='/mnt/data/mev_bot-artifacts/north_star/capture/'+sess
POOL_DISC=hashlib.sha256(b'account:Pool').digest()[:8]
pools={}   # pubkey -> dict
vault_mint=collections.defaultdict(set)   # vault account pubkey -> mints observed in token balances
for p in range(NP):
    f=f'{D}/pumpfun_laserstream_raw_v1_{sess}_part{p:04d}.ndjson.zst'
    pr=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
    for l in pr.stdout:
        h=l[:60]
        if b'"account"' in h:
            d=json.loads(l); pl=d['payload']
            if pl['owner_b58']!=AMM: continue
            data=base64.b64decode(pl['data_b64'])
            if len(data)<211 or data[:8]!=POOL_DISC: continue
            pools[pl['pubkey_b58']]=dict(len=len(data),base=b58(data[43:75]),quote=b58(data[75:107]),bv=b58(data[139:171]),qv=b58(data[171:203]),
                creator=b58(data[11:43]),index=struct.unpack('<H',data[9:11])[0],slot=d['slot'])
        elif b'"transaction"' in h:
            d=json.loads(l); pl=d['payload']; m=pl['meta']
            if not m['err_is_none']: continue
            msg=pl['message']; ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
            if AMM not in ks: continue
            for tb in (m['pre_token_balances'] or [])+(m['post_token_balances'] or []):
                ai=tb.get('account_index')
                if ai is not None and ai<len(ks): vault_mint[ks[ai]].add(tb.get('mint'))
    pr.stdout.close(); pr.wait()
amm_b=b58d(AMM); pump_b=b58d(PUMP); wsol_b=b58d(WSOL)
cls=collections.Counter(); samples={}
for pk,v in pools.items():
    q=v['quote']; b=v['base']
    qclass='WSOL' if q==WSOL else 'USDC' if q==USDC else 'OTHER'
    orient='quote_is_WSOL_base_token' if q==WSOL else 'REVERSED_base_is_WSOL' if b==WSOL else 'no_WSOL_side'
    canon=None
    if q==WSOL and b!=WSOL:
        auth=pda([b'pool-authority',b58d(b)],pump_b)
        canon=(b58(pda([b'pool',(0).to_bytes(2,'little'),auth,b58d(b),wsol_b],amm_b))==pk)
    # vault verification
    bvm=vault_mint.get(v['bv']); qvm=vault_mint.get(v['qv'])
    vv='vault_mints_confirmed' if (bvm=={b} and qvm=={q}) else ('vaults_not_observed' if not bvm or not qvm else 'VAULT_MINT_MISMATCH')
    key=(qclass,orient,'canonical' if canon else ('noncanonical' if canon is False else 'n/a'),vv)
    cls[key]+=1; samples.setdefault(key,[]).append(dict(pool=pk,base=b,quote=q,idx=v['index'],len=v['len']))
out={'|'.join(map(str,k)):n for k,n in sorted(cls.items(),key=lambda x:-x[1])}
os.makedirs('/tmp/mh_recon2/sol',exist_ok=True)
json.dump({'pool_records':len(pools),'classes':out,'samples':{'|'.join(map(str,k)):v[:3] for k,v in samples.items()}},open(f'/tmp/mh_recon2/sol/{sess}.pools.json','w'),indent=1)
print(sess,'pool account records',len(pools)); [print(' ',n,k) for k,n in out.items()]
