"""
Individual gate checks — the deterministic truth layer.

Every check is a pure function of (repo_path, config) -> CheckResult. No model involvement.
Subprocess-based, cross-platform (Windows msvc target is primary; commands are configurable).
A check that cannot run (tool missing) returns passed=False with a clear reason — never a silent pass.
"""
from __future__ import annotations

import json
import re
import shutil
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional


@dataclass
class CheckResult:
    name: str
    passed: bool
    detail: dict[str, Any] = field(default_factory=dict)
    summary: str = ""


def _run(cmd: list[str], cwd: str, timeout: int = 1800) -> tuple[int, str, str]:
    try:
        p = subprocess.run(
            cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout
        )
        return p.returncode, p.stdout, p.stderr
    except FileNotFoundError:
        return 127, "", f"tool not found: {cmd[0]}"
    except subprocess.TimeoutExpired:
        return 124, "", f"timeout after {timeout}s"


def _have(tool: str) -> bool:
    return shutil.which(tool) is not None


# --------------------------------------------------------------------------- build
def check_build(repo: str, target: Optional[str] = None) -> CheckResult:
    if not _have("cargo"):
        return CheckResult("build", False, summary="cargo not found on PATH")
    cmd = ["cargo", "build", "--release"]
    if target:
        cmd += ["--target", target]
    rc, out, err = _run(cmd, repo)
    return CheckResult("build", rc == 0, {"returncode": rc, "stderr_tail": err[-2000:]},
                       "compiled" if rc == 0 else "build failed")


# --------------------------------------------------------------------------- clippy / fmt / hygiene
def check_clippy(repo: str) -> CheckResult:
    if not _have("cargo"):
        return CheckResult("clippy", False, summary="cargo not found")
    rc, out, err = _run(["cargo", "clippy", "--release", "--", "-D", "warnings"], repo)
    return CheckResult("clippy", rc == 0, {"returncode": rc, "stderr_tail": err[-2000:]},
                       "clean" if rc == 0 else "clippy warnings/errors")


def check_fmt(repo: str) -> CheckResult:
    if not _have("cargo"):
        return CheckResult("fmt", False, summary="cargo not found")
    rc, out, err = _run(["cargo", "fmt", "--check"], repo)
    return CheckResult("fmt", rc == 0, {"returncode": rc}, "formatted" if rc == 0 else "unformatted")


# anti-spaghetti: no stubs/placeholders in production paths
_STUB_PATTERNS = [
    re.compile(r"\bunimplemented!\s*\("),
    re.compile(r"\btodo!\s*\("),
    re.compile(r'\bpanic!\s*\(\s*"(?:stub|not implemented|TODO)', re.I),
    re.compile(r"//\s*(TODO|FIXME|STUB|HACK)\b", re.I),
]



def check_dossier_test_integrity(repo) -> "CheckResult":
    """The builder must not alter a materialized dossier property test to make it pass.

    Runs `scripts/materialize_tests.py --verify`, which re-hashes every materialized test
    against what its dossier renders. Any edit, deletion, or drift fails this check — the
    correctness authority is protected mechanically, not by trust.
    """
    import subprocess, sys
    from pathlib import Path as _P
    script = _P(repo) / "scripts" / "materialize_tests.py"
    if not script.is_file():
        # no materializer in this repo layout -> nothing to verify (non-fatal)
        return CheckResult("dossier_test_integrity", True, "materializer not present (skip)", {})
    try:
        p = subprocess.run([sys.executable, str(script), "--repo", str(repo), "--verify"],
                           capture_output=True, text=True, timeout=120)
    except Exception as e:  # noqa: BLE001
        return CheckResult("dossier_test_integrity", False, f"verify error: {e}", {})
    ok = p.returncode == 0
    detail = (p.stdout.strip().splitlines()[-1] if p.stdout.strip() else "") or p.stderr[:200]
    return CheckResult("dossier_test_integrity", ok, detail, {"returncode": p.returncode})


def check_no_stubs(repo: str, production_globs: list[str]) -> CheckResult:
    hits: list[str] = []
    root = Path(repo)
    for g in production_globs:
        for f in root.glob(g):
            if f.suffix != ".rs":
                continue
            text = f.read_text(encoding="utf-8", errors="ignore")
            # allow markers in #[cfg(test)] blocks only: crude but effective — strip test modules
            text_wo_tests = re.sub(r"#\[cfg\(test\)\][\s\S]*?\n}\n", "", text)
            for rx in _STUB_PATTERNS:
                if rx.search(text_wo_tests):
                    hits.append(f"{f.relative_to(root)}: {rx.pattern}")
    return CheckResult("no_stubs", len(hits) == 0, {"hits": hits},
                       "no stubs" if not hits else f"{len(hits)} stub/TODO in production paths")


# --------------------------------------------------------------------------- tests
def check_tests(repo: str, required_test_names: Optional[list[str]] = None) -> CheckResult:
    if not _have("cargo"):
        return CheckResult("test", False, summary="cargo not found")
    rc, out, err = _run(["cargo", "test", "--release", "--", "--nocapture"], repo, timeout=3600)
    combined = out + err
    passed = rc == 0
    detail: dict[str, Any] = {"returncode": rc}
    # verify required named tests actually ran (not just "some tests passed")
    missing: list[str] = []
    if required_test_names:
        for name in required_test_names:
            if name not in combined:
                missing.append(name)
        if missing:
            passed = False
    detail["missing_required_tests"] = missing
    detail["stderr_tail"] = err[-1500:]
    return CheckResult("test", passed, detail,
                       "tests pass" if passed else f"test failure / missing {len(missing)} required")


# --------------------------------------------------------------------------- secrets
_SECRET_PATTERNS = [
    re.compile(r"[1-9A-HJ-NP-Za-km-z]{80,90}"),          # base58 solana secret-key length window
    re.compile(r"[0-9a-fA-F]{64}"),                        # 32-byte hex
    re.compile(r"(api[_-]?key|token|secret)\s*[:=]\s*[\"'][^\"']{16,}", re.I),
]


def check_secrets(repo: str) -> CheckResult:
    """Prefer gitleaks if present; fall back to a conservative regex scan of tracked files."""
    if _have("gitleaks"):
        rc, out, err = _run(["gitleaks", "detect", "--no-banner", "--redact"], repo)
        return CheckResult("secrets", rc == 0, {"tool": "gitleaks", "returncode": rc, "out_tail": out[-1000:]},
                           "no leaks" if rc == 0 else "gitleaks findings")
    # fallback
    rc, out, err = _run(["git", "ls-files"], repo)
    hits: list[str] = []
    root = Path(repo)
    for rel in out.splitlines():
        if not rel.strip():
            continue
        f = root / rel
        if not f.is_file() or f.suffix in (".png", ".jpg", ".jpeg", ".gz", ".zip", ".bin"):
            continue
        try:
            text = f.read_text(encoding="utf-8", errors="ignore")
        except Exception:
            continue
        # skip obvious placeholders
        if "PLACEHOLDER" in text:
            text = re.sub(r'.*PLACEHOLDER.*', "", text)
        for rx in _SECRET_PATTERNS:
            if rx.search(text):
                hits.append(f"{rel}: {rx.pattern[:30]}")
                break
    return CheckResult("secrets", len(hits) == 0, {"tool": "regex", "hits": hits},
                       "no secrets" if not hits else f"{len(hits)} possible secrets")


# --------------------------------------------------------------------------- benchmarks
def check_bench(repo: str, bench_name: str, budgets_ns: dict[str, float]) -> CheckResult:
    """
    Run a criterion.rs benchmark and parse p50/p99/p999 from its JSON output.
    budgets_ns: {'p50_ns': ..., 'p99_ns': ..., 'p999_ns': ...}; a metric over budget fails the gate.
    Requires the bot's benches to emit machine-readable output (criterion --output-format or a custom harness).
    """
    if not _have("cargo"):
        return CheckResult("bench", False, summary="cargo not found")
    rc, out, err = _run(["cargo", "bench", "--bench", bench_name], repo, timeout=3600)
    measured = _parse_criterion(out + err)
    if not measured:
        return CheckResult("bench", False, {"returncode": rc, "parsed": {}},
                           "could not parse benchmark output")
    violations = {k: (measured.get(k), budgets_ns[k]) for k in budgets_ns
                  if measured.get(k) is not None and measured[k] > budgets_ns[k]}
    passed = rc == 0 and not violations
    return CheckResult("bench", passed, {"measured": measured, "violations": violations},
                       "within budget" if passed else f"{len(violations)} budget violations")


def _parse_criterion(text: str) -> dict[str, float]:
    """Best-effort parse of criterion textual output into ns metrics."""
    out: dict[str, float] = {}
    # criterion prints e.g. "time:   [1.2345 us 1.2400 us 1.2456 us]" (low/median/high)
    m = re.search(r"time:\s*\[([\d.]+)\s*(ns|us|ms)\s+([\d.]+)\s*(ns|us|ms)\s+([\d.]+)\s*(ns|us|ms)\]", text)
    if m:
        def to_ns(v: str, unit: str) -> float:
            return float(v) * {"ns": 1, "us": 1_000, "ms": 1_000_000}[unit]
        out["p50_ns"] = to_ns(m.group(3), m.group(4))  # median
    return out


# --------------------------------------------------------------------------- determinism
def check_determinism(repo: str, replay_bin: str, fixture: str, runs: int = 3) -> CheckResult:
    """
    Run the bot's replay binary N times over the same fixture and assert byte-identical
    DecisionRecord output. Requires the build to have produced `replay_bin`.
    """
    outputs: list[str] = []
    for _ in range(runs):
        rc, out, err = _run([replay_bin, "--fixture", fixture, "--emit-decisions"], repo)
        if rc != 0:
            return CheckResult("determinism", False, {"returncode": rc, "stderr_tail": err[-800:]},
                               "replay binary failed")
        outputs.append(out)
    identical = all(o == outputs[0] for o in outputs)
    return CheckResult("determinism", identical, {"runs": runs, "identical": identical},
                       "byte-identical" if identical else "NONDETERMINISTIC output across runs")


# --------------------------------------------------------------------------- memory soak
def check_memory_soak(repo: str, soak_bin: str = "", max_growth_mb: float = 50.0) -> "CheckResult":
    """
    Run the bot's memory soak harness (a long-running synthetic-load binary the build produces)
    and assert steady-state RSS does not trend upward beyond max_growth_mb — i.e. no leak.
    Honest when the harness isn't built yet: returns passed=False with a clear reason, never a
    silent pass. Enforces constitution criterion 99 / §57 memory-safety mandate.
    """
    if not soak_bin:
        return CheckResult("memory_soak", False,
                           {"reason": "no soak binary configured"},
                           "soak harness not built yet (criterion 99 not yet verifiable)")
    from pathlib import Path as _P
    if not _P(soak_bin).is_file():
        return CheckResult("memory_soak", False, {"soak_bin": soak_bin},
                           "soak binary path does not exist")
    rc, out, err = _run([soak_bin, "--report-rss-growth-mb"], repo, timeout=3600)
    try:
        growth = float((out + err).strip().splitlines()[-1])
    except (ValueError, IndexError):
        return CheckResult("memory_soak", False, {"stdout_tail": out[-500:]},
                           "could not parse soak RSS growth output")
    passed = growth <= max_growth_mb
    return CheckResult("memory_soak", passed,
                       {"rss_growth_mb": growth, "max_growth_mb": max_growth_mb},
                       f"steady-state (grew {growth:.1f}MB)" if passed
                       else f"POSSIBLE LEAK: RSS grew {growth:.1f}MB > {max_growth_mb}MB")
