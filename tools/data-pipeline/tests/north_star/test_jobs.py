"""Real bounded subprocess fixtures; never launches a collector or service."""
import importlib
import json
import os
from pathlib import Path
import sys
import pytest
from dataclasses import FrozenInstanceError, replace

sys.path.insert(0, str(Path(__file__).parents[2] / 'src'))


def make_config(tmp_path, code='print("fixture")', **kwargs):
    jobs = importlib.import_module('north_star.jobs')
    return jobs.JobConfig(stage='test', config_id='v1',
                          argv=(sys.executable, '-c', code), cwd=tmp_path,
                          timeout_s=5, **kwargs)


def test_config_is_deeply_immutable_and_identity_covers_execution(tmp_path):
    jobs = importlib.import_module('north_star.jobs')
    argv = [sys.executable, '-c', 'print("fixture")']
    config = jobs.JobConfig(stage='test', config_id='v1', argv=argv,
                            cwd=tmp_path, timeout_s=5)
    original = config.job_id
    argv.append('changed')
    assert config.job_id == original
    with pytest.raises(FrozenInstanceError):
        config.stage = 'changed'
    for change in ({'stage': 'new'}, {'config_id': 'v2'}, {'timeout_s': 6},
                   {'argv': (sys.executable, '-c', 'pass')}, {'cwd': tmp_path.parent}):
        assert replace(config, **change).job_id != original
    for change in ({'timeout_s': 0}, {'timeout_s': float('inf')},
                   {'argv': 'python -c pass'}, {'argv': ('python', '-c', 'pass')},
                   {'cwd': Path('.')}, {'stage': ''}, {'config_id': ''}):
        with pytest.raises((TypeError, ValueError)):
            replace(config, **change)


def test_timeout_updates_heartbeat_and_reports_owned_child_exit(tmp_path):
    import concurrent.futures
    import time
    jobs = importlib.import_module('north_star.jobs')
    root = tmp_path / 'job'
    config = replace(make_config(tmp_path, 'import time; print("alive", flush=True); time.sleep(30)'), timeout_s=0.8)
    observed = []
    with concurrent.futures.ThreadPoolExecutor() as pool:
        future = pool.submit(jobs.run_job, config, root)
        while not future.done():
            try:
                state = json.loads((root / 'heartbeat.json').read_text())
                if state['status'] == 'running':
                    observed.append(state['updated_at'])
            except FileNotFoundError:
                pass
            time.sleep(0.02)
        result = future.result()
    assert len(set(observed)) >= 2
    assert result['status'] == 'timed_out'
    assert isinstance(result['returncode'], int) and result['returncode'] != 0
    assert result['elapsed_s'] < 4
    assert (root / 'stdout.log').read_text().strip() == 'alive'
    assert json.loads((root / 'exit.json').read_text()) == result


def test_os_singleton_rejects_competing_process_and_releases_on_exit(tmp_path):
    import subprocess
    import time
    jobs = importlib.import_module('north_star.jobs')
    root = tmp_path / 'job'
    root.mkdir()
    # Stale PID text is irrelevant: the operating system owns the lock.
    (root / 'job.lock').write_text('999999 stale PID')
    ready = tmp_path / 'ready'
    code = ('import os,sys,time; from pathlib import Path; '
            'sys.path.insert(0, sys.argv[1]); '
            'from north_star.jobs import singleton_lock; '
            '\nwith singleton_lock(Path(sys.argv[2])):\n'
            ' Path(sys.argv[3]).write_text("locked"); time.sleep(0.8); os._exit(0)')
    child = subprocess.Popen([sys.executable, '-c', code,
                              str(Path(__file__).parents[2] / 'src'), str(root), str(ready)],
                             cwd=tmp_path, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        deadline = time.monotonic() + 5
        while not ready.exists() and child.poll() is None and time.monotonic() < deadline:
            time.sleep(0.01)
        assert ready.exists(), child.communicate(timeout=5)
        with pytest.raises(jobs.JobBusy):
            jobs.run_job(make_config(tmp_path), root)
        assert not (root / 'stdout.log').exists()
        assert child.wait(timeout=5) == 0
        assert jobs.run_job(make_config(tmp_path), root)['status'] == 'completed'
        assert (root / 'job.lock').exists()
    finally:
        if child.poll() is None:
            child.kill()  # Only this test's synthetic child.
        child.communicate(timeout=5)


def test_exact_completed_resume_verifies_logs_without_launch(tmp_path):
    jobs = importlib.import_module('north_star.jobs')
    config = make_config(tmp_path, 'from pathlib import Path; p=Path("count"); p.write_text(p.read_text()+"x" if p.exists() else "x"); print("one run")')
    root = tmp_path / 'job'
    first = jobs.run_job(config, root)
    assert jobs.run_job(config, root) == first
    assert (tmp_path / 'count').read_text() == 'x'
    assert first['logs']['stdout.log']['bytes'] > 0
    (root / 'stdout.log').write_text('corrupt')
    with pytest.raises(jobs.JobResumeError):
        jobs.run_job(config, root)
    assert (tmp_path / 'count').read_text() == 'x'


@pytest.mark.parametrize('case', ['config', 'failed', 'timeout', 'orphan', 'missing_log', 'missing_exit', 'bad_exit', 'bad_identity', 'bad_heartbeat'])
def test_nonmatching_failed_incomplete_or_corrupt_job_never_retries(tmp_path, case):
    jobs = importlib.import_module('north_star.jobs')
    root = tmp_path / 'job'
    code = 'from pathlib import Path; p=Path("count"); p.write_text(p.read_text()+"x" if p.exists() else "x")'
    if case == 'failed':
        code += '; import sys; print("bad input"); sys.exit(7)'
    if case == 'timeout':
        code += '; import time; time.sleep(30)'
    config = replace(make_config(tmp_path, code), timeout_s=0.4 if case == 'timeout' else 5)
    if case == 'orphan':
        root.mkdir()
        (root / 'heartbeat.json').write_text('{}')
    else:
        result = jobs.run_job(config, root)
        if case == 'failed':
            assert result['status'] == 'failed' and result['returncode'] == 7
        if case == 'config':
            config = replace(config, config_id='changed')
        elif case == 'missing_log':
            (root / 'stderr.log').unlink()
        elif case == 'missing_exit':
            (root / 'exit.json').unlink()
        elif case == 'bad_exit':
            (root / 'exit.json').write_text('{broken')
        elif case == 'bad_identity':
            (root / 'identity.json').write_text('{}')
        elif case == 'bad_heartbeat':
            (root / 'heartbeat.json').write_text('{}')
    with pytest.raises(jobs.JobResumeError):
        jobs.run_job(config, root)
    if case != 'orphan':
        assert (tmp_path / 'count').read_text() == 'x'
    else:
        assert not (tmp_path / 'count').exists()


def test_launch_error_has_sanitized_exit_and_no_retry(tmp_path):
    jobs = importlib.import_module('north_star.jobs')
    secret = 'SECRET_IN_EXECUTABLE_AND_ARG'
    config = replace(make_config(tmp_path), argv=(str(tmp_path / secret), secret))
    root = tmp_path / 'job'
    result = jobs.run_job(config, root)
    assert result['status'] == 'launch_failed'
    assert result['returncode'] is None and result['child_pid'] is None
    assert result['error_type'] == 'FileNotFoundError'
    for receipt in root.glob('*.json'):
        assert secret not in receipt.read_text()
    with pytest.raises(jobs.JobResumeError):
        jobs.run_job(config, root)


def test_environment_is_snapshotted_and_in_identity(tmp_path, monkeypatch):
    jobs = importlib.import_module('north_star.jobs')
    monkeypatch.setenv('NORTH_STAR_TEST_SECRET', 'original-secret')
    config = make_config(tmp_path, 'import os; print(os.environ["NORTH_STAR_TEST_SECRET"])')
    monkeypatch.setenv('NORTH_STAR_TEST_SECRET', 'changed-secret')
    assert make_config(tmp_path).env != config.env
    assert replace(config, env=make_config(tmp_path).env).job_id != config.job_id
    root = tmp_path / 'job'
    jobs.run_job(config, root)
    assert (root / 'stdout.log').read_text().strip() == 'original-secret'
    assert 'original-secret' not in repr(config)
    for receipt in root.glob('*.json'):
        assert 'original-secret' not in receipt.read_text()


def test_shell_metacharacters_are_literal_arguments(tmp_path):
    jobs = importlib.import_module('north_star.jobs')
    config = make_config(tmp_path, 'import sys; print(sys.argv[1])')
    literal = '& echo injected > UNWANTED'
    config = replace(config, argv=(*config.argv, literal))
    jobs.run_job(config, tmp_path / 'job')
    assert (tmp_path / 'job' / 'stdout.log').read_text().strip() == literal
    assert not (tmp_path / 'UNWANTED').exists()


def test_atomic_publication_failure_preserves_previous_and_is_bounded(tmp_path, monkeypatch):
    jobs = importlib.import_module('north_star.jobs')
    target = tmp_path / 'heartbeat.json'
    jobs._atomic_json(target, {'status': 'old'})
    calls = []
    def deny(*args):
        calls.append(args)
        raise PermissionError('synthetic sharing violation')
    monkeypatch.setattr(jobs.os, 'replace', deny)
    with pytest.raises(PermissionError):
        jobs._atomic_json(target, {'status': 'new'})
    assert json.loads(target.read_text()) == {'status': 'old'}
    assert len(calls) == 6
    assert not list(tmp_path.glob('*.pending'))


@pytest.mark.parametrize('suffix', ['.cmd', '.bat', '.CMD'])
def test_windows_batch_files_cannot_implicitly_invoke_shell(tmp_path, suffix):
    config = make_config(tmp_path)
    with pytest.raises(ValueError, match='shell'):
        replace(config, argv=(str(tmp_path / ('fixture' + suffix)),))


def test_invalid_environment_and_nested_mutation(tmp_path):
    config = make_config(tmp_path)
    pairs = [['FIXTURE', 'value']]
    frozen = replace(config, env=pairs)
    pairs[0][1] = 'mutated'
    assert frozen.env == (('FIXTURE', 'value'),)
    for value in ([('A', 'one'), ('A', 'two')], [('BAD=KEY', 'value')],
                  [('', 'value')], [('KEY', 'bad\0value')]):
        with pytest.raises(ValueError):
            replace(config, env=value)


def test_success_has_durable_logs_exit_and_owner(tmp_path):
    jobs = importlib.import_module('north_star.jobs')
    cwd = tmp_path / 'working'
    cwd.mkdir()
    secret = 'synthetic-command-secret-DO-NOT-RECORD'
    config = jobs.JobConfig(
        stage='fixture', config_id='v1', cwd=cwd,
        argv=(sys.executable, '-c',
              'import os,sys; print(os.getcwd()); print("error-stream", file=sys.stderr)', secret),
        timeout_s=5,
    )
    root = tmp_path / 'job'
    result = jobs.run_job(config, root)
    assert result['status'] == 'completed'
    assert result['returncode'] == 0
    assert result['owner_pid'] == os.getpid()
    assert result['child_pid'] != os.getpid()
    assert result['job_id'] == config.job_id
    assert (root / 'stdout.log').read_text().strip() == str(cwd)
    assert (root / 'stderr.log').read_text().strip() == 'error-stream'
    assert json.loads((root / 'exit.json').read_text()) == result
    heartbeat = json.loads((root / 'heartbeat.json').read_text())
    assert heartbeat['status'] == 'completed'
    assert heartbeat['job_id'] == config.job_id
    for path in root.glob('*.json'):
        assert secret not in path.read_text()
    assert not list(root.glob('*.pending'))
