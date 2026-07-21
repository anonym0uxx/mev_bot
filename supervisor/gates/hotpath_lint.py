"""
Hot-path constitution lint — mechanically enforces the §24 Rust performance-engineering
law and criterion 109's textual bans, so violations fail a gate instead of surviving
until a human reads the diff.

Scope of rules (path-scoped, regex-based, with a counted escape hatch):
  - HOT crates: no async/tokio, no serde_json, no floats (outside *boundary* adapters),
    no syscall clocks, no sleeps, no format!/println!/dbg!, no unwrap/expect/panic!.
  - EVERYWHERE: no libc mlockall / sched_setaffinity (Linux-isms), no /tmp paths.
  - MONEY crates: no `as f32` / `as f64` casts.

Escape hatch: a line containing `LINT-ALLOW(rule_id): <reason>` passes but is counted
and reported — silent exemptions don't exist.
"""
from __future__ import annotations

import glob as _glob
import os
import re
from dataclasses import dataclass, field

from .checks import CheckResult


@dataclass
class LintRule:
    rule_id: str
    pattern: str            # regex applied per line
    message: str
    path_globs: list[str]   # which files this rule applies to (repo-relative)
    exclude_file_substrings: list[str] = field(default_factory=list)

    def compiled(self) -> re.Pattern:
        return re.compile(self.pattern)


# Default crate layout matches repo_scaffold/ (override via GateConfig.lint_* globs).
HOT_GLOBS = ["rust/crates/hot-*/src/**/*.rs"]
MONEY_GLOBS = ["rust/crates/hot-core/src/**/*.rs", "rust/crates/evaluator/src/**/*.rs"]
ALL_RUST = ["rust/**/*.rs"]
ALL_FILES = ["rust/**/*.rs", "rust/**/Cargo.toml"]


def default_rules(hot_globs: list[str] | None = None,
                  money_globs: list[str] | None = None) -> list[LintRule]:
    hot = hot_globs or HOT_GLOBS
    money = money_globs or MONEY_GLOBS
    return [
        # ---- hot-path purity (§24 perf law clause (b); criterion 109)
        LintRule("hot_await", r"\.await\b", "async/await forbidden on hot path", hot),
        LintRule("hot_tokio", r"\btokio::", "tokio forbidden on hot path (control plane only)", hot),
        LintRule("hot_serde_json", r"\bserde_json\b", "serde_json forbidden on hot path", hot),
        LintRule("hot_float", r"\b(f32|f64)\b",
                 "floats forbidden in hot crates (quantize at *boundary* adapters only)",
                 hot, exclude_file_substrings=["boundary"]),
        LintRule("hot_sys_clock", r"\b(SystemTime::now|Instant::now)\b",
                 "syscall clocks forbidden on hot path (calibrated TSC / event time only)", hot),
        LintRule("hot_sleep", r"\bthread::sleep\b", "sleep forbidden on hot path", hot),
        LintRule("hot_alloc_fmt", r"\b(format!|println!|dbg!)\s*\(",
                 "allocating/IO macros forbidden on hot path", hot),
        LintRule("hot_panic", r"(\.unwrap\(\)|\.expect\(|panic!\s*\()",
                 "panic paths forbidden in hot crates (panic-free by construction; panic=abort)", hot),
        # ---- Windows-native (§24 clause (c); criterion 109) — everywhere
        LintRule("linuxism_mlock", r"\bmlockall\b",
                 "libc mlockall is a Linux-ism; use VirtualLock behind OsTune", ALL_RUST),
        LintRule("linuxism_affinity", r"\bsched_setaffinity\b",
                 "sched_setaffinity is a Linux-ism; use SetThreadGroupAffinity behind OsTune", ALL_RUST),
        LintRule("tmp_path", r"/tmp/",
                 "absolute /tmp paths break Windows and clean checkouts (named defect)", ALL_FILES),
        # ---- money integrity (§24 clause (a); §22)
        LintRule("money_float_cast", r"\bas\s+f(32|64)\b",
                 "float casts forbidden in money/evaluator crates", money),
    ]


_ALLOW = re.compile(r"LINT-ALLOW\(([a-z_]+)\)\s*:\s*\S")


def load_repo_rules(repo: str) -> list[LintRule]:
    """Load additional lint rules the repo ships in rust/lint_rules.yaml (optional).

    This lets a new hot-path ban be added as a committed data edit — Hermes can
    propose one, commit it, and it takes effect on the next gate with no code change.
    Schema per rule: {rule_id, pattern, message, path_globs: [...],
    exclude_file_substrings: [...] (optional)}. Malformed files are ignored with no
    crash (the built-in rules always still apply).
    """
    try:
        import yaml  # local import; pyyaml already a supervisor dep
    except ImportError:
        return []
    path = os.path.join(repo, "rust", "lint_rules.yaml")
    if not os.path.isfile(path):
        return []
    try:
        with open(path, encoding="utf-8") as fh:
            data = yaml.safe_load(fh) or {}
        out: list[LintRule] = []
        for r in data.get("rules", []):
            if not all(k in r for k in ("rule_id", "pattern", "message", "path_globs")):
                continue
            re.compile(r["pattern"])  # validate regex; skip on failure
            out.append(LintRule(
                rule_id=str(r["rule_id"]),
                pattern=str(r["pattern"]),
                message=str(r["message"]),
                path_globs=list(r["path_globs"]),
                exclude_file_substrings=list(r.get("exclude_file_substrings", [])),
            ))
        return out
    except (OSError, re.error, ValueError, TypeError, AttributeError,
            yaml.YAMLError):
        return []


def _files_for(repo: str, globs: list[str]) -> set[str]:
    out: set[str] = set()
    for g in globs:
        out.update(_glob.glob(os.path.join(repo, g), recursive=True))
    return {f for f in out if os.path.isfile(f)}


def scan(repo: str, rules: list[LintRule]) -> tuple[list[dict], list[dict]]:
    """Returns (violations, allowed) — each a list of {rule, file, line, text}."""
    violations: list[dict] = []
    allowed: list[dict] = []
    for rule in rules:
        rx = rule.compiled()
        for path in sorted(_files_for(repo, rule.path_globs)):
            rel = os.path.relpath(path, repo)
            if any(sub in os.path.basename(rel) for sub in rule.exclude_file_substrings):
                continue
            try:
                with open(path, encoding="utf-8", errors="replace") as fh:
                    for ln, line in enumerate(fh, 1):
                        if not rx.search(line):
                            continue
                        entry = {"rule": rule.rule_id, "file": rel, "line": ln,
                                 "text": line.strip()[:160]}
                        m = _ALLOW.search(line)
                        if m and m.group(1) == rule.rule_id:
                            allowed.append(entry)
                        else:
                            violations.append(entry)
            except OSError:
                continue
    return violations, allowed


def check_hotpath_lint(repo: str,
                       hot_globs: list[str] | None = None,
                       money_globs: list[str] | None = None) -> CheckResult:
    rules = default_rules(hot_globs, money_globs) + load_repo_rules(repo)
    violations, allowed = scan(repo, rules)
    passed = not violations
    extra = sum(1 for r in load_repo_rules(repo))
    summary = (f"clean ({len(allowed)} explicit LINT-ALLOW exemptions; "
               f"{extra} repo rule(s) merged)" if passed
               else f"{len(violations)} violation(s); first: "
                    f"{violations[0]['rule']} {violations[0]['file']}:{violations[0]['line']}")
    return CheckResult("hotpath_lint", passed,
                       {"violations": violations[:100], "allowed": allowed[:100]},
                       summary=summary)
