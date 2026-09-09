# Bounded Windows job-runner foundation — tested, NOT deployed

## Requirement and boundary

Read `master/NORTH_STAR_WINDOWS_MASTER_V5.md:409–414` and
`BUILD_PLAN.md:264–269,492`: long aggregation/export/certification jobs require
reviewed independent Windows ownership, durable logs, exit status, immutable
stage/config identity, exact resume and a gateway-survival rehearsal.

This change supplies the bounded synchronous **foundation only**. It does NOT
satisfy independent service/task ownership or gateway-survival acceptance. No
collector was launched/stopped, no scheduler/service was installed, no production
capture workspace was changed, and no git commit was made.

Owned files (relative to `tools/data-pipeline`):
- `src/north_star/jobs.py`
- `tests/north_star/test_jobs.py`
- `north_star/BUILD_JOBS_RECEIPT.md`

## Implemented contract

- `JobConfig`: frozen configuration, deep-copied argv/environment, absolute
  executable and existing absolute cwd, positive finite timeout; batch files
  rejected to avoid Windows implicit-shell execution. Hash identity includes
  stage, config version, argv, cwd, timeout and the exact environment snapshot.
- `run_job(config, root)`: one dedicated trusted attempt directory; stdlib
  `Popen`, explicit argv/cwd/environment, `shell=False`, closed stdin, stdout and
  stderr streamed directly to files (not buffered in pipes).
- `singleton_lock(root)`: nonblocking Windows `msvcrt` byte-range OS lock, held
  across verification/execution/publication. Lock file is never unlinked; stale
  PID text is irrelevant, and process death releases the OS lock. Receipt PIDs
  are diagnostics, not authority to adopt or kill a process.
- Immutable identity file uses existing `io._publish_new`. Receipts reuse
  `fs_integrity.checked_path` / `stable_reader`. Heartbeat and exit JSON use
  same-directory temp, flush/fsync and atomic replace. A witnessed Windows
  reader-sharing violation required a finite six-attempt publication retry with
  10ms between attempts; this never retries a job.
- Running heartbeat, owner/child PID, terminal status, actual returncode,
  elapsed time and log byte counts/SHA256. Timeout kills/waits only the direct
  Popen child owned by that invocation. Failure and launch-failure recorded;
  launch errors persist only exception class, never exception text.
- Exact completed result resumes only after identity, successful exit,
  matching final heartbeat and both log hashes/sizes verify. Failed, timeout,
  mismatched, missing, corrupt and orphaned state never silently reruns.
- JSON receipts contain no raw command, cwd, stage/config text or environment.
  Child output logs can contain child-emitted secrets and require protected
  storage. Identity hashes are not encryption.

## Real Windows test receipt

Environment observed: `Windows-10-10.0.26200-SP0`, Python 3.11.15.
UTC observation: `2026-09-09T17:50:47.270931+00:00` (before final reruns).
Working directory: `D:/repos/mev_bot-north-star/tools/data-pipeline`.

Final executed commands and actual output:

```text
python -m pytest tests/north_star/test_jobs.py tests/north_star/test_atomic_io.py tests/north_star/test_fs_integrity.py -q --tb=short
84 passed in 3.56s
exit code: 0

python -m pytest tests/north_star/test_jobs.py -q --tb=short
22 passed in 3.07s
exit code: 0

git diff --check
(no output)
exit code: 0
```

Tests use real short `sys.executable` children: stdout/stderr and explicit cwd,
nonzero exit, stalled child timeout, shell metacharacters as literal argv,
concurrent cross-process lock contention and abrupt `os._exit` lock release,
heartbeat reads while Windows replaces the file, exact resume with an on-disk
execution counter, environment pinning, missing/corrupt artifacts and sanitized
launch failure. One deliberately injected filesystem sharing error verifies the
finite publication retry budget and preservation of the prior JSON; it is not
represented as production output. No synthetic fixture is claimed to be source
collection or dataset evidence.

### Red → green evidence

- Initial test: missing jobs module, 1 failed. Test import setup initially lacked
  `src`; corrected with the repository-local source path. First green: 1 passed.
- Mutable argv/config test: 1 failed / 1 passed → 2 passed.
- Timeout/heartbeat: real TimeoutExpired, 1 failed / 2 passed → 3 passed.
- Cross-process singleton: missing lock API, 1 failed / 3 passed → 4 passed.
- Resume/failure/launch cases: 12 failed / 3 passed (including the real Windows
  sharing violation) → 15 passed.
- Environment identity: 1 failed / 17 passed → 18 passed.
- Windows batch rejection: 3 failed / 19 passed → 22 passed.
- Final adjacent shared-module regression: 84 passed; shared modules unmodified.

Final source SHA256:
- `src/north_star/jobs.py`: `5f23e28b8656bb7658f91f7b2660d423d73798d9db5ff2e8d5d970d0aaaec897`
- `tests/north_star/test_jobs.py`: `a68eb7a08d45fe4aa37d6efa7f26fe18e41e330c68f8901a4ce783fc1e3ccf02`

## Parent integration and independent review

Independent review `deleg_15b34399` found no must-fix within the synchronous direct-child scope. Parent then ran the real accounting pytest target through this wrapper: job `a3428ad9aaf9a55c38527c689cf6b30b9fcad8ce4c5315f39e8333b29836bf91`, 58 passed, returncode0. Exact second invocation returned the verified original result without relaunch. Receipt/log hashes were independently read back from `D:/mev_bot-artifacts/north_star/jobs/accounting_fixture_v1/`. This is integration evidence, not durable service deployment.

## Remaining acceptance / limitations

- Review and deploy an independent Windows owner, then test actual gateway
  restart survival; this synchronous library alone does not provide it.
- Only a directly owned child is supported, not descendant-tree supervision.
  No PID-based orphan adoption, recovery kill, or automatic retry exists.
- Caller must pin code/input/config versions in `config_id`; executable/file
  contents are not automatically hashed. Prefer explicit minimal environment
  for stable cross-session identity rather than ambient session variables.
- Completion verifies execution receipts/logs, not arbitrary application output
  semantics. Stages must validate and atomically publish their outputs before
  exit zero; independent dataset acceptance remains required.
- Dedicated trusted directory scope is the singleton namespace; different roots
  are not a machine-global lock. Do not delete/repurpose interrupted attempt
  directories to force retries: an orphan child may still exist.
- No disk/RAM/output quota or resource preflight; duration is bounded, but OS
  process creation and filesystem calls themselves have no hard wall-clock
  guarantee. Logs stream to disk and are flushed/fsynced at normal finalization.
- Filesystem errors/crashes can leave incomplete state without exit receipt;
  such state fails closed. Receipt pair is not one multi-file transaction.
- POSIX fallback exists but Windows is the platform actually exercised here.
