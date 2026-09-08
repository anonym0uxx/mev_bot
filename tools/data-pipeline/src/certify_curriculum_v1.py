#!/usr/bin/env python
"""Gate 2 tokenizer certification + Gate 5 verification on final artifacts."""
import json, hashlib, re, os, sys
import numpy as np
from transformers import AutoTokenizer
from huggingface_hub import HfApi

BASE = 'D:/repos/mev_bot/tools/data-pipeline/output/qwen_curriculum_v1'
TOKENIZER_ID = 'unsloth/Qwen3.8-27B'
CONTEXT_LIMIT = 32768

tok = AutoTokenizer.from_pretrained(TOKENIZER_ID)
tok_rev = HfApi().model_info(TOKENIZER_ID).sha
print(f'tokenizer: {TOKENIZER_ID} rev={tok_rev} vocab={len(tok)}', flush=True)

# ---- eval exclusion IDs ----
eval_ids = set()
for f in ('eval/qwen_eval_v1.jsonl', 'eval_v1_1/qwen_eval_v1_1.jsonl'):
    for line in open(os.path.join(BASE, f), encoding='utf-8'):
        r = json.loads(line)
        def collect(o):
            if isinstance(o, dict):
                for k, v in o.items():
                    if k in ('mint_id', 'mint_id_raw', 'state_id', 'outcome_state_id') and isinstance(v, str) and v:
                        eval_ids.add(v)
                    else:
                        collect(v)
            elif isinstance(o, list):
                for x in o:
                    collect(x)
        collect(r)
print(f'eval exclusion IDs: {len(eval_ids)}', flush=True)

META_PAT = re.compile(r'compression|zstd|snappy|parquet_file|file_offset|row_group', re.I)

def visible_text(rec):
    if 'content' in rec:
        return rec['content']
    parts = [rec.get('instruction', '')]
    parts.append(json.dumps(rec.get('input', ''), ensure_ascii=False) if not isinstance(rec.get('input'), str) else rec['input'])
    out = rec.get('output', '')
    parts.append(json.dumps(out, ensure_ascii=False) if not isinstance(out, str) else out)
    return '\n'.join(parts)

report = {}
for tag, path in (('CPT', 'cpt/qwen_cpt_v1.jsonl'), ('SFT', 'sft/qwen_sft_v1.jsonl')):
    full = os.path.join(BASE, path)
    raw = open(full, 'rb').read()
    sha = hashlib.sha256(raw).hexdigest()
    lens, chars = [], 0
    leak_hits, meta_hits, manifest_tok = 0, 0, 0
    ids_seen = set()
    dup_hash = {}
    for line in raw.decode('utf-8').splitlines():
        if not line.strip():
            continue
        rec = json.loads(line)
        txt = visible_text(rec)
        h = hashlib.sha1(txt.encode()).hexdigest()
        dup_hash[h] = dup_hash.get(h, 0) + 1
        n = len(tok.encode(txt))
        lens.append(n); chars += len(txt)
        manifest_tok += rec.get('token_count', 0)
        for eid in eval_ids:
            if eid in txt:
                leak_hits += 1
                break
        if META_PAT.search(txt):
            meta_hits += 1
        ids_seen.add(rec.get('id'))
    a = np.array(lens)
    dups = sum(v - 1 for v in dup_hash.values() if v > 1)
    report[tag] = {
        'records': len(lens),
        'sha256': sha,
        'tokens_true': int(a.sum()),
        'tokens_manifest_field_sum': manifest_tok,
        'p50': int(np.percentile(a, 50)), 'p90': int(np.percentile(a, 90)),
        'p95': int(np.percentile(a, 95)), 'p99': int(np.percentile(a, 99)),
        'max': int(a.max()),
        'chars_per_token': round(chars / a.sum(), 3),
        'over_context_32768': int((a > CONTEXT_LIMIT).sum()),
        'eval_id_leak_records': leak_hits,
        'compression_meta_records': meta_hits,
        'exact_dup_texts': int(dups),
        'unique_ids': len(ids_seen) == len(lens),
    }
    print(json.dumps({tag: report[tag]}, indent=2), flush=True)

# manifest cross-check
for tag, mf in (('CPT', 'cpt/CPT_MANIFEST_V1.json'), ('SFT', 'sft/SFT_MANIFEST_V1.json')):
    m = json.load(open(os.path.join(BASE, mf), encoding='utf-8'))
    pref = m.get('file_hash') or m.get('sha256') or m.get('hash', '')
    report[tag]['manifest_hash_match'] = report[tag]['sha256'].startswith(pref[:32]) if pref else None
    report[tag]['manifest_source_mix'] = m.get('source_composition') or m.get('source_counts')

report['tokenizer'] = {'id': TOKENIZER_ID, 'revision': tok_rev, 'vocab': len(tok), 'context_limit_checked': CONTEXT_LIMIT}
json.dump(report, open('/tmp/gate2_5_cert.json', 'w'), indent=2)
print('WROTE /tmp/gate2_5_cert.json', flush=True)
print('DONE', flush=True)
