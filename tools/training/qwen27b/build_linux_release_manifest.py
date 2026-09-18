#!/usr/bin/env python3
"""Rebind the verified candidate release to Linux paths for native DeepSpeed.

Runs on the Linux training host. Read-only against every source it inspects.

Two things this fixes that a naive Windows->Linux copy cannot:

1. The Windows manifest pins the tokenizer at a C:\\ path that does not exist on
   Linux, and it pins two image preprocessor files that are absent from the Linux
   snapshot. This builder pins only the text-tokenization set, hashes it on the
   Linux side, and refuses to continue unless every shared file matches the
   Windows-verified digest byte for byte.

2. The Windows manifest asserted admission.clean_seed = true as a bare boolean.
   An assertion is not evidence. This builder sets clean_seed only from a
   SHA256-bound clean-seed receipt that independently re-derived the raw seed
   inventory, and it records that receipt's hash in the manifest.

Every data binding keeps its original digest; the builder re-hashes each file at
its mapped Linux path and fails closed on any mismatch or size drift.
"""
import argparse
import hashlib
import json
import sys
from pathlib import Path

# Files that decide token ids and chat rendering. Deliberately NOT including the
# Windows-only preprocessor_config.json / video_preprocessor_config.json, which
# do not exist on the Linux snapshot and are irrelevant to text training.
TEXT_TOKENIZER_FILES = (
    'chat_template.jinja',
    'config.json',
    'merges.txt',
    'tokenizer.json',
    'tokenizer_config.json',
    'vocab.json',
)


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for block in iter(lambda: handle.read(4 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def translate(value, mapping):
    """Map a Windows absolute path onto its Linux mount using explicit prefixes."""
    text = str(value).replace('\\', '/')
    for source, target in mapping:
        if text.lower().startswith(source.lower()):
            return target + text[len(source):]
    return None


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--windows-manifest', required=True,
                        help='the verified CANDIDATE_RELEASE.json built on Windows')
    parser.add_argument('--path-map', action='append', required=True,
                        metavar='WINDOWS_PREFIX=LINUX_PREFIX',
                        help='repeatable; e.g. "D:/mev_bot-artifacts=/mnt/d/mev_bot-artifacts"')
    parser.add_argument('--tokenizer-dir', required=True,
                        help='raw upstream snapshot directory on this host')
    parser.add_argument('--seed-receipt', required=True,
                        help='CLEAN_SEED_RECEIPT.json produced by verify_clean_seed.py')
    parser.add_argument('--out', required=True)
    parser.add_argument('--accept-host-runtime', action='store_true',
                        help='record this host\'s actual package versions instead of '
                             'requiring them to equal the Windows-pinned runtime. The '
                             'tokenizer stack must still match; only host-runtime '
                             'packages may differ, and the difference is reported.')
    args = parser.parse_args(argv)

    mapping = []
    for entry in args.path_map:
        source, _, target = entry.partition('=')
        if not source or not target:
            raise SystemExit('bad --path-map entry: ' + entry)
        mapping.append((source.replace('\\', '/').rstrip('/'),
                        target.replace('\\', '/').rstrip('/')))

    windows = json.loads(Path(args.windows_manifest).read_text(encoding='utf-8'))
    failures = []

    # --- clean seed: evidence, not assertion ---------------------------------
    receipt_path = Path(args.seed_receipt)
    receipt = json.loads(receipt_path.read_text(encoding='utf-8'))
    seed_ok = (receipt.get('clean_seed_verified') is True
               and receipt.get('raw_base_selected') is True
               and not receipt.get('failures'))
    if not seed_ok:
        failures.append('clean-seed receipt does not verify the raw upstream base')

    # --- tokenizer -----------------------------------------------------------
    tokenizer_dir = Path(args.tokenizer_dir).expanduser().resolve()
    pinned = windows['tokenizer']['files']
    tokenizer_files = {}
    for name in TEXT_TOKENIZER_FILES:
        candidate = tokenizer_dir / name
        if not candidate.is_file():
            failures.append('tokenizer file missing on Linux: ' + name)
            continue
        actual = sha256(candidate)
        tokenizer_files[name] = actual
        if name in pinned and pinned[name] != actual:
            failures.append('tokenizer digest mismatch %s: %s != %s'
                            % (name, actual, pinned[name]))
    dropped = sorted(set(pinned) - set(TEXT_TOKENIZER_FILES))
    if dropped:
        print('[info] not pinned on Linux (absent from snapshot): %s' % ', '.join(dropped))

    def bind(entry, label):
        """Re-hash a Windows binding at its Linux path; fail closed on drift."""
        target = translate(entry['path'], mapping)
        if target is None:
            failures.append('no path mapping for %s (%s)' % (label, entry['path']))
            return None
        path = Path(target)
        if not path.is_file():
            failures.append('missing at mapped path: %s' % target)
            return None
        actual = sha256(path)
        if actual != entry['sha256']:
            failures.append('digest drift %s: %s != %s' % (target, actual, entry['sha256']))
            return None
        if path.stat().st_size != entry['bytes']:
            failures.append('size drift %s' % target)
            return None
        bound = dict(entry)
        bound['path'] = str(path)
        bound['windows_path'] = entry['path']
        return bound

    exclusion = bind(windows['exclusion_union'], 'exclusion_union')
    phases = {}
    for phase, block in windows['phases'].items():
        phases[phase] = {'task_targets': block['task_targets']}
        for partition in ('train', 'validation'):
            bound = [bind(item, '%s/%s' % (phase, partition)) for item in block[partition]]
            if any(item is None for item in bound):
                continue
            phases[phase][partition] = bound

    # --- runtime: the loader compares installed versions to this block ---------
    import importlib.metadata as metadata
    tokenizer_stack = ('transformers', 'tokenizers')
    required_runtime = windows['runtime']
    installed = {}
    for package in required_runtime:
        try:
            installed[package] = metadata.version(package)
        except metadata.PackageNotFoundError:
            installed[package] = None
            failures.append('runtime package missing on this host: ' + package)
    runtime = dict(required_runtime)
    differing = {p: (required_runtime[p], installed[p])
                 for p in required_runtime if installed[p] != required_runtime[p]}
    for package in tokenizer_stack:
        if package in differing and not failures:
            failures.append(
                'tokenizer stack differs from the validated runtime: %s %s != %s'
                % (package, differing[package][1], differing[package][0]))
    if differing and args.accept_host_runtime:
        for package in differing:
            runtime[package] = installed[package]
    elif differing:
        for package, (pinned, actual) in sorted(differing.items()):
            failures.append(
                'host runtime differs for %s: installed %s, pinned %s '
                '(pass --accept-host-runtime to record the host versions)'
                % (package, actual, pinned))

    manifest = {
        'schema_version': windows['schema_version'],
        'release_id': windows['release_id'] + '_linux',
        'derived_from': {'windows_manifest': str(Path(args.windows_manifest).resolve()),
                         'windows_manifest_sha256': sha256(args.windows_manifest)},
        'status': 'CANDIDATE',
        'training_authorized': False,
        'scope': windows['scope'],
        'split_policy': windows['split_policy'],
        'host': 'native_linux',
        'tokenizer': {
            'repo': windows['tokenizer']['repo'],
            'revision': windows['tokenizer']['revision'],
            'path': str(tokenizer_dir),
            'files': tokenizer_files,
            'files_not_pinned_on_linux': dropped,
            'template_kwargs': windows['tokenizer']['template_kwargs'],
            'mask_policy': windows['tokenizer']['mask_policy'],
            'effective_sha256': windows['tokenizer']['effective_sha256'],
        },
        'runtime': runtime,
        'runtime_pinned_by_windows_manifest': required_runtime,
        'runtime_installed_on_this_host': installed,
        'exclusion_union': exclusion,
        'phases': phases,
        'admission': {
            'clean_seed': seed_ok,
            'clean_seed_receipt': {
                'path': str(receipt_path.resolve()),
                'sha256': sha256(receipt_path),
                'seed_path': receipt.get('seed_path'),
                'expected_revision': receipt.get('expected_revision'),
                'shard_count': receipt.get('shard_count'),
                'shard_bytes_total': receipt.get('shard_bytes_total'),
            },
            'gates': dict(windows['admission']['gates']),
        },
        'binding_failures': failures,
    }
    if failures:
        manifest['status'] = 'BLOCKED_BINDING_FAILURES'

    out = Path(args.out)
    out.write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({'status': manifest['status'], 'release_id': manifest['release_id'],
                      'clean_seed': seed_ok, 'binding_failures': failures,
                      'manifest': str(out)}, indent=2))
    return 0 if not failures else 2


if __name__ == '__main__':
    sys.exit(main())
