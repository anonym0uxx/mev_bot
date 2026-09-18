#!/usr/bin/env python3
"""Manifest-bound real loader census. CPU only; never loads model weights."""
import argparse
import json
from pathlib import Path
from transformers import AutoTokenizer
from train_qwen27b import load_release, prepare_datasets, dataset_report


def main():
    p=argparse.ArgumentParser()
    p.add_argument('--release_manifest',required=True)
    p.add_argument('--output',required=True)
    args=p.parse_args()
    release=load_release(args.release_manifest,real_training=False)
    tok=AutoTokenizer.from_pretrained(release['tokenizer_path'],local_files_only=True)
    reports={}
    for phase in release['phases']:
        train,val,geometry=prepare_datasets(release,phase,tok)
        reports[phase]=dataset_report(release,phase,train,val,geometry)
    Path(args.output).write_text(json.dumps(reports,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(reports,indent=2))


if __name__=='__main__':
    main()
