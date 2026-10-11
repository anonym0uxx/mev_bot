"""Instruction-level quote identity for pump.fun curve trades.
For each successful tx with a TradeEvent, find the pump instruction that EMITS it (the outer/inner pump ix preceding the event
within the same invocation group) and check whether USDC / WSOL mint is among THAT instruction's own accounts.
Cross-tab vs event vs==0. Output /tmp/mh_recon2/sol/<sess>.quote_ix.json
"""
import sys,json,base64,struct,subprocess,collections,os
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
USDC='EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v'; WSOL='So11111111111111111111111111111111111111112'
sess=sys.argv[1]; NP=int(sys.argv[2]); D='/mnt/data/mev_bot-artifacts/north_star/capture/'+sess
os.makedirs('/tmp/mh_recon2/sol',exist_ok=True)
c=collections.Counter(); ex={}
for p in range(NP):
    f=f'{D}/pumpfun_laserstream_raw_v1_{sess}_part{p:04d}.ndjson.zst'
    pr=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
    for l in pr.stdout:
        if b'"transaction"' not in l[:60]: continue
        d=json.loads(l); pl=d['payload']; m=pl['meta']
        if not m['err_is_none']: continue
        msg=pl['message']; ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
        if PUMP not in ks: continue
        # invocation groups: each outer ix + its inner group (inner index == outer index)
        inner={g['index']:g['instructions'] for g in (m['inner_instructions'] or [])}
        for oi,o in enumerate(msg['instructions']):
            seq=[('o',o)]+[('i',x) for x in inner.get(oi,[])]
            for j,(k,ix) in enumerate(seq):
                if ks[ix['program_id_index']]!=PUMP: continue
                b=base64.b64decode(ix['data_b64'])
                if b[:16]==TR or len(b)<8: continue
                # is the next pump ix in this group a TradeEvent self-CPI? (the program emits it right after the instruction body)
                evs=[]
                for k2,ix2 in seq[j+1:]:
                    if ks[ix2['program_id_index']]!=PUMP: continue
                    b2=base64.b64decode(ix2['data_b64'])
                    if b2[:16]==TR and len(b2)>=137: evs.append(b2)
                    break
                if not evs: continue
                e=evs[0]; vs=struct.unpack('<Q',e[105:113])[0]; sol=struct.unpack('<Q',e[48:56])[0]
                import base64 as B
                acc=B.b64decode(ix['accounts_b64']); accks=[ks[a] for a in acc if a<len(ks)]
                has_usdc=USDC in accks; has_wsol=WSOL in accks
                disc=b[:8].hex()
                key=('vs0' if vs==0 else 'vs>0','ix_has_USDC' if has_usdc else '-','ix_has_WSOL' if has_wsol else '-')
                c[key]+=1; c[('disc',disc,key[0],key[1],key[2])]+=1
                ex.setdefault(key,(pl['signature_b58'][:12],disc,len(accks)))
    pr.stdout.close(); pr.wait()
out={'|'.join(map(str,k)):n for k,n in sorted(c.items(),key=lambda x:-x[1])}
json.dump({'counts':out,'examples':{ '|'.join(k):v for k,v in ex.items()}},open(f'/tmp/mh_recon2/sol/{sess}.quote_ix.json','w'),indent=1)
print(sess); [print(' ',n,k) for k,n in list(out.items())[:24]]
