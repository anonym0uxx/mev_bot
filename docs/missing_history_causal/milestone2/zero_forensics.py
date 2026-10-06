"""Zero-reserve TradeEvent forensics. For every event with vs==0 (parts 0..NP): layout length, quote-mint evidence
(token balances other than the event mint), user SOL delta vs event sol, curve-account snapshot reserves for the same
mint's PDA, program/log evidence. Pure read; prints counters + a few examples."""
import sys,json,base64,struct,subprocess,collections
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
sess=sys.argv[1]; NP=int(sys.argv[2])
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58(b):
    n=int.from_bytes(b,'big');s=''
    while n:n,r=divmod(n,58);s=A[r]+s
    return '1'*(len(b)-len(b.lstrip(b'\0')))+s
WSOL='So11111111111111111111111111111111111111112'
c=collections.Counter(); ex=[]; zmints=collections.Counter(); allm=collections.Counter(); quote_seen=collections.Counter()
for pn in range(NP):
    f=f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_{sess}_part{pn:04d}.ndjson.zst'
    p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,bufsize=1<<20)
    for l in p.stdout:
        if b'"transaction"' not in l[:60]: continue
        d=json.loads(l); pl=d['payload']; m=pl['meta']; msg=pl['message']
        if not m['err_is_none']: continue
        ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
        if PUMP not in ks: continue
        evs=[]
        for g in (m['inner_instructions'] or []):
            for ix in g['instructions']:
                if ks[ix['program_id_index']]!=PUMP: continue
                b=base64.b64decode(ix['data_b64'])
                if b[:16]==TR and len(b)>=137: evs.append(b)
        for b in evs:
            mint=b58(b[16:48]); vs=struct.unpack('<Q',b[105:113])[0]; allm[mint]+=1
            if vs!=0: continue
            zmints[mint]+=1
            tb=(m['pre_token_balances'] or [])+(m['post_token_balances'] or [])
            other={e.get('mint') for e in tb}-{mint}
            for o in other: quote_seen[o]+=1
            logs=m['log_messages'] or []
            c[('len',len(b))]+=1
            c[('other_token_mints_in_tx',tuple(sorted(o[:6] for o in other)))]+=1
            if len(ex)<3:
                ex.append(dict(sig=pl['signature_b58'][:12],mint=mint[:8],sol=struct.unpack('<Q',b[48:56])[0],tok=struct.unpack('<Q',b[56:64])[0],
                  buy=b[64],vs=vs,vt=struct.unpack('<Q',b[113:121])[0],rs=struct.unpack('<Q',b[121:129])[0],rt=struct.unpack('<Q',b[129:137])[0],
                  tail=b[137:].hex()[:200],logs=[x[:70] for x in logs if 'Instruction' in x][:6]))
print(sess,'zero events',sum(zmints.values()),'zero mints',len(zmints),'of',len(allm))
full=sum(1 for m,n in zmints.items() if n==allm[m]); print('mints where ALL events zero:',full,'| partly zero:',len(zmints)-full)
print(sorted(c.items(),key=lambda x:-x[1])[:10]); print('quote-like token mints seen in zero-event txs:',quote_seen.most_common(6))
for e in ex: print(e)
