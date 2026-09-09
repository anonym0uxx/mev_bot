"""Restore an identified Git blob without clobbering an existing source file.

Destinations must be caller-controlled trusted trees; see fs_integrity for the
link policy and bounded race guarantees. This is not a hostile-filesystem sandbox.
"""
from pathlib import Path
import hashlib
import os
import subprocess
import tempfile

from north_star.fs_integrity import checked_path, stable_reader


def recover_blob(repo, object_id, target, expected_bytes, expected_sha256):
    target = Path(target)
    if checked_path(target, allow_missing=True, create_parents=True) is not None:
        with stable_reader(target) as (existing, info):
            digest = hashlib.file_digest(existing, 'sha256').hexdigest()
        if info.st_size != expected_bytes or digest != expected_sha256:
            raise FileExistsError('Existing recovery target differs from manifest')
        return {'state': 'VERIFIED_EXISTING', 'path': str(target), 'bytes': expected_bytes, 'sha256': digest, 'git_blob': object_id}
    h = hashlib.sha256()
    count = 0
    fd, temporary = tempfile.mkstemp(prefix=target.name + '.', suffix='.pending', dir=target.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            child = subprocess.Popen(['git', '-C', str(repo), 'cat-file', 'blob', object_id], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            try:
                while chunk := child.stdout.read(1024 * 1024):
                    output.write(chunk)
                    h.update(chunk)
                    count += len(chunk)
                child.stdout.close()
                if child.wait() != 0:
                    raise ValueError('Git blob read failed')
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()
            output.flush()
            os.fsync(output.fileno())
        if count != expected_bytes or h.hexdigest() != expected_sha256:
            raise ValueError('Manifest size/hash mismatch')
        # Atomic no-clobber publication on the same volume. A racing writer
        # raises FileExistsError instead of overwriting an existing source.
        checked_path(target, allow_missing=True)
        os.link(temporary, target)
        return {'state': 'RECOVERED', 'path': str(target), 'bytes': count, 'sha256': h.hexdigest(), 'git_blob': object_id}
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
