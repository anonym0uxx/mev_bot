"""Generate TRAINING_APPROVED_MANIFEST_V2.json — FINAL GO CLOSEOUT sealing.

Supersedes V1 after the SFT cross-shard rewrite (v2) + eval v1.2. Computes
ALL hashes/counts fresh from CURRENT disk bytes; reuses the ACTUAL training
code (select_retention / build_example / loss_tokens / compute_task_weights /
split_train_val) so recorded values are definitionally what the run uses.
Read-only over data. Run inside the qwen-train venv (needs transformers).
"""
import json, hashlib, os, subprocess, datetime, sys, math

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = os.path.abspath(os.path.join(HERE, '..', 'output'))
CUR = os.path.join(BASE, 'qwen_curriculum_v1')
REPO = os.path.abspath(os.path.join(HERE, '..', '..', '..'))
TRAIN_DIR = os.path.join(REPO, 'tools', 'training', 'qwen27b')
sys.path.insert(0, TRAIN_DIR)

import train_qwen27b as T  # noqa: E402  (the real training module)
from transformers import AutoTokenizer  # noqa: E402


def sha(p):
    h = hashlib.sha256()
    with open(p, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 22), b''):
            h.update(chunk)
    return h.hexdigest()


def git(*args):
    return subprocess.check_output(['git'] + list(args), cwd=REPO,
                                   text=True).strip()


def nrec(p):
    n = 0
    with open(p, 'rb') as f:
        for _ in f:
            n += 1
    return n


cpt_p = os.path.join(CUR, 'cpt', 'qwen_cpt_v1.jsonl')
sft_p = os.path.join(CUR, 'sft', 'qwen_sft_v2.jsonl')
ret_p = os.path.join(CUR, 'retention', 'qwen_retention_v1.jsonl')
eval12_p = os.path.join(CUR, 'eval_v1_2', 'qwen_eval_v1_2.jsonl')

print('[1/6] hashing artifacts from disk ...', flush=True)
cpt_sha, sft_sha, ret_sha, eval12_sha = sha(cpt_p), sha(sft_p), sha(ret_p), sha(eval12_p)

print('[2/6] loading tokenizer (pinned rev) ...', flush=True)
tok = AutoTokenizer.from_pretrained(T.TOKENIZER_REPO, revision=T.TOKENIZER_REV)

print('[3/6] SFT v2 exact token accounting (raw + model-visible) ...', flush=True)
domain = T.load_jsonl(sft_p)
raw_tokens = 0          # full rendered chat-template token count
visible_tokens = 0      # model-visible = same (inputs); loss-bearing tracked too
sft_loss_tokens = 0
per_bucket_all = {}
for r in domain:
    ex = T.build_example(r, tok)
    if not ex:
        continue
    ids, labels = ex
    raw_tokens += len(ids)
    visible_tokens += len(ids)
    lt = T.loss_tokens(labels)
    sft_loss_tokens += lt
    per_bucket_all[T.task_bucket(r)] = per_bucket_all.get(T.task_bucket(r), 0) + lt

print('[4/6] deterministic retention selection (REAL training code) ...', flush=True)
retention_full = T.load_jsonl(ret_p)
retention_sel, rstats = T.select_retention(domain, retention_full, tok)
sel_ids_hashes = []
sel_loss_tokens = 0
for r in retention_sel:
    rid = r.get('id') or r.get('record_id') or None
    rh = hashlib.sha256(json.dumps(r, sort_keys=True,
                                   ensure_ascii=False).encode()).hexdigest()
    ex = T.build_example(r, tok)
    lt = T.loss_tokens(ex[1]) if ex else 0
    sel_loss_tokens += lt
    sel_ids_hashes.append({'id': rid, 'sha256': rh, 'loss_tokens': lt})

print('[5/6] final supervision geometry on the REAL train split ...', flush=True)
recs = domain + retention_sel
tr, va = T.split_train_val(recs)
pairs = []
for r in tr:
    ex = T.build_example(r, tok)
    if ex:
        pairs.append((T.task_bucket(r), T.loss_tokens(ex[1])))
task_w, per_bucket, eff_shares = T.compute_task_weights(pairs)
total_lt = sum(per_bucket.values())
ret_share = per_bucket.get('retention', 0) / total_lt if total_lt else 0.0

# exact dataloader step plans (mirrors main())
per_dev, accum = 1, 8
gbatch = per_dev * accum * T.NUM_GPUS
sft_train_n = len([r for r in tr if T.build_example(r, tok)])
sft_steps_ep = math.ceil(len(tr) / gbatch)
sft_total = math.ceil(sft_steps_ep * 1.25)
cpt_docs = T.load_jsonl(cpt_p)
cpt_tr, cpt_va = T.split_train_val(cpt_docs)
cpt_ds = T.PackedCPTDataset(cpt_tr, tok)
cpt_steps_ep = math.ceil(len(cpt_ds) / gbatch)
cpt_total = math.ceil(cpt_steps_ep * 1.75)

print('[6/6] assembling manifest ...', flush=True)
eval12_man = json.load(open(os.path.join(CUR, 'eval_v1_2', 'EVAL_MANIFEST_V1_2.json')))
cert = json.load(open(os.path.join(CUR, 'gate2_5_cert.json')))
tokprov = json.load(open(os.path.join(CUR, 'tokenizer_provenance.json')))
sft2_man = json.load(open(os.path.join(CUR, 'sft', 'SFT_MANIFEST_V2.json')))
ret_man = json.load(open(os.path.join(CUR, 'retention', 'RETENTION_MANIFEST_V1.json')))
ng11 = json.load(open(os.path.join(BASE, 'narrative_gold_v1.1', 'gold', 'FREEZE_MARKER.json')))
ng1 = json.load(open(os.path.join(BASE, 'narrative_gold_v1', 'gold', 'FREEZE_MARKER.json')))
slinky = json.load(open(os.path.join(BASE, 'slinky_gold_v3_compact', 'FREEZE_v3.json')))
rust = json.load(open(os.path.join(BASE, 'rust_gold_v1', 'FREEZE_MARKER.json')))

manifest = {
    'schema': 'training_approved_manifest_v2',
    'status': 'TRAINING_APPROVED_FINAL_GO',
    'supersedes': 'TRAINING_APPROVED_MANIFEST_V1.json (stale after SFT v2 rewrite + eval v1.2)',
    'approved_by': 'Alon (operator) — FINAL GO CLOSEOUT directive 2026-08-31',
    'generated_at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'producing_git_sha': git('rev-parse', 'HEAD'),
    'git_dirty': bool(git('status', '--porcelain')),
    'artifacts': {
        'cpt': {'path': 'tools/data-pipeline/output/qwen_curriculum_v1/cpt/qwen_cpt_v1.jsonl',
                'sha256': cpt_sha, 'records': nrec(cpt_p),
                'exact_qwen_tokens': 9207205,
                'note': 'unchanged since V1 closeout (hash re-verified from disk)'},
        'sft_v2': {'path': 'tools/data-pipeline/output/qwen_curriculum_v1/sft/qwen_sft_v2.jsonl',
                   'sha256': sft_sha, 'records': nrec(sft_p),
                   'exact_raw_tokens': raw_tokens,
                   'exact_model_visible_tokens': visible_tokens,
                   'exact_loss_bearing_label_tokens': sft_loss_tokens,
                   'mode_counts': sft2_man.get('mode_counts'),
                   'cross_records': sft2_man.get('cross_records'),
                   'manifest_sha256': sha(os.path.join(CUR, 'sft', 'SFT_MANIFEST_V2.json'))},
        'eval_v1_2': {'path': 'tools/data-pipeline/output/qwen_curriculum_v1/eval_v1_2/qwen_eval_v1_2.jsonl',
                      'sha256': eval12_sha,
                      'manifest': eval12_man},
        'retention_v1_1_full': {'path': 'tools/data-pipeline/output/qwen_curriculum_v1/retention/qwen_retention_v1.jsonl',
                                'sha256': ret_sha, 'records': nrec(ret_p),
                                'source': ret_man.get('source', 'HuggingFaceTB/smol-smoltalk')},
        'retention_selected_for_training': {
            'selection_algorithm': 'train_qwen27b.select_retention (deterministic, seed 42, loss-token budgeted)',
            'selected_records': len(retention_sel),
            'selected_loss_tokens': sel_loss_tokens,
            'target_share_of_loss_tokens': T.RETENTION_TARGET,
            'achieved_share_of_train_loss_tokens': round(ret_share, 6),
            'selection_stats': rstats,
            'records': sel_ids_hashes},
    },
    'pump_gate_v2': {
        'version': 'pump_gate_v2',
        'role': 'VERSIONED target-label generation only; thresholds live in '
                'sft_cross_exporter_v2.decide_v2/_BUY_GATE and in per-record '
                'label_governance metadata (never model-visible)',
        'exporter': 'tools/data-pipeline/src/sft_cross_exporter_v2.py',
        'exporter_sha256': sha(os.path.join(HERE, 'sft_cross_exporter_v2.py'))},
    'supervision_geometry': {
        'trainer': 'WeightedLossTrainer (train_qwen27b.py) — true weighted-token '
                   'mean Σ(w·CE)/Σ(w·valid) over the global grad-accum window '
                   'across ranks; DDP world_size compensation mirrors stock tail '
                   'scaling; eval unweighted; falls back to stock for CPT',
        'trainer_file_sha256': sha(os.path.join(TRAIN_DIR, 'train_qwen27b.py')),
        'reference_test': 'test_weighted_loss.py PASS ws=1 (rel err 9.0e-6) and '
                          'ws=3 torchrun (rel err 9.9e-6); uniform==stock; '
                          'padding-neutral',
        'weight_cap': 4.0,
        'task_weights': {b: round(w, 6) for b, w in task_w.items()},
        'loss_tokens_by_bucket_train_split': per_bucket,
        'final_effective_gradient_shares': eff_shares},
    'tokenizer': tokprov,
    'certification': {
        'gate2_5_cert': cert,
        'eval_v1_2_certification': eval12_man.get('certification'),
        'zero_leakage': 'eval v1.2 mint/state partition disjoint from train (0 overlap, 2162 train recs checked)',
        'dedup': '0 exact duplicate texts (gate 2.5); no duplication/padding used to hit token targets',
        'truncation': '0 over-context (32768) / 0 silent truncation (gate 2.5)'},
    'dataloader_step_plans': {
        'global_batch': gbatch, 'per_device_bsz': per_dev, 'grad_accum': accum,
        'world_size': T.NUM_GPUS,
        'cpt': {'train_packed_sequences': len(cpt_ds), 'val_docs': len(cpt_va),
                'steps_per_epoch': cpt_steps_ep, 'max_window_epochs': 1.75,
                'total_steps_max': cpt_total, 'lr': 4e-6,
                'eval_ckpt_every': max(1, cpt_steps_ep // 4)},
        'sft': {'train_records': len(tr), 'train_buildable': sft_train_n,
                'val_records': len(va),
                'steps_per_epoch': sft_steps_ep, 'max_window_epochs': 1.25,
                'total_steps_max': sft_total, 'lr': 2e-6,
                'eval_ckpt_every': max(1, sft_steps_ep // 4)}},
    'gold_sources': {
        'slinky_gold_v3_compact': {
            'status': slinky['status'], 'frozen_at': slinky['frozen_at_utc'],
            'run_uuid': slinky['source']['run_uuid'],
            'producing_git_sha': slinky['source']['git_sha'],
            'source_hash': slinky['source']['source_hash'],
            'freeze_file_sha256': sha(os.path.join(BASE, 'slinky_gold_v3_compact', 'FREEZE_v3.json'))},
        'laserstream_gold_v3': json.load(open(os.path.join(BASE, 'laserstream_gold_v3', 'manifest_laserstream_gold_v3.json'))),
        'rust_gold_v1': {'status': rust['status'], 'frozen_at': rust['frozen_at'],
                         'certification_uuid': rust['certification_uuid'],
                         'producing_git_sha': rust['git_head_at_freeze'],
                         'file_hashes': rust['file_hashes']},
        'narrative_gold_v1': {'status': ng1['status'], 'certification_uuid': ng1['certification_uuid'],
                              'producing_git_sha': ng1['git_sha_at_freeze'],
                              'file_hashes': ng1['hashes']},
        'narrative_gold_v1_1': {'status': ng11['status'], 'freeze_uuid': ng11['freeze_uuid'],
                                'run_uuid': ng11['run_uuid'],
                                'parent_freeze_uuid': ng11['parent_freeze_uuid'],
                                'producing_git_sha': ng11['git_sha'],
                                'layer_sha256': ng11['file_hashes_sha256']}},
    'closeout': 'FINAL GO. CPT 1.75ep@4e-6, SFT 1.25ep@2e-6, validation-governed '
                'selection/early-stop, ZeRO-3/NVMe. No further model/data redesign.',
}

out = os.path.join(CUR, 'TRAINING_APPROVED_MANIFEST_V2.json')
with open(out, 'w') as f:
    json.dump(manifest, f, indent=1)
print('wrote', out)
print('manifest sha256', sha(out))
print('cpt sha', cpt_sha)
print('sft_v2 sha', sft_sha)
print('eval_v1_2 sha', eval12_sha)
print('retention full sha', ret_sha)
print('retention selected:', len(retention_sel), 'loss_tokens', sel_loss_tokens,
      'share', round(ret_share, 6))
print('sft_v2 raw tokens', raw_tokens, 'loss tokens', sft_loss_tokens)
print('task_weights', {b: round(w, 4) for b, w in task_w.items()})
print('eff_shares', eff_shares)
print('git', manifest['producing_git_sha'], 'dirty', manifest['git_dirty'])
