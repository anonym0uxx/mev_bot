"""Join: ledger rows x (tx's pump instruction family). Cross-tab class vs corpus-DISC coverage, zero-reserve separated."""
import sys,json,base64,subprocess,collections,re
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
src=open('/home/alon/build/mev_bot_consol/training/code/src/v2/renormalize_raw.py').read()
KNOWN={}
for m in re.finditer(r"\(PUMP_FUN, bytes\(\[([\d, ]+)\]\)\): \('(\w+)'",src):
    KNOWN[bytes(int(x) for x in m.group(1).split(','))]=m.group(2)
sess=sys.argv[1]; NP=int(sys.argv[2]); D='/mnt/data/mev_bot-artifacts/north_star/capture/'+sess
unc=set()
for p in range(NP):
    f=f'{D}/pumpfun_laserstream_raw_v1_{sess}_part{p:04d}.ndjson.zst'
    pr=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
    for l in pr.stdout:
        if b'"transaction"' not in l[:60]: continue
        d=json.loads(l); pl=d['payload']; m=pl['meta']
        if not m['err_is_none']: continue
        msg=pl['message']; ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
        if PUMP not in ks: continue
        ixs=list(msg['instructions'])
        for g in m['inner_instructions'] or []: ixs+=g['instructions']
        pump=[base64.b64decode(i['data_b64']) for i in ixs if ks[i['program_id_index']]==PUMP]
        if any(b[:16]==TR and len(b)>=137 for b in pump) and not any(b[:8] in KNOWN and KNOWN[b[:8]] in ('buy','sell') for b in pump):
            unc.add(pl['signature_b58'])
    pr.stdout.close(); pr.wait()
rows=[json.loads(l) for l in open(f'/tmp/mh_recon2/ledger/{sess}.jsonl')]
c=collections.Counter()
for r in rows:
    z='zero_reserve' if r.get('vs')==0 else 'nonzero'
    c[(r['cls'],z,'NOT_in_corpus_DISC' if r['sig'] in unc else 'corpus_covered_ix')]+=1
out={f'{a}|{b}|{d}':n for (a,b,d),n in sorted(c.items())}
json.dump(out,open(f'/tmp/mh_recon2/ledger/{sess}.crosstab.json','w'),indent=1)
print(sess); [print(' ',n,k) for k,n in sorted(out.items())]
