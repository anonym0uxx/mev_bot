set -u
# Runs the production slice (seeded < capture start, in-session streamed, band OFF = causal path) and the field diff for each sampled mint.
B=/training/mh_build/target/debug/examples/slice_prompt
T=/training/v2/canonical/renormalized_v7/trades.sorted.jsonl
python3 - <<'PYEOF'
import json
d=json.load(open('/tmp/mh_recon2/slice/sample.json'))
with open('/tmp/mh_recon2/slice/sample.tsv','w') as f:
    for s,L in d.items():
        for x in L: f.write('%s\t%s\t%s\t%d\t%s\n'%(s,x['mint'],x['creator'],x['launch_ms'],','.join(map(str,x['clocks']))))
PYEOF
while IFS=$'\t' read -r S M C L CL; do
  if [ "$S" = S1 ]; then D=/tmp/mh_recon2/m1/s1; SB=1788965346866; else D=/tmp/mh_recon2/m1/s2; SB=1788975568113; fi
  OUT=/tmp/mh_recon2/slice/samp_${S}_${M:0:8}
  SEED_TAPE=$T SEED_BEFORE_MS=$SB $B $D $M $C $L $CL > $OUT.rust.jsonl 2> $OUT.err
  python3 /tmp/mh_recon2/q/fielddiff.py $OUT.rust.jsonl $M $S $OUT.diff.json > $OUT.fd.txt 2>&1
  echo "$S $M $(wc -l < $OUT.rust.jsonl) rows; $(tail -1 $OUT.err | cut -c1-80)"
done < /tmp/mh_recon2/slice/sample.tsv
