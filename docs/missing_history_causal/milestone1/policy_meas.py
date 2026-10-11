import json,base64,subprocess,collections,sys
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
BS={bytes([102,6,61,18,1,218,235,234]),bytes([51,230,133,164,1,127,131,173]),bytes([184,23,238,97,103,197,211,61]),bytes([93,246,130,60,231,233,64,178]),bytes([56,252,116,8,158,223,205,95])}
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58(b):
    n=int.from_bytes(b,'big');s=''
    while n:
        n,r=divmod(n,58);s=A[r]+s
    return '1'*(len(b)-len(b.lstrip(b'\0')))+s
sess=sys.argv[1]; parts=sys.argv[2:]
c=collections.Counter(); maxslot={}
for pn in parts:
    f=f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_{sess}_part{pn}.ndjson.zst'
    p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE)
    for l in p.stdout:
        d=json.loads(l)
        if d['record_type']!='transaction': continue
        pl=d['payload'];m=pl['meta'];msg=pl['message']
        ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
        if PUMP not in ks: continue
        ok=m['err_is_none']
        evs=[];bsix=0
        flat=list(msg['instructions'])+[i for g in (m['inner_instructions'] or []) for i in g['instructions']]
        for ix in flat:
            if ks[ix['program_id_index']]!=PUMP: continue
            b=base64.b64decode(ix['data_b64'])
            if b[:16]==TR and len(b)>=137: evs.append(b)
            elif b[:8] in BS: bsix+=1
        c[('ok' if ok else 'failed','events>0' if evs else 'events=0','bsix>0' if bsix else 'bsix=0')]+=1
        if ok and evs:
            for b in evs:
                mint=b58(b[16:48]); s=d['slot']
                if mint in maxslot and s<maxslot[mint]: c['slot_regress_within_file_order']+=1
                maxslot[mint]=max(maxslot.get(mint,0),s)
                c['ok_events']+=1
            if bsix and len(evs)!=bsix: c['bsix!=nev']+=1
            if len(evs)==1:
                for ix in msg['instructions']:
                    if ks[ix['program_id_index']]==PUMP:
                        b=base64.b64decode(ix['data_b64'])
                        if b[:8] in BS:
                            acc=base64.b64decode(ix['accounts_b64'])
                            if len(acc)>6:
                                c['user==acc6' if b58(evs[0][65:97])==ks[acc[6]] else 'user!=acc6']+=1
print(sess,sorted(c.items(),key=str))
