import sys,json,base64,struct,subprocess,collections
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
USDC='EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v'; WSOL='So11111111111111111111111111111111111111112'
sess=sys.argv[1]; NP=int(sys.argv[2]); D='/mnt/data/mev_bot-artifacts/north_star/capture/'+sess
c=collections.Counter(); other=collections.Counter()
for p in range(NP):
    f=f'{D}/pumpfun_laserstream_raw_v1_{sess}_part{p:04d}.ndjson.zst'
    pr=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
    for l in pr.stdout:
        if b'"transaction"' not in l[:60]: continue
        d=json.loads(l); pl=d['payload']; m=pl['meta']
        if not m['err_is_none']: continue
        msg=pl['message']; ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
        if PUMP not in ks: continue
        inner={g['index']:g['instructions'] for g in (m['inner_instructions'] or [])}
        for oi,o in enumerate(msg['instructions']):
            seq=[o]+inner.get(oi,[])
            for j,ix in enumerate(seq):
                if ks[ix['program_id_index']]!=PUMP: continue
                b=base64.b64decode(ix['data_b64'])
                if b[:16]==TR: continue
                ev=None
                for ix2 in seq[j+1:]:
                    if ks[ix2['program_id_index']]!=PUMP: continue
                    b2=base64.b64decode(ix2['data_b64'])
                    if b2[:16]==TR and len(b2)>=137: ev=b2
                    break
                if ev is None: continue
                vs=struct.unpack('<Q',ev[105:113])[0]
                acc=[ks[a] if a<len(ks) else None for a in base64.b64decode(ix['accounts_b64'])]
                q=acc[2] if len(acc)>2 else None
                qc='WSOL' if q==WSOL else 'USDC' if q==USDC else 'legacy_noquote_acct' if len(acc)<=19 else 'OTHER:'+str(q)[:8]
                c[(qc,'vs0' if vs==0 else 'vs>0')]+=1
                if qc.startswith('OTHER'): other[(b[:8].hex(),qc)]+=1
    pr.stdout.close(); pr.wait()
print(sess,dict(c)); print(' other',dict(other))
