"""Zero-reserve events = USDC-quoted curve trades? Search the TradeEvent tail for the quote amount.
For each vs==0 event: user's USDC delta (token balances) vs every u64 in the event tail; report which offset matches.
"""
import sys,json,base64,struct,subprocess,collections
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
USDC='EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v'
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58(b):
    n=int.from_bytes(b,'big'); o=''
    while n: n,r=divmod(n,58); o=A[r]+o
    return '1'*(len(b)-len(b.lstrip(b'\0')))+o
sess=sys.argv[1]; NP=int(sys.argv[2]); D='/mnt/data/mev_bot-artifacts/north_star/capture/'+sess
def amt(entries,owner,mint):
    for e in entries or []:
        if e.get('owner')==owner and e.get('mint')==mint:
            try: return int((e.get('ui_token_amount') or {}).get('amount') or 0)
            except Exception: return 0
    return 0
hits=collections.Counter(); n=0; fields=collections.Counter(); lens=collections.Counter(); ex=[]
for p in range(NP):
    f=f'{D}/pumpfun_laserstream_raw_v1_{sess}_part{p:04d}.ndjson.zst'
    pr=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
    for l in pr.stdout:
        if b'"transaction"' not in l[:60]: continue
        d=json.loads(l); pl=d['payload']; m=pl['meta']
        if not m['err_is_none']: continue
        msg=pl['message']; keys=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
        for g in m['inner_instructions'] or []:
            for i in g['instructions']:
                if keys[i['program_id_index']]!=PUMP: continue
                b=base64.b64decode(i['data_b64'])
                if b[:16]!=TR or len(b)<137: continue
                sol=struct.unpack('<Q',b[48:56])[0]
                vs=struct.unpack('<Q',b[105:113])[0]
                if vs!=0 or sol!=0: continue
                n+=1; lens[len(b)]+=1
                user=b58(b[65:97]); pre=m['pre_token_balances']; post=m['post_token_balances']
                ud=abs(amt(post,user,USDC)-amt(pre,user,USDC))
                tail=b[137:]
                u64s=[struct.unpack('<Q',tail[o:o+8])[0] for o in range(0,len(tail)-7,8)]
                found=[(o*8+137) for o,v in enumerate(u64s) if ud and v==ud]
                for o in found: hits[o]+=1
                if not found: hits['none']+=1
                if len(ex)<3: ex.append((ud,[v for v in u64s if v][:6],len(b)))
    pr.stdout.close(); pr.wait()
print(sess,'zero events',n,'tail offset matching |user USDC delta|:',dict(hits)); print('lens',dict(lens)); print(ex)
