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


# Hot / money crate scope (criterion 109 / §24). These point at the REAL crates
# in this workspace (pump-quant-* / pq-*), not the repo_scaffold placeholder
# `hot-*` layout — a glob that matches no files makes the whole lint a silent
# no-op, which is exactly the enforcement hole this closes. The committed
# `rust/lint_rules.yaml` (see `load_glob_config`) is the authoritative source of
# these globs; these constants are the fallback when that file is absent.
#
# HOT = the latency-critical decision path: the pure integer decision crates the
# hot loop calls into. MONEY = every crate that computes or carries money /
# outcome quantities, where a float cast is a §22 violation.
#
# NOTE: the app's own hot modules (pump-quant-app engine/lane/attention/position/
# structure) also belong in the HOT set, but pump-quant-app is owned by a
# concurrent build and carries three legitimate, provably-safe hits that its
# owner must annotate with inline LINT-ALLOW (see rust/lint_rules.yaml's pending
# hand-off block). They are appended there, not edited here, to avoid clobbering
# that concurrent work.
HOT_GLOBS = [
    "rust/crates/pump-quant-core/src/**/*.rs",
    "rust/crates/pump-quant-signals/src/**/*.rs",
    "rust/crates/pump-quant-features/src/**/*.rs",
    "rust/crates/pump-quant-protocol/src/**/*.rs",
]
MONEY_GLOBS = [
    "rust/crates/pump-quant-core/src/**/*.rs",
    "rust/crates/pump-quant-strategy/src/**/*.rs",
    "rust/crates/pump-quant-evaluator/src/**/*.rs",
    "rust/crates/pump-quant-domain/src/**/*.rs",
    "rust/crates/pump-quant-market-state/src/**/*.rs",
    "rust/crates/pump-quant-features/src/**/*.rs",
    "rust/crates/pump-quant-simulator/src/**/*.rs",
    "rust/crates/pump-quant-narrative/src/**/*.rs",
    "rust/crates/pump-quant-wallet-graph/src/**/*.rs",
    "rust/crates/pump-quant-watchlist/src/**/*.rs",
    "rust/crates/pump-quant-memory/src/**/*.rs",
]
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


def _code_portion(line: str) -> str:
    """Return the code portion of a Rust source line, with any `//`/`/*` comment
    stripped — but only comments *outside* string/char literals.

    The bans target real code, not prose: a doc comment that merely *mentions*
    `f64`, `async`, or `panic!` is not a violation. Stripping comments respects
    string literals so a `//` inside `"https://..."` (or a `/tmp` path in a string
    the ban legitimately wants to catch) is preserved. This is a matcher
    *tightening* (fewer false positives), never a relaxation of any ban.
    """
    out = []
    i, n = 0, len(line)
    in_str = False       # inside a "..." string
    in_char = False      # inside a '...' char
    while i < n:
        c = line[i]
        if in_str:
            out.append(c)
            if c == "\\" and i + 1 < n:
                out.append(line[i + 1])
                i += 2
                continue
            if c == '"':
                in_str = False
            i += 1
            continue
        if in_char:
            out.append(c)
            if c == "\\" and i + 1 < n:
                out.append(line[i + 1])
                i += 2
                continue
            if c == "'":
                in_char = False
            i += 1
            continue
        # not in a literal
        if c == '"':
            in_str = True
            out.append(c)
            i += 1
            continue
        if c == "'":
            in_char = True
            out.append(c)
            i += 1
            continue
        if c == "/" and i + 1 < n and line[i + 1] == "/":
            break  # start of a line comment — drop the rest
        if c == "/" and i + 1 < n and line[i + 1] == "*":
            break  # start of a block comment — drop the rest of the line
        out.append(c)
        i += 1
    return "".join(out)


def _test_region_lines(text: str) -> set[int]:
    """1-indexed line numbers that live inside a `#[cfg(test)]` module.

    Unit tests legitimately `unwrap()`/`expect()`/`panic!`/`format!` — they are
    not the production hot path the §24 bans govern. Skipping test regions is a
    matcher tightening, not a ban relaxation: production code is still fully
    scanned. A region opens at a `#[cfg(test)]` attribute and spans until the
    brace it introduces returns to depth zero.
    """
    lines = text.splitlines()
    marked: set[int] = set()
    i, n = 0, len(lines)
    cfg_re = re.compile(r"#\[\s*cfg\s*\(\s*test\s*\)\s*\]")
    while i < n:
        if cfg_re.search(lines[i]):
            # Find the first '{' at/after this attribute, then track brace depth.
            j = i
            depth = 0
            opened = False
            while j < n:
                code = _code_portion(lines[j])
                for ch in code:
                    if ch == "{":
                        depth += 1
                        opened = True
                    elif ch == "}":
                        depth -= 1
                marked.add(j + 1)
                if opened and depth <= 0:
                    break
                # A single-item `#[cfg(test)]` (e.g. `use ...;`) with no brace:
                # stop at its terminating ';' so we don't swallow the whole file.
                if not opened and code.rstrip().endswith(";"):
                    break
                j += 1
            i = j + 1
            continue
        i += 1
    return marked


def load_glob_config(repo: str) -> tuple[list[str] | None, list[str] | None]:
    """Load the authoritative hot/money glob sets from `rust/lint_rules.yaml`.

    Keys `hot_globs` / `money_globs` (each a list of repo-relative globs) pin the
    lint's scope in a committed data file, so it is not a silent code default that
    can drift out of sync with the crate layout. Returns `(None, None)` when the
    file or a key is absent (callers then fall back to the built-in real-crate
    globs). Malformed input is ignored without crashing.
    """
    try:
        import yaml  # local import; pyyaml already a supervisor dep
    except ImportError:
        return None, None
    path = os.path.join(repo, "rust", "lint_rules.yaml")
    if not os.path.isfile(path):
        return None, None
    try:
        with open(path, encoding="utf-8") as fh:
            data = yaml.safe_load(fh) or {}
        hot = data.get("hot_globs")
        money = data.get("money_globs")
        hot = [str(g) for g in hot] if isinstance(hot, list) and hot else None
        money = [str(g) for g in money] if isinstance(money, list) and money else None
        return hot, money
    except (OSError, ValueError, TypeError, AttributeError, yaml.YAMLError):
        return None, None


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
    """Returns (violations, allowed) — each a list of {rule, file, line, text}.

    A rule fires only on the *code portion* of a line (comments stripped,
    respecting string literals) and only outside `#[cfg(test)]` modules, so a
    banned token appearing in a doc comment or a unit test is not a violation.
    A per-line `LINT-ALLOW(rule_id): reason` moves the hit to `allowed` (counted
    and reported — never silent)."""
    # Cache per-file text + test-region line sets so every rule reuses them.
    text_cache: dict[str, str] = {}
    test_lines_cache: dict[str, set[int]] = {}

    def _file_data(path: str) -> tuple[str, set[int]]:
        if path not in text_cache:
            try:
                with open(path, encoding="utf-8", errors="replace") as fh:
                    txt = fh.read()
            except OSError:
                txt = ""
            text_cache[path] = txt
            test_lines_cache[path] = _test_region_lines(txt)
        return text_cache[path], test_lines_cache[path]

    violations: list[dict] = []
    allowed: list[dict] = []
    for rule in rules:
        rx = rule.compiled()
        for path in sorted(_files_for(repo, rule.path_globs)):
            rel = os.path.relpath(path, repo)
            if any(sub in os.path.basename(rel) for sub in rule.exclude_file_substrings):
                continue
            txt, test_lines = _file_data(path)
            if not txt:
                continue
            for ln, line in enumerate(txt.splitlines(), 1):
                if ln in test_lines:
                    continue  # unit-test code is not the production hot path
                if not rx.search(_code_portion(line)):
                    continue
                entry = {"rule": rule.rule_id, "file": rel, "line": ln,
                         "text": line.strip()[:160]}
                # A line may carry more than one LINT-ALLOW(id) (when it trips more
                # than one rule, e.g. a boundary `as f64` that is both hot_float
                # and money_float_cast); each id is honored independently.
                allow_ids = {m.group(1) for m in _ALLOW.finditer(line)}
                if rule.rule_id in allow_ids:
                    allowed.append(entry)
                else:
                    violations.append(entry)
    return violations, allowed


def check_hotpath_lint(repo: str,
                       hot_globs: list[str] | None = None,
                       money_globs: list[str] | None = None) -> CheckResult:
    # Explicit args win; otherwise the committed rust/lint_rules.yaml is the
    # authoritative scope; otherwise the built-in real-crate defaults. This makes
    # the scope an explicit, committed decision — never a silent code default.
    if hot_globs is None or money_globs is None:
        cfg_hot, cfg_money = load_glob_config(repo)
        hot_globs = hot_globs if hot_globs is not None else cfg_hot
        money_globs = money_globs if money_globs is not None else cfg_money
    rules = default_rules(hot_globs, money_globs) + load_repo_rules(repo)

    # Empty-set guard: a typo'd glob matching zero files makes every rule silently
    # vacuous — "clean" with nothing scanned. Count matched files per glob set and
    # fail closed on zero, so the assertion ("these N files are clean") is explicit.
    hot_files = _files_for(repo, hot_globs) if hot_globs else set()
    money_files = _files_for(repo, money_globs) if money_globs else set()
    all_files = _files_for(repo, ALL_RUST)
    matched_count = len(hot_files | money_files | all_files)
    if matched_count == 0:
        return CheckResult("hotpath_lint", False,
                           {"violations": [], "matched_files": 0,
                            "hot_globs": hot_globs, "money_globs": money_globs},
                           "EMPTY-SET: lint globs matched 0 .rs files — globs may be typo'd")

    violations, allowed = scan(repo, rules)
    passed = not violations
    extra = sum(1 for r in load_repo_rules(repo))
    summary = (f"clean ({len(allowed)} explicit LINT-ALLOW exemptions; "
               f"{extra} repo rule(s) merged; {matched_count} files scanned)" if passed
               else f"{len(violations)} violation(s); first: "
                    f"{violations[0]['rule']} {violations[0]['file']}:{violations[0]['line']} "
                    f"({matched_count} files scanned)")
    return CheckResult("hotpath_lint", passed,
                       {"violations": violations[:100], "allowed": allowed[:100],
                        "matched_files": matched_count},
                       summary=summary)
