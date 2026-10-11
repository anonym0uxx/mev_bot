"""Invocation structure of curve TradeEvents: outer vs inner emitter, events per emitting instruction, event adjacency,
position of the event relative to its emitter, and whether any event has NO preceding pump instruction in its group."""
import sys,json,base64,struct,subprocess,collections
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
sess=sys.argv[1]; NP=int(sys.argv[2]); D='/mnt/data/mev_bot-artifacts/north_star/capture/'+sess
c=collections.Counter(); per_tx=collections.Counter()
for p in range(NP):
    f=f'{D}/pumpfun_laserstream_raw_v1_{sess}_part{p:04d}.ndjson.zst'
    pr=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
    for l in pr.stdout:
        if b'"transaction"' not in l[:60]: continue
        d=json.loads(l); pl=d['payload']; m=pl['meta']
        if not m['err_is_none']: continue
        msg=pl['message']; ks=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
        if PUMP not in ks: continue
        inner={g['index']:g['instructions'] for g in (m['inner_instructions'] or [])}
        txev=0
        for oi,o in enumerate(msg['instructions']):
            seq=[('o',o)]+[('i',x) for x in inner.get(oi,[])]
            pump=[(k,base64.b64decode(ix['data_b64'])) for k,ix in seq if ks[ix['program_id_index']]==PUMP]
            last=None; n_after=0
            for k,b in pump:
                if b[:16]==TR and len(b)>=137:
                    txev+=1
                    if last is None: c['event_with_no_preceding_pump_ix_in_group']+=1
                    else:
                        n_after+=1; c[('event_after_emitter_kind',last[0],'event_kind',k)]+=1
                else:
                    if last is not None and n_after: c[('events_per_emitter',n_after)]+=1
                    last=(k,b[:8].hex()); n_after=0
            if last is not None and n_after: c[('events_per_emitter',n_after)]+=1
        if txev: per_tx[min(txev,5)]+=1
    pr.stdout.close(); pr.wait()
print(sess); [print(' ',n,k) for k,n in sorted(c.items(),key=lambda x:-x[1])]
print('  events per tx (5=5+):',dict(sorted(per_tx.items())))
