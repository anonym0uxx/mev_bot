"""Full IDL-driven TradeEvent parse; cross-check event.quote_mint vs the emitting ix's quote account (V2 idx 2) and ix_name.
Usage: idl_event.py SESS NPARTS  -> JSON summary. Read-only."""
import sys,json,struct,subprocess,collections,base64
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58(b):
    n=int.from_bytes(b,'big'); s=''
    while n: n,r=divmod(n,58); s=A[r]+s
    return '1'*(len(b)-len(b.lstrip(b'\0')))+s
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
IDL=json.load(open('/home/alon/build/mev_bot_consol/docs/missing_history_causal/milestone4/idl/pump.json'))
TYPES={t['name']:t['type'] for t in IDL['types']}
FIELDS=TYPES['TradeEvent']['fields']
def rd(t,b,o):
    if isinstance(t,str):
        w={'u64':8,'i64':8,'u32':4,'u16':2,'u8':1,'bool':1,'pubkey':32,'i32':4,'u128':16}[t]
        if o+w>len(b): raise IndexError
        x=b[o:o+w]
        v={'u64':lambda:int.from_bytes(x,'little'),'i64':lambda:int.from_bytes(x,'little',signed=True),'u32':lambda:int.from_bytes(x,'little'),
           'u16':lambda:int.from_bytes(x,'little'),'u8':lambda:x[0],'bool':lambda:x[0]!=0,'pubkey':lambda:b58(x),'i32':lambda:int.from_bytes(x,'little',signed=True),'u128':lambda:int.from_bytes(x,'little')}[t]()
        return v,o+w
    if 'vec' in t:
        n,o=rd('u32',b,o); out=[]
        for _ in range(n):
            v,o=rd(t['vec'],b,o); out.append(v)
        return out,o
    if 'option' in t:
        f,o=rd('u8',b,o)
        return (rd(t['option'],b,o) if f else (None,o))
    if 'defined' in t:
        ty=TYPES[t['defined']['name']]; d={}
        for f in ty['fields']:
            d[f['name']],o=rd(f['type'],b,o)
        return d,o
    raise ValueError(t)
def parse(b):
    o=16; d={}
    for f in FIELDS:
        t=f['type']
        if o>=len(b): d['_ended_before']=f['name']; break
        try:
            if t=='string':
                n,o=rd('u32',b,o); d[f['name']]=bytes(b[o:o+n]).decode('utf8','replace'); o+=n
            else: d[f['name']],o=rd(t,b,o)
        except IndexError:
            d['_truncated_in']=f['name']; break
    return d,o
sess=sys.argv[1]; NP=int(sys.argv[2])
DISC=bytes.fromhex('e445a52e51cb9a1d')
c=collections.Counter(); ixn=collections.Counter(); qm=collections.Counter(); lens=collections.Counter(); short=0
for pn in range(NP):
    f=f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_{sess}_part{pn:04d}.ndjson.zst'
    p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,bufsize=1<<20)
    for l in p.stdout:
        if b'"transaction"' not in l[:80]: continue
        d=json.loads(l); pl=d['payload']; m=pl.get('meta') or {}
        if m.get('err') is not None or not m.get('err_is_none',True) and 'err_is_none' in m: continue
        msg=pl['message']; keys=msg['account_keys_b58']+m.get('loaded_writable_addresses_b58',[])+m.get('loaded_readonly_addresses_b58',[])
        outer=msg['instructions']; groups={g['index']:g['instructions'] for g in (m.get('inner_instructions') or [])}
        def ixacc(ix):
            return list(base64.b64decode(ix['accounts_b64'])) if ix.get('accounts_b64') else list(ix.get('accounts',[]))
        for oi,oix in enumerate(outer):
            members=[('o',oix)]+[('i',x) for x in groups.get(oi,[])]
            # an event belongs to the nearest preceding NON-event pump ix in the same invocation tree (outer ix first)
            emitter=None
            for kind,ix in members:
                if keys[ix['program_id_index']]!=PUMP: continue
                data=base64.b64decode(ix['data_b64'])
                if data[:16]==bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee'):
                    lens[len(data)]+=1
                    try: ev,o=parse(data)
                    except Exception as e: c['parse_fail']+=1; continue
                    c['events']+=1
                    c['end:'+str(ev.get('_ended_before') or ev.get('_truncated_in') or 'full')]+=1
                    ixn[(ev.get('ix_name'),'emitter_known' if emitter else 'no_emitter')]+=1
                    if emitter:
                        acc,v2=emitter
                        pq=keys[acc[2]] if len(acc)>2 and acc[2]<len(keys) else None
                        WS='So11111111111111111111111111111111111111112'; NAT='11111111111111111111111111111111'
                        eq=ev.get('quote_mint'); canon=NAT if pq==WS else pq
                        if v2: c['v2:'+('ix_quote_agrees_with_event(WSOL<->native)' if canon==eq else 'DISAGREE ix=%s ev=%s'%(pq,eq))]+=1
                        else: c['legacy_ix:event_quote='+('native' if eq==NAT else str(eq))]+=1
                    qm[(ev.get('quote_mint'),'vsol0' if ev.get('virtual_sol_reserves')==0 else 'vsol>0','vq0' if ev.get('virtual_quote_reserves')==0 else 'vq>0')]+=1
                else:
                    emitter=(ixacc(ix),data[:8].hex() in('c2ab1c46684d5b2f','b817ee6167c5d33d','5df6823ce7e940b2'))
    p.kill()
print(sess,dict(c)); print('ix_name',dict(ixn)); print('event lens top',lens.most_common(6)); print('quote_mint,vsol&vquote==0',dict(qm))
