import json,base64,subprocess,collections,struct,sys
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
sess=sys.argv[1]; c=collections.Counter(); bymint=collections.defaultdict(lambda:[0,0]); lens=collections.Counter()
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
                vs,vt,rs,rt=struct.unpack('<QQQQ',b[105:137]); mint=b[16:48].hex()
                z=(vs==0); bymint[mint][0]+=1; bymint[mint][1]+=z
                lens[(len(b),z)]+=1; c[('vs0' if z else 'ok','rs0' if rs==0 else 'rs>0')]+=1
mixed=sum(1 for a,z in bymint.values() if 0<z<a); allz=sum(1 for a,z in bymint.values() if z==a and z>0)
print(sess,dict(c)); print('lens',dict(lens)); print('mints',len(bymint),'all-zero mints',allz,'mixed mints',mixed)
