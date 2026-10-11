#!/bin/bash
# Corpus pipeline (renormalize_raw.py, unmodified) over each raw part, in parallel. Output: per-part trades.jsonl
sess=$1; R=/home/alon/build/mev_bot_consol/training/code/src/v2/renormalize_raw.py
D=/mnt/data/mev_bot-artifacts/north_star/capture/$sess; O=/tmp/mh_recon2/ref/$sess; mkdir -p $O
ls $D/pumpfun_laserstream_raw_v1_${sess}_part*.ndjson.zst | xargs -P 48 -I{} bash -c 'f={}; n=$(basename $f .ndjson.zst); n=${n##*_part}; python3 '$R' '$O'/p$n $f > '$O'/p$n.log 2>&1'
cat $O/p*/trades.jsonl > $O/trades.jsonl; wc -l $O/trades.jsonl
