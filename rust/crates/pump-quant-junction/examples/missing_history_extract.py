import sys,json,glob,base64,struct,subprocess,os
from multiprocessing import Pool
def part(f):
    out=[]; tx={}
    p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE)
    for l in p.stdout:
        try:d=json.loads(l)
        except: continue
        rt=d.get('record_type')
        if rt=='account':
            pl=d['payload']
            if pl['owner_b58'].startswith('6EF8') and pl['data_len']==151:
                r=base64.b64decode(pl['data_b64']); vt,vs,rt_,rs=struct.unpack('<QQQQ',r[8:40])
                out.append((d['record_index'],pl['pubkey_b58'],d['slot'],d['recv_unix_ms'],vs,vt,pl.get('txn_signature_b58')))
    p.wait(); return f,out
if __name__=='__main__':
    sess=sys.argv[1]
    fs=sorted(glob.glob(f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_*_part*.ndjson.zst'))
    with Pool(24) as P:
        res=dict(P.imap_unordered(part,fs))
    with open(f'/tmp/mh_recon/snaps_{sess}.jsonl','w') as o:
        for f in fs:
            for t in res[f]: o.write(json.dumps(t)+'\n')
    print(sess,len(fs),sum(len(v) for v in res.values()))
