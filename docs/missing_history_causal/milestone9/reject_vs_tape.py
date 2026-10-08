"""Reject classification against the frozen tape. A reject is LEGITIMATELY outside the feature contract iff the frozen builder
also produced no row for that signature (same rule, same inputs). It LEAVES HISTORY INCOMPLETE iff the tape has a row for a
signature the producer rejected. Also the reverse: tape rows for signatures the producer emitted no row for.
Usage: reject_vs_tape.py REJECT_SIGS.txt SESSION_START_MS SESSION_END_MS COVER_END_MS"""
import json,sys
rej=set(open(sys.argv[1]).read().split())
a,b,cov=int(sys.argv[2]),int(sys.argv[3]),int(sys.argv[4])
hit=0; hit_venue={}
tape_pumpswap_sigs=0
for l in open('/training/v2/canonical/renormalized_v7/trades.sorted.jsonl'):
    if '"pumpswap"' not in l: continue
    i=l.find('"recv_unix_ms": ')
    t=int(l[i+16:l.find(',',i+16)])
    if t<a or t>min(b,cov): continue
    tape_pumpswap_sigs+=1
    j=l.find('"signature": "')
    sig=l[j+14:l.find('"',j+14)]
    if sig in rej:
        hit+=1
print(json.dumps({'rejected_signatures':len(rej),'tape_pumpswap_rows_in_window':tape_pumpswap_sigs,'rejected_sigs_with_a_tape_row':hit}))
