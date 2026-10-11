for s in 20260909_144906_000490 20260909_173928_000596; do zstd -dc /mnt/data/mev_bot-artifacts/north_star/capture/$s/pumpfun_laserstream_events_v1_$s.ndjson.zst | grep -F '"event_type":"create' | python3 -c "
import sys,json
for l in sys.stdin:
    d=json.loads(l);print(json.dumps([d['event_type'],d['venue'],d['slot'],d['recv_unix_ms'],d['mint_b58'],d['curve_account_b58'],d['tx_status']]))
" > /tmp/mh_recon2/work/create_$s.jsonl; done
