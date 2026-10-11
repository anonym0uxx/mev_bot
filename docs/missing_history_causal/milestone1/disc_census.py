import json,base64,subprocess,collections,sys
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee'); TAG=bytes.fromhex('e445a52e51cb9a1d')
sess=sys.argv[1]; part=sys.argv[2]
f=f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_{sess}_part{part}.ndjson.zst'
p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE)
c=collections.Counter();bal=collections.Counter();lens=collections.Counter()
for l in p.stdout:
    d=json.loads(l)
    if d['record_type']!='transaction': continue
    pl=d['payload'];m=pl['meta'];msg=pl['message']
    ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
    if PUMP not in ks or not m['err_is_none']: continue
    flat=[]
    for ix in msg['instructions']: flat.append(('o',ix))
    for g in m['inner_instructions'] or []:
        for ix in g['instructions']: flat.append(('i',ix))
    nev=0;trd=[]
    for w,ix in flat:
        if ks[ix['program_id_index']]!=PUMP: continue
        b=base64.b64decode(ix['data_b64'])
        if b[:16]==TR: nev+=1;lens[len(b)]+=1
        elif b[:8]==TAG: c[('otherevent',b[8:16].hex())]+=1
        else: c[(w,b[:8].hex())]+=1
    c[('tx_nev',nev)]+=1
print(sorted(c.items(),key=lambda x:-x[1])[:40]);print(lens.most_common(8))
