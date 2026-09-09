"""Filesystem policy probes; all data are temporary synthetic fixtures.

Mock lstat classifications for portable Windows symlink/reparse coverage without
administrator privileges. The underlying reads/writes still use real files.
"""
import hashlib
from pathlib import Path
import stat
from types import SimpleNamespace

import pytest

from test_atomic_io import load_io
from test_recovery import load_recovery


def mark_path(monkeypatch, path, kind):
    original = Path.lstat

    def lstat(self, *args, **kwargs):
        result = original(self, *args, **kwargs)
        if self.absolute() != path.absolute():
            return result
        attrs = {key: getattr(result, key) for key in dir(result) if key.startswith('st_')}
        if kind == 'reparse':
            attrs['st_file_attributes'] = getattr(result, 'st_file_attributes', 0) | 0x400
        else:
            attrs['st_mode'] = (stat.S_IFLNK if kind == 'symlink' else stat.S_IFIFO) | 0o600
        return SimpleNamespace(**attrs)

    monkeypatch.setattr(Path, 'lstat', lstat)


@pytest.mark.parametrize('kind', ['symlink', 'reparse', 'nonregular'])
@pytest.mark.parametrize(('operation', 'location'), [
    (operation, location)
    for operation in ('recover', 'verify', 'resume')
    for location in ('target', 'parent', 'ancestor', 'receipt')
    if not (operation == 'recover' and location == 'receipt')
])
def test_rejects_unsafe_existing_paths(tmp_path, monkeypatch, kind, operation, location):
    parent = tmp_path / 'ancestor' / 'parent'
    parent.mkdir(parents=True)
    target = parent / 'part'
    module = load_io()
    module.publish_bytes(target, b'fixture', {'config': 'v1'})
    paths = {'target': target, 'parent': parent, 'ancestor': parent.parent,
             'receipt': target.with_name('part.receipt.json')}
    mark_path(monkeypatch, paths[location], kind)
    with pytest.raises((ValueError, OSError)):
        if operation == 'recover':
            load_recovery().recover_blob(tmp_path, 'unused', target, 7, hashlib.sha256(b'fixture').hexdigest())
        elif operation == 'verify':
            module.verify_partition(target, {'config': 'v1'})
        else:
            module.publish_bytes(target, b'fixture', {'config': 'v1'})


@pytest.mark.parametrize('kind', ['symlink', 'reparse', 'nonregular'])
@pytest.mark.parametrize('location', ['parent', 'receipt'])
def test_publication_rejects_unsafe_paths_before_writing(tmp_path, monkeypatch, kind, location):
    parent = tmp_path / 'out'
    parent.mkdir()
    target = parent / 'part'
    receipt = parent / 'part.receipt.json'
    if location == 'receipt':
        receipt.write_bytes(b'preserve')
    mark_path(monkeypatch, parent if location == 'parent' else receipt, kind)
    with pytest.raises((ValueError, OSError)):
        load_io().publish_bytes(target, b'fixture', {})
    assert not target.exists()
    assert not list(parent.glob('*.pending'))
    if location == 'receipt':
        assert receipt.read_bytes() == b'preserve'


@pytest.mark.parametrize('operation', ['recover', 'verify'])
@pytest.mark.parametrize('change', ['replacement', 'mutation'])
def test_rejects_target_change_during_hash(tmp_path, monkeypatch, operation, change):
    import os
    target = tmp_path / 'part'
    module = load_io()
    module.publish_bytes(target, b'fixture', {})
    replacement = tmp_path / 'replacement'
    replacement.write_bytes(b'corrupt')
    replacement_info = replacement.stat()
    original_digest = hashlib.file_digest
    original_stat, original_lstat = Path.stat, Path.lstat
    changed = False

    def digest(source, algorithm):
        nonlocal changed
        result = original_digest(source, algorithm)
        if Path(source.name) == target:
            changed = True
            if change == 'mutation':
                target.write_bytes(b'corrupt')
                os.utime(target, ns=(1000000000, 1000000000))
        return result

    def observed(original):
        def probe(self, *args, **kwargs):
            if changed and change == 'replacement' and self == target:
                return replacement_info
            return original(self, *args, **kwargs)
        return probe

    monkeypatch.setattr(hashlib, 'file_digest', digest)
    monkeypatch.setattr(Path, 'stat', observed(original_stat))
    monkeypatch.setattr(Path, 'lstat', observed(original_lstat))
    with pytest.raises((ValueError, OSError)):
        if operation == 'recover':
            load_recovery().recover_blob(tmp_path, 'unused', target, 7, hashlib.sha256(b'fixture').hexdigest())
        else:
            module.verify_partition(target, {})


def test_receipt_rechecked_after_partition_hash(tmp_path, monkeypatch):
    target = tmp_path / 'part'
    module = load_io()
    module.publish_bytes(target, b'fixture', {})
    receipt = target.with_name('part.receipt.json')
    original = hashlib.file_digest

    def digest(source, algorithm):
        result = original(source, algorithm)
        receipt.write_text('{}')
        return result

    monkeypatch.setattr(hashlib, 'file_digest', digest)
    with pytest.raises((ValueError, OSError)):
        module.verify_partition(target, {})


@pytest.mark.parametrize('operation', ['recover', 'verify'])
def test_hash_and_size_use_same_open_descriptor(tmp_path, monkeypatch, operation):
    import os
    target = tmp_path / 'part'
    module = load_io()
    module.publish_bytes(target, b'fixture', {})
    original_fstat, original_digest = os.fstat, hashlib.file_digest
    events = []

    def fstat(fd):
        events.append(('stat', fd))
        return original_fstat(fd)

    def digest(source, algorithm):
        events.append(('hash', source.fileno()))
        return original_digest(source, algorithm)

    monkeypatch.setattr(os, 'fstat', fstat)
    monkeypatch.setattr(hashlib, 'file_digest', digest)
    if operation == 'recover':
        load_recovery().recover_blob(tmp_path, 'unused', target, 7, hashlib.sha256(b'fixture').hexdigest())
    else:
        module.verify_partition(target, {})
    index = next(i for i, event in enumerate(events) if event[0] == 'hash')
    descriptor = events[index][1]
    assert ('stat', descriptor) in events[:index]
    assert ('stat', descriptor) in events[index + 1:]


@pytest.mark.parametrize('operation', ['recover', 'publish'])
def test_racing_existing_target_is_never_clobbered(tmp_path, monkeypatch, operation):
    import os
    import subprocess
    target = tmp_path / 'part'
    original = os.link

    def racing_link(source, destination, *args, **kwargs):
        if Path(destination) == target:
            target.write_bytes(b'preserve racing writer')
        return original(source, destination, *args, **kwargs)

    monkeypatch.setattr(os, 'link', racing_link)
    with pytest.raises(FileExistsError):
        if operation == 'recover':
            repo = tmp_path / 'repo'
            subprocess.run(['git', 'init', '-q', str(repo)], check=True)
            oid = subprocess.run(['git', '-C', str(repo), 'hash-object', '-w', '--stdin'],
                                 input=b'fixture', capture_output=True, check=True).stdout.decode().strip()
            load_recovery().recover_blob(repo, oid, target, 7, hashlib.sha256(b'fixture').hexdigest())
        else:
            load_io().publish_bytes(target, b'fixture', {})
    assert target.read_bytes() == b'preserve racing writer'
    assert not target.with_name('part.receipt.json').exists()
    assert not list(tmp_path.glob('*.pending'))


def test_existing_receipt_is_never_clobbered(tmp_path):
    target = tmp_path / 'part'
    receipt = target.with_name('part.receipt.json')
    receipt.write_bytes(b'preserve receipt')
    with pytest.raises(FileExistsError):
        load_io().publish_bytes(target, b'fixture', {})
    assert not target.exists()
    assert receipt.read_bytes() == b'preserve receipt'


@pytest.mark.parametrize('dangling', [False, True])
def test_real_symlink_target_rejected(tmp_path, dangling):
    external = tmp_path / 'external'
    if not dangling:
        external.write_bytes(b'fixture')
    target = tmp_path / 'part'
    try:
        target.symlink_to(external)
    except OSError as error:
        pytest.skip(f'Native symlink creation unavailable: {error}')
    with pytest.raises(ValueError, match='link/reparse'):
        load_recovery().recover_blob(tmp_path, 'unused', target, 7, hashlib.sha256(b'fixture').hexdigest())
    with pytest.raises(ValueError, match='link/reparse'):
        load_io().publish_bytes(target, b'fixture', {})
    assert target.is_symlink()
