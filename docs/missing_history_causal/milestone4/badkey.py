"""Malformed-key census in RAW capture records: which key strings are not 32 bytes, in which list (static/loaded), at which
index, which capture build (record schema/producer fields), and does the raw tx carry the same string (i.e. is the defect
in the capture emitter, not in our to_wire)?"""
import sys,json,subprocess,collections,glob
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def blen(s):
    n=0
    for c in s:
        i=A.find(c)
        if i<0: return -1
        n=n*58+i
    nb=(n.bit_length()+7)//8
    return nb+(len(s)-len(s.lstrip('1')))
sess=sys.argv[1]; NP=int(sys.argv[2])
bad=collections.Counter(); where=collections.Counter(); ex=[]; ntx=0; nbadtx=0; hdr=None; vers=collections.Counter()
for pn in range(NP):
    f=f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_{sess}_part{pn:04d}.ndjson.zst'
    p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,bufsize=1<<20)
    for l in p.stdout:
        if b'"record_type": "transaction"' not in l[:200] and b'"record_type":"transaction"' not in l[:200]: continue
        d=json.loads(l); pl=d['payload']; ntx+=1
        if hdr is None: hdr={k:v for k,v in d.items() if k!='payload'}
        m=pl.get('meta') or {}; msg=pl.get('message') or {}
        lists=[('static',msg.get('account_keys_b58',[])),('loaded_w',m.get('loaded_writable_addresses_b58',[])),('loaded_r',m.get('loaded_readonly_addresses_b58',[]))]
        off=0; hit=False
        for nm,ks in lists:
            for j,k in enumerate(ks):
                L=blen(k)
                if L!=32:
                    hit=True; bad[(k,L)]+=1; where[(nm,off+j)]+=1
                    if len(ex)<3: ex.append((pl['signature_b58'][:12],nm,off+j,k))
            off+=len(ks)
        nbadtx+=hit
    p.kill()
print(sess,'tx',ntx,'tx_with_bad_key',nbadtx)
print('bad strings',dict(bad)); print('where (list,index) top',where.most_common(6)); print('examples',ex); print('record header keys',hdr)
