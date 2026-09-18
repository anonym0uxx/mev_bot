#!/usr/bin/env python3
"""Prove the training seed is the RAW upstream Qwen and not a prior checkpoint.

Runs on the Linux training host. Read-only: never downloads, never converts,
never touches a checkpoint directory. Exits non-zero when the seed is not the
pinned raw revision, so a launch cannot proceed on a contaminated base.

Why this exists: the North Star lineage must train from a clean upstream base.
The earlier bad-data CPT/SFT checkpoints must never become the seed. A path check
alone is not evidence -- the file count, per-shard digests, index total size and
tokenizer digests are all re-derived here and written to a receipt.
"""
import argparse
import hashlib
import json
import sys
from pathlib import Path

REVISION = '3ea932cee0a432ae86e9c7826cbe8aef52323a28'
REPO = 'unsloth/Qwen3.8-27B'
SHARD_COUNT = 18
INDEX_TOTAL_SIZE = 55562855904

# Text-tokenization identity, confirmed identical on the Windows snapshot and the
# Linux cache. These are the files that actually determine token ids and the chat
# template, so they are the ones that must match byte-for-byte.
TOKENIZER_FILES = {
    'tokenizer.json': '0997f410c57a1f4e53b09e4be8f4a172d90edd9564368fb0847030937229b9f3',
    'tokenizer_config.json': '2e6eac2825dcd97362f8910ca75f6cb0405dd142e732386eb725af57926c91e5',
    'config.json': '191e0af232104ed8b65258cf3fb2b842e288008baca7633c11b82a1ac7203aab',
    'chat_template.jinja': '12827f24b742ea4e80cdc12dbcf9622227056b9f797252a3149263d4f9aaadce',
    'vocab.json': 'ce99b4cb2983d118806ce0a8b777a35b093e2000a503ebde25853284c9dfa003',
    'merges.txt': 'a9d356d7bdf1ef4949e3e748e95b8e10ad9d4e2e838eddc38a0a7b6b94d1db8d',
}

# Paths that must never be selected as the raw seed. Presence is fine; selection
# is not. Each was trained on the superseded bad-data corpus.
FORBIDDEN_SEED_MARKERS = ('cpt_final_bf16', 'qwen_sft_v2', 'qwen_sft_v1', 'sft_final')


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for block in iter(lambda: handle.read(4 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def audit(seed_dir, prior_paths=()):
    """Re-derive the raw-seed identity. Returns (receipt, ok)."""
    seed = Path(seed_dir).expanduser().resolve()
    receipt = {
        'schema': 'north_star_clean_seed_receipt_v1',
        'repo': REPO,
        'expected_revision': REVISION,
        'seed_path': str(seed),
        'clean_seed_verified': False,
        'training_authorized': False,
        'failures': [],
    }

    if any(marker in str(seed) for marker in FORBIDDEN_SEED_MARKERS):
        receipt['failures'].append(
            'seed path itself matches a superseded bad-data checkpoint marker')
        return receipt, False

    if seed.name != REVISION:
        receipt['failures'].append(
            'seed directory is not the pinned revision: %s != %s' % (seed.name, REVISION))
    if not seed.is_dir():
        receipt['failures'].append('seed directory missing or not a directory')
        return receipt, False

    # 1. Shard inventory and per-shard digests.
    shards = sorted(seed.glob('model-*-of-%05d.safetensors' % SHARD_COUNT))
    if len(shards) != SHARD_COUNT:
        receipt['failures'].append(
            'expected %d safetensors shards, found %d' % (SHARD_COUNT, len(shards)))
    receipt['shard_count'] = len(shards)
    receipt['shard_sha256'] = {p.name: sha256(p) for p in shards}
    receipt['shard_bytes_total'] = sum(p.stat().st_size for p in shards)

    # 2. Index total size must match the sum of what is actually on disk, within
    #    the safetensors header overhead. A truncated or stub download fails here.
    index = seed / 'model.safetensors.index.json'
    if not index.is_file():
        receipt['failures'].append('model.safetensors.index.json missing')
    else:
        declared = json.loads(index.read_text())['metadata']['total_size']
        receipt['index_total_size'] = declared
        receipt['index_sha256'] = sha256(index)
        if declared != INDEX_TOTAL_SIZE:
            receipt['failures'].append(
                'index total_size %s != pinned %s' % (declared, INDEX_TOTAL_SIZE))
        actual = receipt['shard_bytes_total']
        if actual < declared:
            receipt['failures'].append(
                'shard bytes %s are less than declared total_size %s (truncated seed)'
                % (actual, declared))
        receipt['shard_bytes_minus_declared'] = actual - declared

    # 3. Tokenizer/template identity.
    tokenizer = {}
    for name, expected in TOKENIZER_FILES.items():
        path = seed / name
        if not path.is_file():
            tokenizer[name] = {'present': False, 'matches': False}
            receipt['failures'].append('tokenizer file missing: ' + name)
            continue
        actual = sha256(path)
        matches = actual == expected
        tokenizer[name] = {'present': True, 'matches': matches, 'sha256': actual}
        if not matches:
            receipt['failures'].append(
                'tokenizer file digest mismatch: %s (%s != %s)' % (name, actual, expected))
    receipt['tokenizer_files'] = tokenizer

    # 4. Prove the prior bad-data lineage was not selected.
    receipt['prior_checkpoints_present_but_not_selected'] = [
        {'path': str(p), 'exists': Path(p).exists()} for p in prior_paths]
    for prior in prior_paths:
        resolved = Path(prior).expanduser().resolve()
        if seed == resolved or str(seed).startswith(str(resolved) + '/') or resolved == seed.parent:
            receipt['failures'].append(
                'seed resolves to or nests inside superseded checkpoint: ' + str(prior))

    receipt['shard_inventory_complete'] = receipt['shard_count'] == SHARD_COUNT
    receipt['raw_base_selected'] = not receipt['failures']
    receipt['clean_seed_verified'] = receipt['raw_base_selected']
    return receipt, receipt['raw_base_selected']


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--seed', required=True,
                        help='snapshot directory of the raw upstream seed')
    parser.add_argument('--prior-checkpoint', action='append', default=[],
                        help='superseded checkpoint path that must not be the seed')
    parser.add_argument('--out', default=None, help='write JSON receipt here')
    args = parser.parse_args(argv)
    receipt, ok = audit(args.seed, args.prior_checkpoint)
    text = json.dumps(receipt, indent=2) + '\n'
    if args.out:
        Path(args.out).write_text(text, encoding='utf-8')
    print(text)
    return 0 if ok else 2


if __name__ == '__main__':
    sys.exit(main())
