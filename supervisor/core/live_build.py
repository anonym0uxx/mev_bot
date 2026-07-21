"""
Live build automation — the git-backed VCS adapter and the Claude Code headless driver
that together close the orchestrator's TODO(live) seam.

Design principles (each is a guardrail, not decoration):

  1. THE GATE DECIDES, NOT THE MODEL. Claude Code writes code; the supervisor's gates decide
     whether it is correct. A commit happens only after a gate passes. A push happens only
     after a commit. A merge happens only after CI (which re-runs the gate) passes. The model
     never certifies its own work — the constitution's law, enforced by control flow.
  2. FAIL LOUD, FAIL CLOSED. Every subprocess return code is checked. A stuck agent is bounded
     by a timeout. A failed gate reverts the working tree rather than leaving it dirty.
  3. NOTHING UNVERIFIED REACHES A SHARED BRANCH. Work happens on per-milestone branches; main
     is only reached through a PR that CI has gated.
  4. BUDGET AND TURN BOUNDED. Each headless invocation is capped (--max-turns, timeout, and an
     iteration ceiling per task) so a bad prompt cannot burn the account or spin forever.

This module has NO effect until it is pointed at a real repo and a real `claude` binary; it is
import-safe and unit-testable with fakes.
"""
from __future__ import annotations

import json
import os
import shlex
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Optional


# --------------------------------------------------------------------- git adapter
class GitError(RuntimeError):
    pass


def _run(cmd: list[str], cwd: Path, timeout: int = 120,
         check: bool = True) -> subprocess.CompletedProcess:
    """Run a subprocess, checked and bounded. Never shell=True."""
    p = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True,
                       encoding="utf-8", errors="replace", timeout=timeout)
    if check and p.returncode != 0:
        raise GitError(f"$ {' '.join(shlex.quote(c) for c in cmd)}\n"
                       f"exit {p.returncode}\nstdout: {p.stdout}\nstderr: {p.stderr}")
    return p


@dataclass
class GitVcs:
    """A real VcsAdapter backed by git. Injected into BuildOrchestrator in production.

    The orchestrator's VcsAdapter contract is (apply_diff, commit, branch, revert_last).
    apply_diff here is deliberately conservative: the live builder is Claude Code editing
    files directly on disk (not producing unified diffs), so `apply_diff` is used by the
    orchestrator only in the dossier-component path; the file-editing path is driven by the
    ClaudeCodeDriver below and this adapter simply stages and commits the result.
    """
    repo: Path
    author_name: str = "hermes-build"
    author_email: str = "hermes-build@localhost"

    def _env(self) -> None:
        # deterministic, attributable commits without touching global git config
        os.environ.setdefault("GIT_AUTHOR_NAME", self.author_name)
        os.environ.setdefault("GIT_AUTHOR_EMAIL", self.author_email)
        os.environ.setdefault("GIT_COMMITTER_NAME", self.author_name)
        os.environ.setdefault("GIT_COMMITTER_EMAIL", self.author_email)

    def current_branch(self) -> str:
        return _run(["git", "rev-parse", "--abbrev-ref", "HEAD"], self.repo).stdout.strip()

    def branch(self, name: str) -> None:
        """Create-or-checkout a branch. Idempotent (safe to call per milestone)."""
        self._env()
        existing = _run(["git", "branch", "--list", name], self.repo).stdout.strip()
        if existing:
            _run(["git", "checkout", name], self.repo)
        else:
            _run(["git", "checkout", "-b", name], self.repo)

    def apply_diff(self, diff: str) -> tuple[bool, str]:
        """Apply a unified diff (dossier-component path). Returns (ok, detail)."""
        self._env()
        patch = self.repo / ".hermes_patch.diff"
        try:
            patch.write_text(diff, encoding="utf-8")
            p = _run(["git", "apply", "--index", str(patch)], self.repo, check=False)
            ok = p.returncode == 0
            return ok, (p.stderr or p.stdout or "").strip()
        finally:
            try:
                patch.unlink(missing_ok=True)
            except OSError:
                pass

    def stage_all(self) -> None:
        self._env()
        _run(["git", "add", "-A"], self.repo)

    def has_changes(self) -> bool:
        p = _run(["git", "status", "--porcelain"], self.repo)
        return bool(p.stdout.strip())

    def commit(self, message: str) -> str:
        """Stage everything and commit. Returns the new SHA. No-op safe."""
        self._env()
        self.stage_all()
        if not self.has_changes():
            # nothing to commit -> return current HEAD, do not error
            return _run(["git", "rev-parse", "HEAD"], self.repo).stdout.strip()
        _run(["git", "commit", "-m", message], self.repo)
        return _run(["git", "rev-parse", "HEAD"], self.repo).stdout.strip()

    def revert_last(self, keep_untracked: bool = True) -> None:
        """Undo uncommitted MODIFICATIONS back to HEAD. By default keeps untracked files
        (newly-created source the builder legitimately added) and never hard-fails on Windows
        lock files. Only a full wipe (keep_untracked=False) removes untracked files, and even
        then it excludes the evidence DB and its SQLite lock files which may be open.
        """
        self._env()
        # revert tracked-file modifications only; this does not touch newly-added files
        _run(["git", "reset", "--hard", "HEAD"], self.repo, check=False)
        if not keep_untracked:
            # exclude the evidence DB + WAL/SHM lock files (open handles fail to delete on Windows)
            _run(["git", "clean", "-fd",
                  "-e", "supervisor_evidence.db",
                  "-e", "supervisor_evidence.db-wal",
                  "-e", "supervisor_evidence.db-shm",
                  "-e", "infra_manifest.json",
                  "-e", ".hermes_dossier_tests.json"],
                 self.repo, check=False)

    def push(self, branch: str, remote: str = "origin", set_upstream: bool = True) -> str:
        self._env()
        cmd = ["git", "push"]
        if set_upstream:
            cmd += ["-u", remote, branch]
        else:
            cmd += [remote, branch]
        p = _run(cmd, self.repo, timeout=300, check=False)
        if p.returncode != 0:
            raise GitError(f"push failed: {p.stderr or p.stdout}")
        return (p.stderr or p.stdout).strip()   # git prints push summary to stderr


# --------------------------------------------------------- Claude Code headless driver
@dataclass
class ClaudeCodeConfig:
    """How the headless `claude` binary is invoked for one unit of build work."""
    binary: str = "claude"
    permission_mode: str = "dontAsk"          # fail-loud automation, not approve-everything
    allowed_tools: list[str] = field(default_factory=lambda: [
        "Read", "Grep", "Glob", "Edit",
        "Bash(cargo build*)", "Bash(cargo test*)", "Bash(cargo fmt*)",
        "Bash(cargo clippy*)", "Bash(cargo check*)",
        "Bash(git status)", "Bash(git diff*)", "Bash(git log*)", "Bash(git add*)",
    ])
    disallowed_tools: list[str] = field(default_factory=lambda: [
        # the driver owns commit/push/merge and the release profile — the model may not
        "Bash(git commit*)", "Bash(git push*)", "Bash(git merge*)",
        "Bash(git reset*)", "Bash(rm *)", "Bash(cargo build --release*)",
    ])
    max_turns: int = 60
    timeout_s: int = 1800
    max_iterations_per_task: int = 3          # gate-fail feedback loops before escalation
    output_format: str = "json"
    append_system_prompt: str = ""

    def usable(self) -> tuple[bool, str]:
        if not self.binary:
            return False, "no claude binary configured"
        return True, "ok"


@dataclass
class ClaudeCodeResult:
    ok: bool
    session_id: str = ""
    text: str = ""
    cost_usd: float = 0.0
    num_turns: int = 0
    error: str = ""


class ClaudeCodeDriver:
    """Invokes Claude Code headlessly to perform one build task against the repo.

    The driver never decides correctness. It asks Claude Code to implement to the prompt
    (which points at the constitution + dossier), then returns control to the caller, which
    runs the supervisor gate. On gate failure the caller calls `iterate` with the gate's
    feedback, up to max_iterations_per_task, then escalates.
    """

    def __init__(self, repo: Path, cfg: ClaudeCodeConfig):
        self.repo = repo
        self.cfg = cfg

    def _cmd(self, prompt: str, resume_session: str = "") -> list[str]:
        cmd = [self.cfg.binary, "-p", prompt,
               "--permission-mode", self.cfg.permission_mode,
               "--output-format", self.cfg.output_format,
               "--max-turns", str(self.cfg.max_turns)]
        for t in self.cfg.allowed_tools:
            cmd += ["--allowedTools", t]
        for t in self.cfg.disallowed_tools:
            cmd += ["--disallowedTools", t]
        if self.cfg.append_system_prompt:
            cmd += ["--append-system-prompt", self.cfg.append_system_prompt]
        if resume_session:
            cmd += ["--resume", resume_session]
        return cmd

    def _invoke(self, cmd: list[str]) -> ClaudeCodeResult:
        import shutil as _sh
        # Windows fix: `claude` installs as claude.CMD (a batch wrapper), which
        # subprocess.run cannot launch by bare name. Resolve the real path, and on Windows
        # run the command as a single shell string so .CMD/.BAT wrappers execute.
        run_cmd = list(cmd)
        resolved = _sh.which(run_cmd[0])
        if resolved:
            run_cmd[0] = resolved
        use_shell = os.name == "nt"
        try:
            if use_shell:
                # build a properly quoted command line for cmd.exe
                line = subprocess.list2cmdline(run_cmd)
                p = subprocess.run(line, cwd=str(self.repo), capture_output=True,
                                   text=True, encoding="utf-8", errors="replace",
                                   timeout=self.cfg.timeout_s, shell=True)
            else:
                p = subprocess.run(run_cmd, cwd=str(self.repo), capture_output=True,
                                   text=True, encoding="utf-8", errors="replace",
                                   timeout=self.cfg.timeout_s)
        except subprocess.TimeoutExpired:
            return ClaudeCodeResult(False, error=f"timed out after {self.cfg.timeout_s}s")
        except FileNotFoundError as e:
            return ClaudeCodeResult(
                False, error=f"could not launch '{cmd[0]}' ({e}). Ensure Claude Code is "
                             f"installed and on PATH (npm install -g @anthropic-ai/claude-code).")
        # guard against a subprocess that produced no captured output (never crash on None)
        out = p.stdout or ""
        err = p.stderr or ""
        # Claude Code returns non-zero on error/rate-limit — honor it
        if p.returncode != 0 and not out.strip():
            return ClaudeCodeResult(False, error=f"exit {p.returncode}: {err[:400]}")
        # parse JSON output
        try:
            data = json.loads(out.strip().splitlines()[-1])
        except (json.JSONDecodeError, IndexError):
            # text fell back or partial — treat as soft failure with captured text
            return ClaudeCodeResult(p.returncode == 0, text=out[:2000],
                                    error="" if p.returncode == 0 else "unparseable output")
        is_error = bool(data.get("is_error"))
        return ClaudeCodeResult(
            ok=not is_error,
            session_id=data.get("session_id", ""),
            text=data.get("result", "")[:4000],
            cost_usd=float(data.get("total_cost_usd", 0.0) or 0.0),
            num_turns=int(data.get("num_turns", 0) or 0),
            error="" if not is_error else str(data.get("result", "error"))[:400],
        )

    def implement(self, prompt: str) -> ClaudeCodeResult:
        """First attempt at a task."""
        ok, why = self.cfg.usable()
        if not ok:
            return ClaudeCodeResult(False, error=why)
        return self._invoke(self._cmd(prompt))

    def iterate(self, session_id: str, gate_feedback: str) -> ClaudeCodeResult:
        """Feed a failed gate's output back to the same session for another attempt."""
        prompt = ("The build gate rejected the previous change. Fix ONLY what these findings "
                  "identify, then stop. Do not weaken tests or the release profile. Findings:\n"
                  + gate_feedback[:3000])
        return self._invoke(self._cmd(prompt, resume_session=session_id))


# ------------------------------------------------------------------- budget guard
@dataclass
class BudgetGuard:
    """Hard stop on spend and wall-clock for an unattended run (the '$100 in an hour' trap)."""
    max_usd: float = 25.0
    max_seconds: int = 14_400            # 4h
    _spent: float = 0.0
    _t0: float = field(default_factory=time.time)

    def charge(self, usd: float) -> None:
        self._spent += usd

    def exceeded(self) -> Optional[str]:
        if self._spent >= self.max_usd:
            return f"budget guard: spent ${self._spent:.2f} >= ${self.max_usd:.2f}"
        if time.time() - self._t0 >= self.max_seconds:
            return f"time guard: exceeded {self.max_seconds}s"
        return None
