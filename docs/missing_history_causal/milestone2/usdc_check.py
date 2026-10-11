import sys,json,base64,struct,subprocess,collections
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
USDC='EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v'
sess=sys.argv[1]; NP=int(sys.argv[2]); c=collections.Counter(); ex=[]
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58(b):
    n=int.from_bytes(b,'big');s=''
    while n:n,r=divmod(n,58);s=A[r]+s
    return '1'*(len(b)-len(b.lstrip(b'\0')))+s
def amt(es,o,mi):
    return sum(int((e.get('ui_token_amount') or {}).get('amount') or 0) for e in es or [] if e.get('owner')==o and e.get('mint')==mi)
for pn in range(NP):
    f=f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_{sess}_part{pn:04d}.ndjson.zst'
    p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,bufsize=1<<20)
    for l in p.stdout:
        if b'"transaction"' not in l[:60]: continue
        d=json.loads(l); pl=d['payload']; m=pl['meta']; msg=pl['message']
        if not m['err_is_none']: continue
        ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
        if PUMP not in ks: continue
        for g in (m['inner_instructions'] or []):
            for ix in g['instructions']:
                if ks[ix['program_id_index']]!=PUMP: continue
                b=base64.b64decode(ix['data_b64'])
                if b[:16]!=TR or len(b)<137: continue
                if struct.unpack('<Q',b[105:113])[0]!=0: continue
                user=b58(b[65:97]); mint=b58(b[16:48]); buy=b[64]; tok=struct.unpack('<Q',b[56:64])[0]
                ud=amt(m['post_token_balances'],user,USDC)-amt(m['pre_token_balances'],user,USDC)
                td=amt(m['post_token_balances'],user,mint)-amt(m['pre_token_balances'],user,mint)
                c[('user_usdc_moves_correct_sign', (ud<0) if buy else (ud>0))]+=1
                c[('user_token_delta==event_qty', td==(tok if buy else -tok))]+=1
                if len(ex)<3: ex.append((('buy' if buy else 'sell'),tok,'usdc_delta',ud,'tok_delta',td,'tail_fee_fields',struct.unpack('<QQ',b[177:193]) if len(b)>=193 else None))
print(sess,dict(c)); print(ex)
