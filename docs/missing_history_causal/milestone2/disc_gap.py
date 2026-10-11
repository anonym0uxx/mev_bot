"""event_only with no recognized buy/sell instruction: what discriminators do those txs carry? and does the corpus DISC table know them?"""
import sys,json,base64,struct,subprocess,collections,re
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
src=open('/home/alon/build/mev_bot_consol/training/code/src/v2/renormalize_raw.py').read()
KNOWN={}
for m in re.finditer(r"\(PUMP_FUN, bytes\(\[([\d, ]+)\]\)\): \('(\w+)'",src):
    KNOWN[bytes(int(x) for x in m.group(1).split(','))]=m.group(2)
sess=sys.argv[1]; NP=int(sys.argv[2]); D='/mnt/data/mev_bot-artifacts/north_star/capture/'+sess
c=collections.Counter(); logs=collections.Counter(); tot=0
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
        if not any(b[:16]==TR and len(b)>=137 for b in pump): continue
        tot+=1
        known=[b[:8] for b in pump if b[:8] in KNOWN and KNOWN[b[:8]] in ('buy','sell')]
        if known: c['has_known_buy_sell_disc']+=1; continue
        for b in pump:
            if b[:16]!=TR: c[('unknown_disc',b[:8].hex())]+=1
        for L in m['log_messages'] or []:
            if L.startswith('Program log: Instruction:'): logs[L[26:]]+=1
    pr.stdout.close(); pr.wait()
print(sess,'tx with TradeEvent',tot); print(c.most_common(12)); print(logs.most_common(10))
