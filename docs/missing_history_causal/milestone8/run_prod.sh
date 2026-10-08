set -u
# PRODUCTION-PATH comparison: wire (with invocation positions) -> production parser -> ingest_curve_tx + ingest_amm_rows -> join.
# Seed = tape rows with recv < capture start ONLY. In-session events come ONLY from the wire. Band OFF (causal).
# Usage: run_prod.sh TAG   (TAG = prod). Reads /tmp/mh_recon2/slice/sample.tsv (+ extra.tsv if present).
B=/training/mh_build/target/debug/examples/slice_prompt
T=/training/v2/canonical/renormalized_v7/trades.sorted.jsonl
TAG=$1
cat /tmp/mh_recon2/slice/sample.tsv /tmp/mh_recon2/slice/extra.tsv 2>/dev/null > /tmp/mh_recon2/slice/all_$TAG.tsv
while IFS=$'\t' read -r S M C L CL; do
  if [ "$S" = S1 ]; then D=/training/mh_build/wire/s1; SB=1788965346866; else D=/training/mh_build/wire/s2; SB=1788975568113; fi
  OUT=/tmp/mh_recon2/slice/${TAG}_${S}_${M:0:8}
  PS_ROWS=1 SEED_TAPE=$T SEED_BEFORE_MS=$SB $B $D $M $C $L $CL > $OUT.rust.jsonl 2> $OUT.err
  python3 /tmp/mh_recon2/q/fielddiff.py $OUT.rust.jsonl $M $S $OUT.diff.json > $OUT.fd.txt 2>&1
  echo "$S $M $(wc -l < $OUT.rust.jsonl) rows; $(tail -1 $OUT.err | cut -c1-120)"
done < /tmp/mh_recon2/slice/all_$TAG.tsv
