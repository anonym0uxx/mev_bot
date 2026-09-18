"""RED/GREEN tests for the Linux release rebinding. Runs on either host."""
import hashlib
import importlib.util
import json
import unittest
from pathlib import Path
import tempfile

HERE = Path(__file__).resolve().parent
MODULE = HERE / 'build_linux_release_manifest.py'


def load():
    spec = importlib.util.spec_from_file_location('build_linux_release_manifest', MODULE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def installed_runtime():
    """Pin the fixture to whatever this interpreter actually has, so the happy
    path exercises the real comparison instead of a hardcoded guess."""
    import importlib.metadata as metadata
    return {p: metadata.version(p)
            for p in ('transformers', 'tokenizers', 'torch', 'accelerate')}


def fixture(root, clean=True):
    """Build a minimal Windows-shaped manifest plus its Linux-side files."""
    data = root / 'win' / 'data'
    data.mkdir(parents=True)
    (data / 'cpt_train.jsonl').write_text('{"a":1}\n')
    (data / 'sft_train.jsonl').write_text('{"b":2}\n')
    exclusion = data / 'EXCLUSION_UNION.jsonl'
    exclusion.write_text('{"mint":"m"}\n')

    tokenizer = root / 'linux-cache' / 'snap'
    tokenizer.mkdir(parents=True)
    files = {}
    for name in load().TEXT_TOKENIZER_FILES:
        (tokenizer / name).write_text(name + '-content')
        files[name] = sha(tokenizer / name)
    # A Windows-only file that must NOT be required on Linux.
    files['preprocessor_config.json'] = 'f' * 64

    manifest = {
        'schema_version': 'qwen27b_release_v2', 'release_id': 'r1', 'status': 'CANDIDATE',
        'training_authorized': False, 'scope': 'source_support_only',
        'split_policy': 'frozen_mint_sha256_10_10_80_legacy_union_v1',
        'runtime': installed_runtime(),
        'tokenizer': {'repo': 'unsloth/Qwen3.8-27B', 'revision': 'rev',
                      'path': 'C:/win/snap', 'files': files,
                      'template_kwargs': {'enable_thinking': False},
                      'mask_policy': 'assistant_body_only_full_render_offsets_v1',
                      'effective_sha256': 'e' * 64},
        'exclusion_union': {'path': str(exclusion), 'rows': 1,
                            'bytes': exclusion.stat().st_size, 'sha256': sha(exclusion)},
        'phases': {
            'cpt': {'train': [{'path': str(data / 'cpt_train.jsonl'), 'rows': 1,
                               'bytes': (data / 'cpt_train.jsonl').stat().st_size,
                               'sha256': sha(data / 'cpt_train.jsonl')}],
                    'validation': [], 'task_targets': {'supplied_episode_ledger': 0.5}},
            'sft': {'train': [{'path': str(data / 'sft_train.jsonl'), 'rows': 1,
                               'bytes': (data / 'sft_train.jsonl').stat().st_size,
                               'sha256': sha(data / 'sft_train.jsonl')}],
                    'validation': [], 'task_targets': {'observed_action_attribution': 1.0}},
        },
        'admission': {'clean_seed': True,
                      'gates': {'semantic_review': False}},
    }
    win_manifest = root / 'CANDIDATE_RELEASE.json'
    win_manifest.write_text(json.dumps(manifest))

    receipt = root / 'CLEAN_SEED_RECEIPT.json'
    receipt.write_text(json.dumps({
        'clean_seed_verified': clean, 'raw_base_selected': clean,
        'failures': [] if clean else ['shard digest mismatch'],
        'seed_path': '/home/alon/.cache/raw/snap/rev', 'expected_revision': 'rev',
        'shard_count': 18, 'shard_bytes_total': 55563006776}))
    return win_manifest, receipt, tokenizer, data


def run(root, win_manifest, receipt, tokenizer, data, out, extra=()):
    m = load()
    return m.main(list(extra)+[
        '--windows-manifest', str(win_manifest),
        '--path-map', '%s=%s' % (root / 'win', root / 'win'),
        '--tokenizer-dir', str(tokenizer),
        '--seed-receipt', str(receipt),
        '--out', str(out),
    ])


class LinuxManifestTest(unittest.TestCase):
    def test_happy_path_binds_linux_paths_and_real_clean_seed_evidence(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            win, receipt, tokenizer, data = fixture(root)
            out = root / 'LINUX.json'
            self.assertEqual(run(root, win, receipt, tokenizer, data, out), 0)
            manifest = json.loads(out.read_text())
            self.assertEqual(manifest['status'], 'CANDIDATE')
            self.assertNotIn('windows_path', manifest['tokenizer'])
            self.assertNotIn('C:/win', json.dumps(manifest['phases']))
            self.assertNotEqual(manifest['tokenizer']['path'], 'C:/win/snap')
            self.assertEqual(Path(manifest['tokenizer']['path']).resolve(), tokenizer.resolve())
            # Evidence-bound, not a bare assertion.
            self.assertTrue(manifest['admission']['clean_seed'])
            self.assertEqual(manifest['admission']['clean_seed_receipt']['sha256'],
                             sha(receipt))
            # Windows-only file must be reported, never required.
            self.assertIn('preprocessor_config.json',
                          manifest['tokenizer']['files_not_pinned_on_linux'])

    def test_clean_seed_false_when_receipt_does_not_verify(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            win, receipt, tokenizer, data = fixture(root, clean=False)
            out = root / 'LINUX.json'
            self.assertEqual(run(root, win, receipt, tokenizer, data, out), 2)
            manifest = json.loads(out.read_text())
            self.assertFalse(manifest['admission']['clean_seed'])
            self.assertEqual(manifest['status'], 'BLOCKED_BINDING_FAILURES')
            self.assertFalse(manifest['training_authorized'])

    def test_data_digest_drift_blocks_the_release(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            win, receipt, tokenizer, data = fixture(root)
            (data / 'cpt_train.jsonl').write_text('{"tampered":true}\n')
            out = root / 'LINUX.json'
            self.assertEqual(run(root, win, receipt, tokenizer, data, out), 2)
            manifest = json.loads(out.read_text())
            self.assertTrue(any('digest drift' in f for f in manifest['binding_failures']))
            self.assertEqual(manifest['status'], 'BLOCKED_BINDING_FAILURES')

    def test_tokenizer_digest_mismatch_blocks_the_release(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            win, receipt, tokenizer, data = fixture(root)
            (tokenizer / 'tokenizer.json').write_text('different-bytes')
            out = root / 'LINUX.json'
            self.assertEqual(run(root, win, receipt, tokenizer, data, out), 2)
            manifest = json.loads(out.read_text())
            self.assertTrue(any('tokenizer digest mismatch' in f
                                for f in manifest['binding_failures']))

    def test_runtime_mismatch_blocks_unless_explicitly_accepted(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            win, receipt, tokenizer, data = fixture(root)
            manifest = json.loads(win.read_text())
            manifest['runtime']['torch'] = '0.0.0-not-installed'
            win.write_text(json.dumps(manifest))
            out = root / 'LINUX.json'
            self.assertEqual(run(root, win, receipt, tokenizer, data, out), 2)
            blocked = json.loads(out.read_text())
            self.assertTrue(any('host runtime differs' in f
                                for f in blocked['binding_failures']))
            # Explicit acceptance records the host version instead of the lie.
            out2 = root / 'LINUX2.json'
            self.assertEqual(
                run(root, win, receipt, tokenizer, data, out2,
                    extra=['--accept-host-runtime']), 0)
            accepted = json.loads(out2.read_text())
            self.assertEqual(accepted['runtime']['torch'],
                             accepted['runtime_installed_on_this_host']['torch'])
            self.assertEqual(accepted['runtime_pinned_by_windows_manifest']['torch'],
                             '0.0.0-not-installed')

    def test_missing_mapped_file_blocks_instead_of_silently_dropping(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            win, receipt, tokenizer, data = fixture(root)
            (data / 'sft_train.jsonl').unlink()
            out = root / 'LINUX.json'
            self.assertEqual(run(root, win, receipt, tokenizer, data, out), 2)
            manifest = json.loads(out.read_text())
            self.assertTrue(any('missing at mapped path' in f
                                for f in manifest['binding_failures']))


if __name__ == '__main__':
    unittest.main()
