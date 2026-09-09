"""Bounded synchronous jobs, NOT a deployed/gateway-independent supervisor.

Caller controls trusted directories and executable/config version pins. Logs may
contain child-emitted secrets; JSON receipts never contain argv, env, or exception
messages. Only the directly spawned child is owned; descendants are unsupported.
"""
from dataclasses import dataclass, field
from collections.abc import Mapping
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from north_star.fs_integrity import checked_path, stable_reader
from north_star.io import _publish_new


def _json(value):
    return json.dumps(value, sort_keys=True, allow_nan=False).encode('utf-8')


@dataclass(frozen=True)
class JobConfig:
    """Execution identity; config_id must pin code, inputs, and stage config.

    Environment is snapshotted by default; pass a minimal explicit mapping for
    stable cross-session identities. Identity hashes are not secret encryption.
    """
    stage: str
    config_id: str
    argv: tuple[str, ...] = field(repr=False)
    cwd: Path = field(repr=False)
    timeout_s: float
    env: tuple[tuple[str, str], ...] = field(default_factory=lambda: tuple(sorted(os.environ.items())), repr=False)

    def __post_init__(self):
        import math
        if not isinstance(self.stage, str) or not self.stage or not isinstance(self.config_id, str) or not self.config_id:
            raise ValueError('Nonempty stage and config version required')
        if not isinstance(self.argv, (list, tuple)) or not self.argv or any(
                not isinstance(arg, str) or '\x00' in arg for arg in self.argv):
            raise ValueError('Explicit argv sequence required')
        if not Path(self.argv[0]).is_absolute():
            raise ValueError('Absolute executable required')
        if Path(self.argv[0]).suffix.lower() in ('.bat', '.cmd'):
            raise ValueError('Batch files require an implicit shell and are refused')
        if not Path(self.cwd).is_absolute() or not Path(self.cwd).is_dir():
            raise ValueError('Existing absolute cwd required')
        if isinstance(self.timeout_s, bool) or not isinstance(self.timeout_s, (int, float)) or not math.isfinite(self.timeout_s) or self.timeout_s <= 0:
            raise ValueError('Positive finite timeout required')
        object.__setattr__(self, 'argv', tuple(self.argv))
        object.__setattr__(self, 'cwd', Path(self.cwd))
        pairs = tuple(self.env.items()) if isinstance(self.env, Mapping) else tuple(self.env)
        if any(not isinstance(pair, (tuple, list)) or len(pair) != 2 or
               any(not isinstance(v, str) or '\x00' in v for v in pair) or
               not pair[0] or '=' in pair[0] for pair in pairs):
            raise ValueError('Invalid explicit environment')
        names = [pair[0].upper() if os.name == 'nt' else pair[0] for pair in pairs]
        if len(set(names)) != len(names):
            raise ValueError('Duplicate environment key')
        object.__setattr__(self, 'env', tuple(sorted(tuple(pair) for pair in pairs)))

    @property
    def job_id(self):
        return hashlib.sha256(_json({
            'stage': self.stage, 'config_id': self.config_id,
            'argv': self.argv, 'cwd': str(self.cwd), 'timeout_s': self.timeout_s,
            'env': self.env,
        })).hexdigest()


def _atomic_json(path, value):
    checked_path(path, allow_missing=True, create_parents=True)
    fd, temporary = tempfile.mkstemp(prefix=path.name + '.', suffix='.pending', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            output.write(_json(value))
            output.flush()
            os.fsync(output.fileno())
        checked_path(path, allow_missing=True)
        # Ordinary Windows readers may briefly deny FILE_SHARE_DELETE.
        # Retry only this publication, never execution, with a finite budget.
        for attempt in range(6):
            try:
                os.replace(temporary, path)
                break
            except PermissionError:
                if attempt == 5:
                    raise
                time.sleep(0.01)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


class JobBusy(RuntimeError):
    """Another owner holds this run directory's OS lock."""


@contextmanager
def singleton_lock(root):
    """Nonblocking OS lock; never unlink its inode or trust PID text.

    Windows byte-range locks release on process death. A PID is diagnostic only,
    so PID reuse cannot steal a lock. POSIX flock is a development fallback.
    """
    path = Path(root) / 'job.lock'
    checked_path(path, allow_missing=True, create_parents=True)
    with path.open('a+b', buffering=0) as lock:
        if os.fstat(lock.fileno()).st_size == 0:
            lock.write(b'\0')
        lock.seek(0)
        if os.name == 'nt':
            import msvcrt
            acquire = lambda: msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
            release = lambda: msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            import fcntl
            acquire = lambda: fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            release = lambda: fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
        try:
            acquire()
        except OSError:
            raise JobBusy('Job lock unavailable') from None
        try:
            yield
        finally:
            lock.seek(0)
            release()


def run_job(config, root):
    """Execute one direct child in a dedicated, trusted attempt directory.

    Only exact successful receipts and hash-verified logs may resume. This does
    not validate application outputs: the stage must validate/publish them
    before exiting zero. Never repurpose/delete an incomplete attempt directory
    to force a retry; an orphan may still be alive. No process is adopted by PID.
    No disk quota, descendant ownership, deployment, or gateway survival claim.
    """
    with singleton_lock(root):
        return _run_locked(config, Path(root))


class JobResumeError(ValueError):
    """Prior state is not a verified successful completion of this identity."""


def _read_json(path):
    with stable_reader(path) as (source, _):
        return json.load(source)


def _logs(root):
    result = {}
    for name in ('stdout.log', 'stderr.log'):
        with stable_reader(root / name) as (source, info):
            result[name] = {'sha256': hashlib.file_digest(source, 'sha256').hexdigest(),
                            'bytes': info.st_size}
    return result


def _resume(config, root):
    try:
        if _read_json(root / 'identity.json') != {'schema': 1, 'job_id': config.job_id}:
            raise ValueError
        result = _read_json(root / 'exit.json')
        if (result['schema'] != 1 or result['job_id'] != config.job_id
                or result['status'] != 'completed' or type(result['returncode']) is not int
                or result['returncode'] != 0 or result['logs'] != _logs(root)
                or _read_json(root / 'heartbeat.json') != result):
            raise ValueError
        return result
    except (OSError, ValueError, KeyError, TypeError):
        raise JobResumeError('Prior job is incomplete, failed, corrupt, or mismatched; no retry') from None


def _run_locked(config, root):
    # A dedicated directory is one attempt: no adoption of orphans, no overwrite.
    if any(path.name != 'job.lock' for path in root.iterdir()):
        return _resume(config, root)
    _publish_new(root / 'identity.json', _json({'schema': 1, 'job_id': config.job_id}))
    state = {'schema': 1, 'job_id': config.job_id, 'owner_pid': os.getpid(),
             'child_pid': None, 'status': 'starting', 'updated_at': time.time(),
             'returncode': None}
    started = time.monotonic()
    _atomic_json(root / 'heartbeat.json', state)
    with (root / 'stdout.log').open('xb') as out, (root / 'stderr.log').open('xb') as err:
        try:
            child = subprocess.Popen(config.argv, cwd=config.cwd, env=dict(config.env), shell=False,
                                     stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                     close_fds=True)
        except OSError as error:
            # Never persist str(error): it may contain argv/cwd or credentials.
            state.update(status='launch_failed', error_type=type(error).__name__)
        else:
            state.update(child_pid=child.pid, status='running')
            timed_out = False
            try:
                while child.poll() is None:
                    remaining = config.timeout_s - (time.monotonic() - started)
                    if remaining <= 0:
                        timed_out = True
                        child.kill()  # Only the Popen handle created here.
                        break
                    state['updated_at'] = time.time()
                    _atomic_json(root / 'heartbeat.json', state)
                    try:
                        child.wait(timeout=min(0.1, remaining))
                    except subprocess.TimeoutExpired:
                        pass
            finally:
                if child.poll() is None:
                    child.kill()
                returncode = child.wait(timeout=5)
            state.update(status='timed_out' if timed_out else ('completed' if returncode == 0 else 'failed'),
                         returncode=returncode)
        for output in (out, err):
            output.flush()
            os.fsync(output.fileno())
    state.update(elapsed_s=time.monotonic() - started, updated_at=time.time(), logs=_logs(root))
    _atomic_json(root / 'heartbeat.json', state)
    _atomic_json(root / 'exit.json', state)
    return state
