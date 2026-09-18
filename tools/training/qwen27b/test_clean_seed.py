"""RED/GREEN tests for the clean-seed proof. No model weights are loaded."""
import hashlib
import importlib.util
import json
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
MODULE = HERE / 'verify_clean_seed.py'


def load():
    spec = importlib.util.spec_from_file_location('verify_clean_seed', MODULE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def build_seed(root, shard_bytes=1024, tokenizer_files=None):
    """Synthetic raw seed. Default tokenizer files deliberately do NOT match pins."""
    seed = root / load().REVISION
    seed.mkdir(parents=True)
    for number in range(1, load().SHARD_COUNT + 1):
        (seed / ('model-%05d-of-%05d.safetensors' % (number, load().SHARD_COUNT))
         ).write_bytes(bytes(shard_bytes))
    (seed / 'model.safetensors.index.json').write_text(json.dumps(
        {'metadata': {'total_size': load().INDEX_TOTAL_SIZE}}))
    for name, content in (tokenizer_files or {'tokenizer.json': 'not-the-pinned-bytes'}).items():
        (seed / name).write_text(content)
    return seed


class CleanSeedTest(unittest.TestCase):
    def test_valid_raw_seed_passes_when_every_identity_matches(self):
        import tempfile
        m = load()
        with tempfile.TemporaryDirectory() as temp:
            seed = build_seed(Path(temp), shard_bytes=m.INDEX_TOTAL_SIZE // m.SHARD_COUNT + 1)
            for name in m.TOKENIZER_FILES:
                (seed / name).write_text(name)
            m.TOKENIZER_FILES = {n: hashlib.sha256(n.encode()).hexdigest()
                                 for n in m.TOKENIZER_FILES}
            receipt, ok = m.audit(seed)
            self.assertTrue(ok, receipt['failures'])
            self.assertTrue(receipt['clean_seed_verified'])
            self.assertEqual(receipt['shard_count'], m.SHARD_COUNT)

    def test_wrong_revision_directory_fails(self):
        import tempfile
        m = load()
        with tempfile.TemporaryDirectory() as temp:
            seed = build_seed(Path(temp))
            moved = seed.parent / 'some-other-revision'
            seed.rename(moved)
            receipt, ok = m.audit(moved)
            self.assertFalse(ok)
            self.assertTrue(any('pinned revision' in f for f in receipt['failures']))

    def test_missing_shard_fails(self):
        import tempfile
        m = load()
        with tempfile.TemporaryDirectory() as temp:
            seed = build_seed(Path(temp))
            next(iter(sorted(seed.glob('model-*.safetensors')))).unlink()
            receipt, ok = m.audit(seed)
            self.assertFalse(ok)
            self.assertTrue(any('shards' in f for f in receipt['failures']))

    def test_tokenizer_digest_mismatch_fails(self):
        import tempfile
        m = load()
        with tempfile.TemporaryDirectory() as temp:
            seed = build_seed(Path(temp))
            receipt, ok = m.audit(seed)
            self.assertFalse(ok)
            self.assertTrue(any('digest mismatch' in f for f in receipt['failures']))

    def test_truncated_seed_fails_even_with_all_shard_names(self):
        import tempfile
        m = load()
        with tempfile.TemporaryDirectory() as temp:
            seed = build_seed(Path(temp), shard_bytes=8)
            receipt, ok = m.audit(seed)
            self.assertFalse(ok)
            self.assertTrue(any('truncated' in f for f in receipt['failures']))

    def test_prior_bad_data_checkpoint_can_never_be_the_seed(self):
        import tempfile
        m = load()
        with tempfile.TemporaryDirectory() as temp:
            bad = Path(temp) / 'cpt_final_bf16'
            bad.mkdir()
            receipt, ok = m.audit(bad, prior_paths=[str(bad)])
            self.assertFalse(ok)
            self.assertTrue(any('bad-data checkpoint marker' in f for f in receipt['failures']))
            self.assertFalse(receipt['training_authorized'])

    def test_prior_checkpoint_as_seed_parent_is_refused(self):
        import tempfile
        m = load()
        with tempfile.TemporaryDirectory() as temp:
            prior = Path(temp) / 'prior_cpt_run'
            seed = build_seed(prior)
            receipt, ok = m.audit(seed, prior_paths=[str(prior)])
            self.assertFalse(ok)
            self.assertTrue(any('nests inside superseded' in f for f in receipt['failures']))


if __name__ == '__main__':
    unittest.main()
