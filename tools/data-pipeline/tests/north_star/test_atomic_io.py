"""Small synthetic fixtures exercise publication integrity, not corpus truth."""
import importlib.util
import hashlib
import pytest


def test_exact_resume_verifies_receipt(tmp_path):
    module = load_io()
    p = tmp_path / 'part'
    first = module.publish_bytes(p, b'fixture', {'config': 'v1'})
    assert module.publish_bytes(p, b'fixture', {'config': 'v1'}) == first


def test_wrong_resume_payload_refused(tmp_path):
    module = load_io()
    p = tmp_path / 'part'
    module.publish_bytes(p, b'fixture', {'config': 'v1'})
    with pytest.raises(ValueError, match='payload'):
        module.publish_bytes(p, b'changed', {'config': 'v1'})
    assert p.read_bytes() == b'fixture'


def test_corrupt_partition_not_accepted(tmp_path):
    module = load_io()
    p = tmp_path / 'part'
    module.publish_bytes(p, b'fixture', {'config': 'v1'})
    p.write_bytes(b'corrupt')
    with pytest.raises(ValueError, match='integrity'):
        module.verify_partition(p, {'config': 'v1'})


def test_changed_config_and_uncommitted_file_refused(tmp_path):
    module = load_io()
    p = tmp_path / 'part'
    module.publish_bytes(p, b'fixture', {'config': 'v1'})
    with pytest.raises(ValueError, match='provenance'):
        module.verify_partition(p, {'config': 'v2'})
    uncommitted = tmp_path / 'orphan'
    uncommitted.write_bytes(b'fixture')
    with pytest.raises(FileNotFoundError):
        module.publish_bytes(uncommitted, b'fixture', {'config': 'v1'})
    assert uncommitted.read_bytes() == b'fixture'
from pathlib import Path

MODULE = Path(__file__).parents[2] / 'src/north_star/io.py'


def load_io():
    assert MODULE.exists(), 'atomic writer is missing'
    import sys
    if str(MODULE.parents[1]) not in sys.path:
        sys.path.insert(0, str(MODULE.parents[1]))
    spec = importlib.util.spec_from_file_location('ns_io', MODULE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_publish_bytes_round_trip_with_bound_provenance(tmp_path):
    target = tmp_path / 'partition.bin'
    data = b'synthetic canonical partition'
    receipt = load_io().publish_bytes(target, data, {'input_sha256': 'a'*64, 'config_sha256': 'b'*64, 'schema': 'fixture/1'})
    assert target.read_bytes() == data
    assert receipt['sha256'] == hashlib.sha256(data).hexdigest()
    assert load_io().verify_partition(target, receipt['provenance']) == receipt


@pytest.mark.parametrize('provenance', [
    {'unsupported': object()}, {'unsupported': {1, 2}},
    {'lossy': (1, 2)}, {1: 'non-string key'},
    {'nonfinite': float('nan')}, {'nonfinite': float('inf')},
])
def test_unsupported_provenance_leaves_no_files(tmp_path, provenance):
    target = tmp_path / 'part'
    with pytest.raises((TypeError, ValueError)):
        load_io().publish_bytes(target, b'fixture', provenance)
    assert list(tmp_path.iterdir()) == []
