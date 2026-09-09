"""Synthetic Git blobs only; no corpus examples or production directories."""
import hashlib
import importlib.util
from pathlib import Path
import subprocess
import pytest


def test_rejects_manifest_mismatch_without_publish(tmp_path):
    repo = tmp_path / 'repo'
    subprocess.run(['git', 'init', '-q', str(repo)], check=True)
    oid = subprocess.run(['git', '-C', str(repo), 'hash-object', '-w', '--stdin'], input=b'fixture', capture_output=True, check=True).stdout.decode().strip()
    dst = tmp_path / 'out' / 'part'
    with pytest.raises(ValueError, match='Manifest'):
        load_recovery().recover_blob(repo, oid, dst, 7, '0' * 64)
    assert not dst.exists()
    assert not list(dst.parent.glob('*.pending'))


def test_existing_different_target_is_never_overwritten(tmp_path):
    repo = tmp_path / 'repo'
    subprocess.run(['git', 'init', '-q', str(repo)], check=True)
    payload = b'fixture'
    oid = subprocess.run(['git', '-C', str(repo), 'hash-object', '-w', '--stdin'], input=payload, capture_output=True, check=True).stdout.decode().strip()
    dst = tmp_path / 'part'
    dst.write_bytes(b'preserve me')
    with pytest.raises(FileExistsError):
        load_recovery().recover_blob(repo, oid, dst, len(payload), hashlib.sha256(payload).hexdigest())
    assert dst.read_bytes() == b'preserve me'


def test_repeat_recovery_verifies_existing_file(tmp_path):
    repo = tmp_path / 'repo'
    subprocess.run(['git', 'init', '-q', str(repo)], check=True)
    payload = b'fixture'
    oid = subprocess.run(['git', '-C', str(repo), 'hash-object', '-w', '--stdin'], input=payload, capture_output=True, check=True).stdout.decode().strip()
    dst = tmp_path / 'part'
    dst.write_bytes(payload)
    result = load_recovery().recover_blob(repo, oid, dst, len(payload), hashlib.sha256(payload).hexdigest())
    assert result['state'] == 'VERIFIED_EXISTING'

MODULE = Path(__file__).parents[2] / 'src/north_star/recovery.py'


def load_recovery():
    assert MODULE.exists(), 'recovery implementation is missing'
    import sys
    if str(MODULE.parents[1]) not in sys.path:
        sys.path.insert(0, str(MODULE.parents[1]))
    spec = importlib.util.spec_from_file_location('ns_recovery', MODULE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_recovers_exact_blob_without_touching_source(tmp_path):
    repo = tmp_path / 'repo'
    subprocess.run(['git', 'init', '-q', str(repo)], check=True)
    payload = b'synthetic recovery fixture\x00\xff'
    oid = subprocess.run(['git', '-C', str(repo), 'hash-object', '-w', '--stdin'], input=payload, capture_output=True, check=True).stdout.decode().strip()
    target = tmp_path / 'store' / 'part.zst'
    result = load_recovery().recover_blob(repo, oid, target, len(payload), hashlib.sha256(payload).hexdigest())
    assert target.read_bytes() == payload
    assert result['state'] == 'RECOVERED'
    assert result['sha256'] == hashlib.sha256(payload).hexdigest()
    assert subprocess.run(['git', '-C', str(repo), 'cat-file', '-e', oid]).returncode == 0
