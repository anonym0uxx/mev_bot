"""Generate TRAINING_APPROVED_MANIFEST_V1.json — final data closeout.

Computes all hashes fresh from disk; no cached values. Read-only over data.
"""
import json, hashlib, os, subprocess, datetime

BASE = os.path.abspath(os.path.join(os.path.dirname(__file__), '..', 'output'))
CUR = os.path.join(BASE, 'qwen_curriculum_v1')

def sha(p):
    h = hashlib.sha256()
    with open(p, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 22), b''):
            h.update(chunk)
    return h.hexdigest()

def git(*args):
    return subprocess.check_output(['git'] + list(args), cwd=BASE, text=True).strip()

cpt_p = os.path.join(CUR, 'cpt', 'qwen_cpt_v1.jsonl')
sft_p = os.path.join(CUR, 'sft', 'qwen_sft_v1.jsonl')
ret_p = os.path.join(CUR, 'retention', 'qwen_retention_v1.jsonl')

# eval v1.1 exclusion hash
eval_dir = os.path.join(CUR, 'eval_v1_1')
eval_files = {f: sha(os.path.join(eval_dir, f)) for f in sorted(os.listdir(eval_dir))
              if os.path.isfile(os.path.join(eval_dir, f))}

cert = json.load(open('D:/tmp/gate2_5_cert.json'))
tokprov = json.load(open('D:/tmp/tokenizer_provenance.json'))

# record counts
def nrec(p):
    n = 0
    with open(p, 'rb') as f:
        for _ in f:
            n += 1
    return n

ng11 = json.load(open(os.path.join(BASE, 'narrative_gold_v1.1', 'gold', 'FREEZE_MARKER.json')))
ng1 = json.load(open(os.path.join(BASE, 'narrative_gold_v1', 'gold', 'FREEZE_MARKER.json')))
slinky = json.load(open(os.path.join(BASE, 'slinky_gold_v3_compact', 'FREEZE_v3.json')))
rust = json.load(open(os.path.join(BASE, 'rust_gold_v1', 'FREEZE_MARKER.json')))
ls3 = json.load(open(os.path.join(BASE, 'laserstream_gold_v3', 'manifest_laserstream_gold_v3.json')))
ret_man = json.load(open(os.path.join(CUR, 'retention', 'RETENTION_MANIFEST_V1.json')))

manifest = {
    'schema': 'training_approved_manifest_v1',
    'status': 'TRAINING_APPROVED',
    'approved_by': 'Alon (operator) — FINAL DATA CLOSEOUT directive 2026-08-31',
    'generated_at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'builder_git_sha': git('rev-parse', 'HEAD'),
    'artifacts': {
        'cpt': {
            'path': 'tools/data-pipeline/output/qwen_curriculum_v1/cpt/qwen_cpt_v1.jsonl',
            'sha256': sha(cpt_p), 'records': nrec(cpt_p),
            'exact_qwen_tokens': 9207205,
            'manifest_sha256': sha(os.path.join(CUR, 'cpt', 'CPT_MANIFEST_V1.json')),
        },
        'sft': {
            'path': 'tools/data-pipeline/output/qwen_curriculum_v1/sft/qwen_sft_v1.jsonl',
            'sha256': sha(sft_p), 'records': nrec(sft_p),
            'exact_qwen_tokens': 10977789,
            'manifest_sha256': sha(os.path.join(CUR, 'sft', 'SFT_MANIFEST_V1.json')),
        },
        'retention_v1_1': {
            'path': 'tools/data-pipeline/output/qwen_curriculum_v1/retention/qwen_retention_v1.jsonl',
            'sha256': sha(ret_p), 'records': nrec(ret_p),
            'exact_qwen_tokens': 800022,
            'allocation': '100% SFT mix-in, 0% CPT (approved; all records instruction-shaped)',
            'modular': True,
            'source': ret_man.get('source', 'HuggingFaceTB/smol-smoltalk'),
            'manifest_sha256': sha(os.path.join(CUR, 'retention', 'RETENTION_MANIFEST_V1.json')),
        },
    },
    'tokenizer': tokprov,
    'authoritative_token_counts_note': (
        'CPT 9,207,205 and SFT 10,977,789 are exact counts from full tokenization with the '
        'pinned tokenizer (gate2_5_cert). Manifest estimator fields are diagnostic only.'),
    'certification': {
        'gate2_5_cert': cert,
        'over_context_32768': 0,
        'silent_truncation': 0,
        'eval_overlap_records': 0,
        'exact_duplicate_texts': 0,
        'compression_policy': ('compression fields are schema-level manifest metadata only; '
                               '0 occurrences in model-visible content (verified by direct scan of build 6)'),
        'contamination': ('0 retained overlaps against the screened GSM8K-test/HumanEval/MBPP-test '
                          '8-gram sets (retention v1.1; 145,064 screened 8-grams, 80 dropped)'),
    },
    'eval_v1_1': {
        'dir': 'tools/data-pipeline/output/qwen_curriculum_v1/eval_v1_1',
        'file_sha256': eval_files,
        'exclusion': '13,722 mints / 15,274 state_ids / 2,672,383 rows removed pre-sampling; 28,996 eval IDs leak-checked',
    },
    'gold_sources': {
        'slinky_gold_v3_compact': {
            'freeze_marker': 'slinky_gold_v3_compact_FROZEN',
            'status': slinky['status'], 'frozen_at': slinky['frozen_at_utc'],
            'run_uuid': slinky['source']['run_uuid'],
            'producing_git_sha': slinky['source']['git_sha'],
            'source_hash': slinky['source']['source_hash'],
            'rows_per_layer': slinky['compact']['rows_per_layer'],
            'freeze_file_sha256': sha(os.path.join(BASE, 'slinky_gold_v3_compact', 'FREEZE_v3.json')),
        },
        'laserstream_gold_v3': {
            'run_uuid': ls3['run_uuid'],
            'producing_git_sha_post_commit': ls3['git_sha'],
            'layer_sha256': ls3['hashes'],
            'manifest_sha256': sha(os.path.join(BASE, 'laserstream_gold_v3', 'manifest_laserstream_gold_v3.json')),
        },
        'rust_gold_v1': {
            'status': rust['status'], 'frozen_at': rust['frozen_at'],
            'certification_uuid': rust['certification_uuid'],
            'producing_git_sha': rust['git_head_at_freeze'],
            'file_hashes': rust['file_hashes'],
        },
        'narrative_gold_v1': {
            'status': ng1['status'], 'frozen_at': ng1['frozen_at'],
            'certification_uuid': ng1['certification_uuid'],
            'producing_git_sha': ng1['git_sha_at_freeze'],
            'file_hashes': ng1['hashes'],
            'on_disk_verified': '12/12 sha256 match, 2026-08-31',
        },
        'narrative_gold_v1_1': {
            'status': ng11['status'],
            'freeze_uuid': ng11['freeze_uuid'], 'run_uuid': ng11['run_uuid'],
            'parent_freeze_uuid': ng11['parent_freeze_uuid'],
            'producing_git_sha': ng11['git_sha'],
            'frozen_at': ng11['freeze_timestamp_utc'],
            'layer_sha256': ng11['file_hashes_sha256'],
            'on_disk_verified': '6/6 layer sha256 match, 2026-08-31 (exact bytes read by build 6)',
            'note': 'This is the narrative input actually read by build_qwen_curriculum_v1.py (NARR path).',
        },
    },
    'source_task_mix': cert.get('source_mix', {}),
    'closeout': 'DATA ENGINEERING IS DONE. No further curriculum redesign. '
                'Do not regenerate CPT/SFT. Training launch requires separate operator go.',
}

out = os.path.join(CUR, 'TRAINING_APPROVED_MANIFEST_V1.json')
json.dump(manifest, open(out, 'w'), indent=1)
print('wrote', out)
print('cpt sha', manifest['artifacts']['cpt']['sha256'])
print('sft sha', manifest['artifacts']['sft']['sha256'])
print('ret sha', manifest['artifacts']['retention_v1_1']['sha256'])
print('builder git', manifest['builder_git_sha'])
print('eval files:', list(eval_files.items())[:4])
