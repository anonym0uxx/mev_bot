import json,base64,subprocess,collections,struct,sys
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
sess=sys.argv[1]; c=collections.Counter(); ex=[]
for pn in range(20):
    f=f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_{sess}_part{pn:04d}.ndjson.zst'
    p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE)
    for l in p.stdout:
        if b'"transaction"' not in l[:60]: continue
        d=json.loads(l); pl=d['payload']; m=pl['meta']; msg=pl['message']
        if not m['err_is_none']: continue
        ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
        if PUMP not in ks: continue
        for g in (m['inner_instructions'] or []):
            for i in g['instructions']:
                if ks[i['program_id_index']]!=PUMP: continue
                b=base64.b64decode(i['data_b64'])
                if b[:16]!=TR or len(b)<137: continue
                tok=struct.unpack('<Q',b[56:64])[0]; vs,vt,rs,rt=struct.unpack('<QQQQ',b[105:137]); isb=b[64]
                c['events']+=1
                if vt==0 or vs==0 or tok==0:
                    k=('vs0' if vs==0 else '')+('vt0' if vt==0 else '')+('tok0' if tok==0 else '')
                    c[k+('|buy' if isb else '|sell')]+=1
                    if len(ex)<3: ex.append((k,isb,tok,vs,vt,rs,rt,len(b)))
print(sess,dict(c)); print(ex)
