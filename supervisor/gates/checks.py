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
import os
import pathlib
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
    # Windows: resolve the real binary (cargo.exe etc) and use the shell for .CMD/.BAT wrappers,
    # so tool launches behave the same as on Linux. Bare-name launches otherwise fail on Windows.
    run_cmd = list(cmd)
    resolved = shutil.which(run_cmd[0])
    if resolved:
        run_cmd[0] = resolved
    try:
        if os.name == "nt":
            line = subprocess.list2cmdline(run_cmd)
            p = subprocess.run(line, cwd=cwd, capture_output=True, text=True,
                               encoding="utf-8", errors="replace", timeout=timeout, shell=True)
        else:
            p = subprocess.run(run_cmd, cwd=cwd, capture_output=True, text=True,
                               encoding="utf-8", errors="replace", timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except FileNotFoundError:
        return 127, "", f"tool not found: {cmd[0]}"
    except subprocess.TimeoutExpired:
        return 124, "", f"timeout after {timeout}s"


def _is_deployment_host(repo: str) -> bool:
    """Phase B (deployment server) vs Phase A (portable authoring machine), per §9.5.
    Release-profile builds are Phase-B-only; Phase-A gates use the portable/dev profile."""
    try:
        from .build_phase import check_phase_provenance
        # a Phase-B-exclusive probe: if the live machine is the pinned deployment host, we're B
        manifest = str(pathlib.Path(repo) / "infra_manifest.json")
        r = check_phase_provenance("bench", [103], manifest)
        return r.phase == "B"
    except Exception:  # noqa: BLE001
        return False


def _cargo_profile_args(repo: str) -> list[str]:
    """[] on a Phase-A machine (portable/dev profile) or ['--release'] on the deployment host.
    The release profile carries deploy-CPU codegen (§9.5/criterion 109) and is meaningful only
    on the server; forcing it on a laptop both violates the two-phase boundary and can fail on
    settings the portable machine can't satisfy."""
    return ["--release"] if _is_deployment_host(repo) else []


def _have(tool: str) -> bool:
    return shutil.which(tool) is not None


def _fmt_per_crate_check(repo: str, cargo_dir: str) -> CheckResult:
    """Per-crate ``cargo fmt -p <pkg> --check`` fallback for Windows MAX_PATH.

    Enumerates workspace members from the workspace Cargo.toml, resolves each
    member's package name, and runs ``cargo fmt -p <pkg> --check`` individually.
    All read-only — never mutates the tree.  Any crate with a diff fails the
    check.
    """
    import pathlib as _p
    _Path = _p.Path
    manifest = (_Path(cargo_dir) / "Cargo.toml").read_text(encoding="utf-8")
    member_paths: list[str] = []
    m = re.search(r"members\s*=\s*\[(.*?)\]", manifest, re.DOTALL)
    if m:
        for item in re.findall(r'"([^"]+)"', m.group(1)):
            if "*" in item:
                base = _Path(cargo_dir) / item.replace("/*", "")
                if base.is_dir():
                    for sub in sorted(base.iterdir()):
                        if (sub / "Cargo.toml").is_file():
                            member_paths.append(str(sub))
            else:
                member_paths.append(item)
    pkg_names: list[str] = []
    for mp in member_paths:
        ct = _Path(cargo_dir) / mp / "Cargo.toml"
        if ct.is_file():
            txt = ct.read_text(encoding="utf-8")
            pm = re.search(r'name\s*=\s*"([^"]+)"', txt)
            if pm:
                pkg_names.append(pm.group(1))
    diffs: list[str] = []
    for crate in pkg_names:
        rc, out, err = _run(["cargo", "fmt", "-p", crate, "--check"], cargo_dir, timeout=120)
        if rc != 0:
            diffs.append(crate)
    if not diffs:
        return CheckResult("fmt", True, {"method": "per-crate", "packages": len(pkg_names)},
                           "formatted (clean, per-crate)")
    return CheckResult("fmt", False,
                       {"method": "per-crate", "diff_crates": diffs,
                        "stderr_tail": err[-2000:] if diffs else ""},
                       f"fmt check failed: {len(diffs)} crate(s) with diff: {diffs[:10]}")


def _cargo_dir(repo: str) -> str:
    """Directory to run cargo in = the one containing the workspace Cargo.toml.

    The assembled repo places the Rust workspace under rust/ (rust/Cargo.toml), but some
    layouts put it at the repo root. Prefer rust/ if it has a Cargo.toml, else the root,
    else fall back to rust/ so the error message names the expected location.
    """
    r = pathlib.Path(repo)
    if (r / "rust" / "Cargo.toml").is_file():
        return str(r / "rust")
    if (r / "Cargo.toml").is_file():
        return str(r)
    return str(r / "rust")


# --------------------------------------------------------------------------- build
def check_build(repo: str, target: Optional[str] = None) -> CheckResult:
    if not _have("cargo"):
        return CheckResult("build", False, summary="cargo not found on PATH")
    cmd = ["cargo", "build"] + _cargo_profile_args(repo)
    if target and _cargo_profile_args(repo):
        cmd += ["--target", target]   # target pinning only with the release profile (Phase B)
    rc, out, err = _run(cmd, _cargo_dir(repo))
    return CheckResult("build", rc == 0, {"returncode": rc, "stderr_tail": err[-2000:]},
                       "compiled" if rc == 0 else "build failed")


# --------------------------------------------------------------------------- clippy / fmt / hygiene
def check_clippy(repo: str) -> CheckResult:
    if not _have("cargo"):
        return CheckResult("clippy", False, summary="cargo not found")
    rc, out, err = _run(["cargo", "clippy"] + _cargo_profile_args(repo) + ["--", "-D", "warnings"], _cargo_dir(repo))
    return CheckResult("clippy", rc == 0, {"returncode": rc, "stderr_tail": err[-2000:]},
                       "clean" if rc == 0 else "clippy warnings/errors")


def check_fmt(repo: str) -> CheckResult:
    """Verify the tree is formatted — WITHOUT mutating it.

    Runs ``cargo fmt --all -- --check`` (read-only).  On Windows, the
    ``--all`` flag can hit OS error 206 (ERROR_FILENAME_EXCED_RANGE) when
    workspace paths exceed MAX_PATH; in that case we fall back to per-crate
    ``cargo fmt -p <pkg> --check`` (also read-only).  A missing cargo is a
    FAIL (the docstring at the top of this module says: never a silent pass).
    """
    if not _have("cargo"):
        return CheckResult("fmt", False, {"reason": "cargo not found"}, "cargo not found; fmt skipped")
    cargo_dir = _cargo_dir(repo)
    rc, out, err = _run(["cargo", "fmt", "--all", "--", "--check"], cargo_dir)
    if rc == 0:
        return CheckResult("fmt", True, {"returncode": rc}, "formatted (clean)")
    # Windows: cargo fmt --all -- --check hits OS error 206 when workspace
    # paths exceed MAX_PATH.  Fall back to per-crate --check (non-mutating).
    combined = (out + err).lower()
    if os.name == "nt" and ("error 206" in combined or "too long" in combined or "filename" in combined):
        return _fmt_per_crate_check(repo, cargo_dir)
    return CheckResult("fmt", False, {"returncode": rc, "stderr_tail": err[-2000:]},
                       "fmt check failed (diff present)")



def check_dossier_test_integrity(repo) -> "CheckResult":
    """The builder must not alter a materialized dossier property test to make it pass.

    Runs `scripts/materialize_tests.py --verify`, which re-hashes every materialized test
    against what its dossier renders. Any edit, deletion, or drift fails this check — the
    correctness authority is protected mechanically, not by trust.

    When the materializer script is absent (no dossier tests materialized in this repo
    layout), the check is a SKIP reported honestly as NOT-PASSED, so the gate battery
    does not silently certify a property it could not verify.
    """
    import subprocess, sys
    from pathlib import Path as _P
    script = _P(repo) / "scripts" / "materialize_tests.py"
    if not script.is_file():
        return CheckResult("dossier_test_integrity", False,
                           {"reason": "materializer not present"},
                           "materializer not present (skip — not certified)")
    try:
        p = subprocess.run([sys.executable, str(script), "--repo", str(repo), "--verify"],
                           capture_output=True, text=True, timeout=120)
    except Exception as e:  # noqa: BLE001
        return CheckResult("dossier_test_integrity", False, {"error": str(e)},
                           f"verify error: {e}")
    ok = p.returncode == 0
    detail = (p.stdout.strip().splitlines()[-1] if p.stdout.strip() else "") or p.stderr[:200]
    return CheckResult("dossier_test_integrity", ok, {"returncode": p.returncode}, detail)


# Genuine Rust stub markers in PRODUCTION source (test modules are stripped before matching).
# Deliberately does NOT match the bare word "TODO" in comments (e.g. the "SERVER (Phase-B) TODO"
# markers that legitimately point at docs/SERVER_BUILD_MANIFEST.md) — only the stub MACROS and
# explicit not-implemented panics count as stubs.
_STUB_PATTERNS = [
    re.compile(r"\btodo!\s*\("),
    re.compile(r"\bunimplemented!\s*\("),
    re.compile(r"""\bpanic!\s*\(\s*[br]?["'][^"']*(?:not\s+impl|unimpl|stub|not\s+yet)""", re.I),
]


def check_no_stubs(repo: str, production_globs: list[str]) -> CheckResult:
    hits: list[str] = []
    matched_files = 0
    root = Path(repo)
    for g in production_globs:
        for f in root.glob(g):
            if f.suffix != ".rs":
                continue
            matched_files += 1
            text = f.read_text(encoding="utf-8", errors="ignore")
            # allow markers in #[cfg(test)] blocks only: crude but effective — strip test modules
            text_wo_tests = re.sub(r"#\[cfg\(test\)\][\s\S]*?\n}\n", "", text)
            for rx in _STUB_PATTERNS:
                if rx.search(text_wo_tests):
                    hits.append(f"{f.relative_to(root)}: {rx.pattern}")
    # Empty-set guard: a typo'd glob matching zero files would silently pass. Fail closed
    # and report the matched count so the assertion ("these N files have no stubs") is
    # explicit, not an unexamined empty set.
    if matched_files == 0:
        return CheckResult("no_stubs", False,
                           {"hits": [], "matched_files": 0, "globs": production_globs},
                           f"EMPTY-SET: production_globs matched 0 .rs files — glob may be typo'd")
    if hits:
        return CheckResult("no_stubs", False,
                           {"hits": hits, "matched_files": matched_files},
                           f"{len(hits)} stub/TODO in production paths ({matched_files} files scanned)")
    return CheckResult("no_stubs", True,
                       {"hits": [], "matched_files": matched_files},
                       f"no stubs ({matched_files} files scanned)")


# --------------------------------------------------------------------------- tests
def check_tests(repo: str, required_test_names: Optional[list[str]] = None,
                single_test: Optional[str] = None) -> CheckResult:
    """Run cargo test. If single_test is given, run ONLY that integration test target
    (`cargo test --test <name>`), so that other leaves' not-yet-implemented tests — which
    reference types their leaf will define later — don't break compilation of the whole test
    suite. This is what makes per-leaf building possible: leaf N's test compiles and runs on its
    own, independent of leaves N+1..end whose tests won't compile until those leaves are built.
    """
    if not _have("cargo"):
        return CheckResult("test", False, summary="cargo not found")
    if single_test:
        cmd = ["cargo", "test"] + _cargo_profile_args(repo) + ["--test", single_test, "--", "--nocapture"]
    else:
        cmd = ["cargo", "test"] + _cargo_profile_args(repo) + ["--", "--nocapture"]
    rc, out, err = _run(cmd, _cargo_dir(repo), timeout=3600)
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
    # An assignment of a long opaque value to a key/token/secret-named field — the shape of a
    # leaked credential. The value must contain BOTH a letter and a digit (real keys/UUIDs do;
    # pure-number config like `reserve_token = 191_548_874` does not) to avoid numeric false
    # positives. Public on-chain addresses/hashes don't match (not assigned to a secret field).
    re.compile(r"(api[_-]?key|secret[_-]?key|private[_-]?key|access[_-]?token|auth[_-]?token|"
               r"passphrase|mnemonic|seed[_-]?phrase)"
               r"\s*[:=]\s*[\"']?(?=[0-9A-Za-z/+_\-]*[A-Za-z])(?=[0-9A-Za-z/+_\-]*[0-9])"
               r"[0-9A-Za-z/+_\-]{16,}", re.I),
    # A base58 Solana *secret key* is 87-88 chars; a PUBLIC key/address is 32-44 chars. Only the
    # secret-key length window is flagged, in secret-ish context, to avoid public-address noise.
    re.compile(r"(secret|priv|keypair|wallet)[^\n]{0,40}[1-9A-HJ-NP-Za-km-z]{85,90}", re.I),
    # PEM private key blocks — unambiguous.
    re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
]

# Paths excluded from secret scanning: quarantined legacy (not shipped, full of public on-chain
# IDs and lock-file hashes), generated manifests/lock files, and binary/vendored artifacts.
_SECRET_SCAN_EXCLUDE_DIRS = ("legacy/", "target/", "node_modules/", ".git/")
_SECRET_SCAN_EXCLUDE_FILES = (
    ".hermes_dossier_tests.json", "Cargo.lock", "package-lock.json", "poetry.lock",
    "infra_manifest.json", "infra_manifest.example.json",
)
_SECRET_SCAN_EXCLUDE_SUFFIXES = (".png", ".jpg", ".jpeg", ".gz", ".zip", ".bin", ".lock",
                                 ".wasm", ".so", ".dll", ".exe")


_SECRET_ALLOWLIST_FILE = ".hermes_secret_allowlist.txt"


def _load_secret_allowlist(repo: str) -> set[str]:
    """Files the operator has knowingly chosen to keep secrets in. Listed one repo-relative
    path per line in .hermes_secret_allowlist.txt (blank lines and #comments ignored). These
    files are skipped by the secret scan; every OTHER file is still scanned, so a NEW key
    leaked into an un-allowlisted file (e.g. by the builder) is still caught. This is an
    explicit, auditable 'I accept these specific secrets' — not a blanket disable."""
    allow: set[str] = set()
    p = Path(repo) / _SECRET_ALLOWLIST_FILE
    if not p.is_file():
        return allow
    try:
        for line in p.read_text(encoding="utf-8", errors="ignore").splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            allow.add(line.replace("\\", "/"))
    except Exception:  # noqa: BLE001
        pass
    return allow


def check_secrets(repo: str) -> CheckResult:
    """Scan tracked, in-scope files for real leaked secrets.

    Quarantined legacy code, generated lock/hash manifests, and binaries are excluded (public
    on-chain IDs / content hashes, not secrets). Files listed in .hermes_secret_allowlist.txt
    are skipped — the operator has explicitly chosen to keep secrets there. Everything else is
    scanned strictly, so a NEW key leaked into a non-allowlisted file still fails the gate
    (Tier-0 protection for future leaks stays intact).
    """
    allow = _load_secret_allowlist(repo)
    if _have("gitleaks"):
        # gitleaks reads its own .gitleaksignore; we still honor our allowlist by post-filtering
        rc, out, err = _run(["gitleaks", "detect", "--no-banner", "--redact"], repo)
        # if the operator has an allowlist, don't hard-fail on gitleaks alone — fall through to
        # the regex scan which respects the allowlist, so behavior is consistent either way.
        if not allow:
            return CheckResult("secrets", rc == 0,
                               {"tool": "gitleaks", "returncode": rc, "out_tail": out[-1000:]},
                               "no leaks" if rc == 0 else "gitleaks findings")
    # regex scan (allowlist-aware)
    rc, out, err = _run(["git", "ls-files"], repo)
    hits: list[str] = []
    skipped = 0
    root = Path(repo)
    for rel in out.splitlines():
        rel = rel.strip()
        if not rel:
            continue
        relz = rel.replace("\\", "/")
        # operator-allowlisted files: knowingly kept secrets, skip
        if relz in allow:
            skipped += 1
            continue
        # skip quarantined/generated/vendored paths and known non-secret manifests
        if any(relz.startswith(d) or f"/{d}" in relz for d in _SECRET_SCAN_EXCLUDE_DIRS):
            continue
        base = relz.rsplit("/", 1)[-1]
        if base in _SECRET_SCAN_EXCLUDE_FILES:
            continue
        f = root / rel
        if not f.is_file() or f.suffix in _SECRET_SCAN_EXCLUDE_SUFFIXES:
            continue
        try:
            text = f.read_text(encoding="utf-8", errors="ignore")
        except Exception:
            continue
        if "PLACEHOLDER" in text or "EXAMPLE" in text:
            text = re.sub(r".*(PLACEHOLDER|EXAMPLE).*", "", text)
        for rx in _SECRET_PATTERNS:
            if rx.search(text):
                hits.append(f"{rel}: {rx.pattern[:40]}")
                break
    detail_summary = ("no secrets" if not hits else f"{len(hits)} possible secrets")
    if skipped:
        detail_summary += f" ({skipped} allowlisted file(s) skipped)"
    return CheckResult("secrets", len(hits) == 0,
                       {"tool": "regex", "hits": hits, "allowlisted_skipped": skipped},
                       detail_summary)


# --------------------------------------------------------------------------- benchmarks
def check_bench(repo: str, bench_name: str, budgets_ns: dict[str, float]) -> CheckResult:
    """
    Run a criterion.rs benchmark and parse p50/p99/p999 from its JSON output.
    budgets_ns: {'p50_ns': ..., 'p99_ns': ..., 'p999_ns': ...}; a metric over budget fails the gate.
    Requires the bot's benches to emit machine-readable output (criterion --output-format or a custom harness).
    """
    if not _have("cargo"):
        return CheckResult("bench", False, summary="cargo not found")
    rc, out, err = _run(["cargo", "bench", "--bench", bench_name], _cargo_dir(repo), timeout=3600)
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
