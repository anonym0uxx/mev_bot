import json,base64,subprocess,collections,sys,struct
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
sess=sys.argv[1]; parts=sys.argv[2:]
c=collections.Counter()
for pn in parts:
    f=f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_{sess}_part{pn}.ndjson.zst'
    p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE)
    recent={}  # mint -> deque of keys (join's 64 lookback)
    for l in p.stdout:
        d=json.loads(l)
        if d['record_type']!='transaction': continue
        pl=d['payload'];m=pl['meta'];msg=pl['message']
        if not m['err_is_none']: continue
        ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
        if PUMP not in ks: continue
        flat=list(msg['instructions'])+[i for g in (m['inner_instructions'] or []) for i in g['instructions']]
        evs=[base64.b64decode(i['data_b64']) for i in flat if ks[i['program_id_index']]==PUMP]
        evs=[b for b in evs if b[:16]==TR and len(b)>=137]
        for b in evs:
            mint=b[16:48]; tok=struct.unpack('<Q',b[56:64])[0]; isb=b[64]; user=b[65:97]
            vs,vt=struct.unpack('<QQ',b[105:121])
            sb=tok if isb else -tok
            old=(d['slot'],user,sb,d['recv_unix_ms'])
            new=old+(vs,vt)
            dq=recent.setdefault(mint,collections.deque(maxlen=64))
            c['events']+=1
            if any(k[:4]==old for k in dq):
                c['old_key_would_drop']+=1
                if any(k==new for k in dq): c['new_key_would_drop(true_replay)']+=1
            dq.append(new)
print(sess,dict(c))
