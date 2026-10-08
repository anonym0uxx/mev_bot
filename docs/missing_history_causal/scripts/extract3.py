import sys,json,glob,base64,struct,subprocess,pickle,os
from multiprocessing import Pool
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
TRADE=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
def part(f):
    snaps=[];txs=[];slots=[];other=[]
    p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,bufsize=1<<20)
    for l in p.stdout:
        d=json.loads(l);rt=d['record_type'];pl=d['payload']
        if rt=='account':
            if pl['owner_b58']==PUMP:
                r=base64.b64decode(pl['data_b64'])
                if pl['data_len']==151:
                    vt,vs,rt_,rs=struct.unpack('<QQQQ',r[8:40])
                    snaps.append((d['record_index'],pl['pubkey_b58'],d['slot'],d['recv_unix_ms'],vs,vt,rs,rt_,r[48],pl.get('write_version'),pl.get('txn_signature_b58'),pl['lamports']))
                else:
                    other.append((d['record_index'],pl['pubkey_b58'],d['slot'],pl['data_len']))
        elif rt=='transaction':
            m=pl['meta'];msg=pl['message']
            ks=msg['account_keys_b58']
            lw=m['loaded_writable_addresses_b58'];lr=m['loaded_readonly_addresses_b58']
            allk=ks+lw+lr
            if PUMP not in allk: continue
            h=msg['header'];n=len(ks);ns=h['num_required_signatures']
            w=[k for i,k in enumerate(ks) if (i<ns-h['num_readonly_signed_accounts']) or (ns<=i<n-h['num_readonly_unsigned_accounts'])]+lw
            evs=[]
            for ii in m['inner_instructions'] or []:
                for i in ii['instructions']:
                    if allk[i['program_id_index']]==PUMP:
                        b=base64.b64decode(i['data_b64'])
                        if b[:16]==TRADE and len(b)>=137:
                            sol,tok=struct.unpack('<QQ',b[48:64]);isb=b[64]
                            vs,vt,rs,rt_=struct.unpack('<QQQQ',b[105:137])
                            fee=struct.unpack('<Q',b[177:185])[0] if len(b)>=185 else -1; cfee=struct.unpack('<Q',b[225:233])[0] if len(b)>=233 else -1; evs.append((sol,tok,isb,vs,vt,rs,rt_,b[16:48].hex(),fee,cfee))
            progs=[allk[i['program_id_index']] for i in msg['instructions']]
            ixn=[x[21:] for x in (m['log_messages'] or []) if x.startswith('Program log: Instruction: ')][:12]; txs.append((pl['signature_b58'],d['slot'],pl['tx_index'],d['record_index'],d['recv_unix_ms'],m['err_is_none'],m['err_hex'],w,evs,sum(1 for x in progs if x==PUMP),ixn,[base64.b64decode(i['data_b64'])[:8].hex() for i in msg['instructions'] if allk[i['program_id_index']]==PUMP]))
        elif rt=='slot':
            slots.append((d['record_index'],d['slot'],pl['status'],pl['parent']))
    p.wait()
    return f,(snaps,txs,slots,other)
if __name__=='__main__':
    sess=sys.argv[1]
    fs=sorted(glob.glob(f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_*_part*.ndjson.zst'))
    with Pool(64) as P: res=dict(P.imap_unordered(part,fs))
    out=[res[f] for f in fs]
    S=[x for o in out for x in o[0]];T=[x for o in out for x in o[1]];L=[x for o in out for x in o[2]];O=[x for o in out for x in o[3]]
    os.makedirs('/tmp/mh_recon2/work',exist_ok=True)
    pickle.dump((S,T,L,O),open(f'/tmp/mh_recon2/work/{sess}.v3.pkl','wb'))
    print(sess,len(fs),len(S),len(T),len(L),len(O))
