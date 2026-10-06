"""Raw capture records -> the daemon's NDJSON wire lines (tools/stream-capture-rs main.rs::daemon_tx_line contract),
plus meta.tx_ok. Harness shim ONLY: all decoding/ingest after this is the production Rust path.
Slice rule (pre-registered): parts 0000..0019 of a session (prefix of the capture; starts at session start).
Usage: to_wire.py SESSION OUTDIR [NPARTS=20]
"""
import sys,json,subprocess,os
from multiprocessing import Pool
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
SW='pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA'
import base64
def tb(v):
    return [{"mint":e["mint"],"owner":e["owner"],"amount":(e.get("ui_token_amount") or {}).get("amount") or "0"} for e in (v or [])]
def part(a):
    sess,pn,outdir=a
    f=f'/mnt/data/mev_bot-artifacts/north_star/capture/{sess}/pumpfun_laserstream_raw_v1_{sess}_part{pn:04d}.ndjson.zst'
    out=open(f'{outdir}/wire_{pn:04d}.ndjson','w')
    p=subprocess.Popen(['zstd','-dc',f],stdout=subprocess.PIPE,bufsize=1<<20)
    n=0
    for l in p.stdout:
        d=json.loads(l);rt=d['record_type'];pl=d['payload']
        if rt=='account':
            if pl['owner_b58']==PUMP and pl['data_len']==151:
                out.write(json.dumps({"lane":"laserstream","kind":"account","slot":d['slot'],"recv_unix_ms":d['recv_unix_ms'],"pubkey_b58":pl['pubkey_b58'],"owner_b58":pl['owner_b58'],"data_b64":pl['data_b64'],"ri":d['record_index']})+'\n');n+=1
        elif rt=='transaction':
            m=pl['meta'];msg=pl['message']
            keys=msg['account_keys_b58']+m['loaded_writable_addresses_b58']+m['loaded_readonly_addresses_b58']
            if PUMP not in keys: continue
            def ixj(i):
                return {"program_b58":keys[i['program_id_index']],"data_b64":i['data_b64'],"accounts":list(base64.b64decode(i['accounts_b64']))}
            ixs=[ixj(i) for i in msg['instructions']]
            for g in (m['inner_instructions'] or []):
                ixs+= [ixj(i) for i in g['instructions']]
            out.write(json.dumps({"lane":"laserstream","kind":"transaction","slot":d['slot'],"recv_unix_ms":d['recv_unix_ms'],"signature_b58":pl['signature_b58'],"account_keys":keys,"instructions":ixs,"meta":{"fee":m['fee'],"compute_units_consumed":m['compute_units_consumed'],"tx_ok":(1 if m["err_is_none"] else 0),"pre_balances":m["pre_balances"],"post_balances":m["post_balances"],"pre_token_balances":tb(m["pre_token_balances"]),"post_token_balances":tb(m["post_token_balances"])},"ri":d['record_index']})+'\n');n+=1
    p.wait();out.close();return pn,n
if __name__=='__main__':
    sess,outdir=sys.argv[1],sys.argv[2];npart=int(sys.argv[3]) if len(sys.argv)>3 else 20
    os.makedirs(outdir,exist_ok=True)
    with Pool(20) as P: r=sorted(P.map(part,[(sess,i,outdir) for i in range(npart)]))
    print(sess,sum(n for _,n in r),'lines')
