import sys,json,base64,struct,subprocess,collections
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
USDC='EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v'
sess=sys.argv[1]; NP=int(sys.argv[2]); c=collections.Counter()
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
for pn in range(NP):
    f=f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_{sess}_part{pn:04d}.ndjson.zst'
    p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,bufsize=1<<20)
    for l in p.stdout:
        if b'"transaction"' not in l[:60]: continue
        d=json.loads(l); pl=d['payload']; m=pl['meta']; msg=pl['message']
        if not m['err_is_none']: continue
        ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
        if PUMP not in ks: continue
        tb=(m['pre_token_balances'] or [])+(m['post_token_balances'] or [])
        usdc=any(e.get('mint')==USDC for e in tb)
        v2='V2' in ' '.join(x for x in (m['log_messages'] or []) if 'Instruction: ' in x and ('Buy' in x or 'Sell' in x))
        for g in (m['inner_instructions'] or []):
            for ix in g['instructions']:
                if ks[ix['program_id_index']]!=PUMP: continue
                b=base64.b64decode(ix['data_b64'])
                if b[:16]==TR and len(b)>=137:
                    vs=struct.unpack('<Q',b[105:113])[0]; sol=struct.unpack('<Q',b[48:56])[0]
                    c[('vs0' if vs==0 else 'vs>0','sol0' if sol==0 else 'sol>0','usdc_in_tx' if usdc else 'no_usdc','V2log' if v2 else 'nonV2log')]+=1
for k,v in sorted(c.items(),key=lambda x:-x[1]): print(v,k)
