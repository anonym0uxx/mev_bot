"""RED/GREEN tests for admission gate evidence. No weights are ever loaded."""
import hashlib
import importlib.util
import json
import struct
import unittest
from pathlib import Path
import tempfile

HERE = Path(__file__).resolve().parent
TARGET = HERE / 'build_gate_evidence.py'


def load():
    spec = importlib.util.spec_from_file_location('build_gate_evidence', TARGET)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def write_shard(path, tensors):
    """tensors: {name: (dtype, shape)} -> safetensors header, payload omitted.

    The counter reads only headers and index metadata, so the payload is left
    out on purpose: writing 50 GB of zeros to prove a sum of shapes would be
    pure waste.
    """
    header = {}
    offset = 0
    for name, (dtype, shape) in tensors.items():
        size = 1
        for dim in shape:
            size *= dim
        nbytes = size * (2 if dtype == 'BF16' else 4)
        header[name] = {'dtype': dtype, 'shape': list(shape),
                        'data_offsets': [offset, offset + nbytes]}
        offset += nbytes
    raw = json.dumps(header).encode()
    path.write_bytes(struct.pack('<Q', len(raw)) + raw)


def above_floor(shards=2, per_shard=600, side=4096):
    """Tensor groups totalling >20B params without allocating any weights."""
    return [{'w%d_%d.weight' % (s, i): ('BF16', [side, side])
             for i in range(per_shard)} for s in range(shards)]


def make_seed(root, tensors_per_shard):
    shards = {}
    weight_map = {}
    for i, tensors in enumerate(tensors_per_shard, 1):
        name = 'model-%05d-of-%05d.safetensors' % (i, len(tensors_per_shard))
        write_shard(root / name, tensors)
        shards[name] = tensors
        for tensor in tensors:
            weight_map[tensor] = name
    (root / 'model.safetensors.index.json').write_text(
        json.dumps({'metadata': {}, 'weight_map': weight_map}))
    total = 0
    for tensors in tensors_per_shard:
        for _, shape in tensors.values():
            count = 1
            for dim in shape:
                count *= dim
            total += count
    return total


TRAINER_OK = """
def main():
    model = load()
    require(all(p.requires_grad for p in model.parameters()), 'Full-parameter training required')
"""

TRAINER_FROZEN = """
def main():
    model = load()
    model.gradient_checkpointing_enable()
"""


class GateEvidenceTest(unittest.TestCase):
    def setUp(self):
        self.m = load()

    def test_parameter_count_is_summed_from_headers_not_weights(self):
        m = self.m
        with tempfile.TemporaryDirectory() as temp:
            seed = Path(temp)
            expected = make_seed(seed, [
                {'a.weight': ('BF16', [100, 50])},
                {'b.weight': ('BF16', [10, 10]), 'c.bias': ('F32', [7])},
            ])
            total, dtype_of, shards, weight_map = m.parameter_count_from_index(
                seed / 'model.safetensors.index.json')
            self.assertEqual(total, expected)
            self.assertEqual(total, 100 * 50 + 10 * 10 + 7)
            self.assertEqual(dtype_of['BF16'], 5000 + 100)
            self.assertEqual(dtype_of['F32'], 7)
            self.assertEqual(len(shards), 2)
            self.assertEqual(len(weight_map), 3)

    def test_full_parameter_gate_requires_both_size_and_trainability(self):
        m = self.m
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            seed = root / 'seed'
            seed.mkdir()
            # ~24B params, above the 20B floor used here.
            big = above_floor()
            make_seed(seed, big)
            trainer = root / 'train_qwen27b.py'
            trainer.write_text(TRAINER_OK)
            result = m.full_parameter_gate(seed, trainer, min_billions=20.0)
            self.assertTrue(result['passed'], result.get('reason'))
            self.assertGreater(result['parameter_billions'], 20.0)
            self.assertTrue(result['trainer_all_parameters_trainable_assertion'])

            # Same weights, but a trainer that could silently freeze layers.
            trainer.write_text(TRAINER_FROZEN)
            result = m.full_parameter_gate(seed, trainer, min_billions=20.0)
            self.assertFalse(result['passed'])
            self.assertIn('requires_grad', result['reason'])

    def test_small_seed_fails_the_size_floor(self):
        m = self.m
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            seed = root / 'seed'
            seed.mkdir()
            make_seed(seed, [{'a.weight': ('BF16', [100, 100])}])
            trainer = root / 'trainer.py'
            trainer.write_text(TRAINER_OK)
            result = m.full_parameter_gate(seed, trainer, min_billions=20.0)
            self.assertFalse(result['passed'])
            self.assertIn('below', result['reason'])

    def test_distributed_gate_separates_fixture_from_native(self):
        m = self.m
        fixture = {'world_size': 3, 'all_ranks_exit_zero': True,
                   'weighted_loss_relative_error': 8.86e-08,
                   'native_deepspeed_verified': False}
        # A CPU fixture is real proof of the loss maths...
        result = m.distributed_loss_gate(fixture, require_native=False)
        self.assertTrue(result['passed'])
        self.assertEqual(result['scope'], 'cpu_fixture_only')
        # ...but it is not proof of native DeepSpeed, and must not be sold as such.
        native = m.distributed_loss_gate(fixture, require_native=True)
        self.assertFalse(native['passed'])
        self.assertIn('native DeepSpeed', native['reason'])

    def test_distributed_gate_rejects_unreconciled_mass(self):
        m = self.m
        bad = {'world_size': 3, 'all_ranks_exit_zero': True,
               'weighted_loss_relative_error': 0.5}
        result = m.distributed_loss_gate(bad)
        self.assertFalse(result['passed'])
        self.assertIn('reconciled', result['reason'])

    def test_unverifiable_gates_stay_blocked_with_named_missing_artifacts(self):
        m = self.m
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            seed = root / 'seed'
            seed.mkdir()
            big = above_floor()
            make_seed(seed, big)
            trainer = root / 'trainer.py'
            trainer.write_text(TRAINER_OK)
            release = root / 'CANDIDATE.json'
            release.write_text('{}')
            receipt = m.build(seed, trainer, None, release)
            self.assertFalse(receipt['all_gates_satisfied'])
            self.assertFalse(receipt['training_authorized'])
            for gate in ('semantic_review', 'economic_contract', 'clean_evaluation'):
                self.assertFalse(receipt['gates'][gate]['passed'])
                self.assertTrue(receipt['gates'][gate]['requires'],
                                gate + ' must name what it is waiting on')
            # An approval is an operator act; the tool must never invent one.
            self.assertIsNone(receipt['approved_by'])
            self.assertIn('full_parameter_27b', receipt['verified_gates'])

    def test_exit_code_is_nonzero_while_any_gate_is_unresolved(self):
        m = self.m
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            seed = root / 'seed'
            seed.mkdir()
            big = above_floor()
            make_seed(seed, big)
            trainer = root / 'trainer.py'
            trainer.write_text(TRAINER_OK)
            release = root / 'CANDIDATE.json'
            release.write_text('{}')
            out = root / 'EVIDENCE.json'
            code = m.main(['--seed-dir', str(seed), '--trainer', str(trainer),
                           '--release', str(release), '--out', str(out)])
            self.assertEqual(code, 2)
            written = json.loads(out.read_text())
            self.assertEqual(written['all_gates_satisfied'], False)

    def test_missing_index_is_blocked_not_crashed(self):
        m = self.m
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            seed = root / 'seed'
            seed.mkdir()
            trainer = root / 'trainer.py'
            trainer.write_text(TRAINER_OK)
            result = m.full_parameter_gate(seed, trainer)
            self.assertFalse(result['passed'])
            self.assertIn('seed index not found', result['reason'])


if __name__ == '__main__':
    unittest.main()
