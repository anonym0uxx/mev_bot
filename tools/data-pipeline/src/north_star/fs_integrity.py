"""Integrity checks for paths in a caller-controlled, trusted directory tree.

Every existing ancestor and file is lstat-checked: symlinks, Windows reparse
points, and nonregular files are refused. These checks detect observed changes;
they do NOT provide a sandbox against arbitrary hostile concurrent filesystem
mutation. Callers must prevent untrusted ancestor replacement and file mutation
throughout an operation (including hard-link aliases). Publication uses an atomic
no-clobber hard link, not an overwriting rename.
"""
from pathlib import Path
from contextlib import contextmanager
import os
import stat


def _reject_link(path, info):
    if stat.S_ISLNK(info.st_mode) or getattr(info, 'st_file_attributes', 0) & 0x400:
        raise ValueError(f'Unsafe link/reparse path: {path}')


def checked_path(path, *, allow_missing=False, create_parents=False):
    """Check ancestors without resolving links; return leaf lstat or None."""
    path = Path(path).absolute()
    for parent in reversed(path.parents):
        try:
            info = parent.lstat()
        except FileNotFoundError:
            if not create_parents:
                raise
            try:
                parent.mkdir()
            except FileExistsError:
                pass
            info = parent.lstat()
        _reject_link(parent, info)
        if not stat.S_ISDIR(info.st_mode):
            raise ValueError(f'Non-directory ancestor: {parent}')
    try:
        info = path.lstat()
    except FileNotFoundError:
        if allow_missing:
            return None
        raise
    _reject_link(path, info)
    if not stat.S_ISREG(info.st_mode):
        raise ValueError(f'Nonregular file: {path}')
    return info


def _version(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_size,
            info.st_mtime_ns, info.st_ctime_ns)


@contextmanager
def stable_reader(path):
    """Bind a read to one regular descriptor and recheck its pathname afterward.

    Size/identity/timestamps are checked before and after the read. A change
    observed at either boundary fails closed; undetectable ABA changes or changes
    after the final check are outside this trusted-directory API's guarantee.
    """
    path = Path(path)
    before = checked_path(path)
    with path.open('rb') as source:
        opened = os.fstat(source.fileno())
        _reject_link(path, opened)
        if not stat.S_ISREG(opened.st_mode) or _version(before) != _version(opened):
            raise ValueError(f'File integrity changed before read: {path}')
        yield source, opened
        after = os.fstat(source.fileno())
        current = checked_path(path)
        if _version(opened) != _version(after) or _version(after) != _version(current):
            raise ValueError(f'File integrity changed during read: {path}')
