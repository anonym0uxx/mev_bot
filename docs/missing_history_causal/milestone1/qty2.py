import json,base64,subprocess,collections,sys,struct
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58(b):
    n=int.from_bytes(b,'big');s=''
    while n:
        n,r=divmod(n,58);s=A[r]+s
    return '1'*(len(b)-len(b.lstrip(b'\0')))+s
sess=sys.argv[1]; parts=sys.argv[2:]
c=collections.Counter(); ex=[]
for pn in parts:
    f=f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_{sess}_part{pn}.ndjson.zst'
    p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE)
    for l in p.stdout:
        d=json.loads(l)
        if d['record_type']!='transaction': continue
        pl=d['payload'];m=pl['meta'];msg=pl['message']
        if not m['err_is_none']: continue
        ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
        if PUMP not in ks: continue
        flat=list(msg['instructions'])+[i for g in (m['inner_instructions'] or []) for i in g['instructions']]
        evs=[base64.b64decode(i['data_b64']) for i in flat if ks[i['program_id_index']]==PUMP and base64.b64decode(i['data_b64'])[:16]==TR]
        if len(evs)!=1: continue
        b=evs[0]
        if len(b)<233: c['short_event']+=1; continue
        sol,tok=struct.unpack('<QQ',b[48:64]); isb=b[64]; user=b58(b[65:97])
        fee=struct.unpack('<Q',b[177:185])[0]; cfee=struct.unpack('<Q',b[225:233])[0]
        if user not in ks: c['user_not_in_keys']+=1; continue
        i=ks.index(user)
        nd=m['post_balances'][i]-m['pre_balances'][i]
        payer=(i==0); txfee=m['fee'] if payer else 0
        nd2=nd+txfee   # remove tx fee if payer
        gross = sol+fee+cfee if isb else sol-fee-cfee
        want = -gross if isb else gross
        mint=b58(b[16:48])
        key=('buy' if isb else 'sell')
        c[(key,'delta==sol')]+= (abs(nd2)==sol)
        c[(key,'delta==sol+-fees')]+= (nd2==want)
        c[(key,'delta==sol+-fees_exact_or_payer_adj')]+= (nd2==want or nd==want)
        c[(key,'n')]+=1
        if nd2!=want and len(ex)<6: ex.append((key,sol,fee,cfee,nd,nd2,want,payer))
print(sess,sorted(c.items(),key=str)); print(ex)
