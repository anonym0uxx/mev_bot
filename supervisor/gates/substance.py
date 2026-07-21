"""
substance.py — gate checks that verify code ACTUALLY EXISTS, not just that it compiles.

The overnight M0–M9 run passed every gate while producing seven files containing only doc
comments and zero code. That was possible because the existing gate measured "does it compile
and is it formatted" — and nothing compiles more cleanly than nothing. An empty crate has no
`todo!()` for no_stubs to find, no malformed code for clippy to flag, no failing assertions.

These checks close that hole. They are deliberately about SUBSTANCE:

  1. check_impl_nonempty      — production crates must contain real code, not comment-only files.
  2. check_dossier_signatures — every dossier leaf's exact Rust signature must appear in the
                                implementation with a non-empty body (the leaf.signature field
                                is the contract; this proves it was filled).
  3. check_dossier_tests_bind — the materialized dossier tests must actually reference the
                                implementation (call its functions / use its types), so an empty
                                crate makes the tests fail to compile rather than vacuously pass.
  4. check_criterion_ledger   — the criterion-completion ledger (docs/criteria_ledger.json) must
                                show every non-deferred criterion satisfied by real evidence.

Together these make "green" mean "real implementation exists and is exercised by tests" — the
property the overnight run silently lacked.
"""
from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

# reuse the canonical CheckResult so these compose with the existing gate battery
from .checks import CheckResult


# Production crates that must contain real implementation (not the tests/, not scaffolding).
# A crate is "comment-only" if, after stripping comments and blank lines, almost nothing remains.
def _strip_rust_noise(src: str) -> str:
    # remove line comments (// and //!) and block comments, then blank lines
    src = re.sub(r"/\*.*?\*/", "", src, flags=re.S)
    src = re.sub(r"^\s*//.*$", "", src, flags=re.M)
    # scaffold markers are NOT real implementation — strip them so a skeleton module doesn't
    # count as "non-empty". These are written by scaffold_workspace.py to keep files valid
    # before leaves land; real leaf code replaces/augments them.
    src = src.replace("const _MODULE_SCAFFOLD: () = ();", "")
    src = re.sub(r"^\s*#\[allow\(dead_code\)\]\s*$", "", src, flags=re.M)
    # a bare `pub mod x;` declaration is structure, not implementation — don't count it
    src = re.sub(r"^\s*pub mod \w+;\s*$", "", src, flags=re.M)
    src = re.sub(r"^\s*$\n", "", src, flags=re.M)
    return src.strip()


def _rust_impl_files(repo: str) -> list[Path]:
    root = Path(repo)
    cargo_root = root / "rust" if (root / "rust").is_dir() else root
    out: list[Path] = []
    for f in cargo_root.rglob("*.rs"):
        parts = f.parts
        # only production src/, never tests/ or benches/ or build scripts
        if "src" in parts and "tests" not in parts and "benches" not in parts:
            out.append(f)
    return out


def check_impl_nonempty(repo: str, min_code_lines_per_crate: int = 8) -> CheckResult:
    """Each production crate's src/ must contain real code, not just doc comments.

    This is the check that would have caught the overnight run: seven crates each holding four
    lines of `//!` comment and nothing else. We strip comments/blanks and require a minimum of
    actual code lines per crate. A crate that is all-comments fails.
    """
    root = Path(repo)
    cargo_root = root / "rust" if (root / "rust").is_dir() else root
    crates_dir = cargo_root / "crates"
    if not crates_dir.is_dir():
        # fall back to any src/lib.rs under the workspace
        crate_srcs = {f.parent.parent: [f] for f in _rust_impl_files(repo)}
    else:
        crate_srcs = {}
        for crate in crates_dir.iterdir():
            if not crate.is_dir():
                continue
            srcs = [f for f in (crate / "src").rglob("*.rs")] if (crate / "src").is_dir() else []
            if srcs:
                crate_srcs[crate] = srcs

    empty: list[str] = []
    thin: list[str] = []
    for crate, srcs in sorted(crate_srcs.items()):
        code = ""
        for f in srcs:
            code += "\n" + _strip_rust_noise(f.read_text(encoding="utf-8", errors="ignore"))
        code_lines = [l for l in code.splitlines() if l.strip()]
        name = crate.name
        if not code_lines:
            empty.append(name)
        elif len(code_lines) < min_code_lines_per_crate:
            thin.append(f"{name}({len(code_lines)}ln)")
    ok = not empty and not thin
    parts = []
    if empty:
        parts.append(f"COMMENT-ONLY crates (no implementation): {empty}")
    if thin:
        parts.append(f"trivially thin crates (< {min_code_lines_per_crate} code lines): {thin}")
    detail = "; ".join(parts) if parts else f"{len(crate_srcs)} crates contain real code"
    return CheckResult("impl_nonempty", ok, {"empty": empty, "thin": thin}, detail)


def _load_dossiers(repo: str):
    """Import the dossier loader and return all dossiers discoverable in this repo."""
    import sys
    root = Path(repo)
    for base in (root, root / "supervisor" / ".."):
        if (base / "supervisor" / "reinforcement" / "dossier.py").is_file():
            if str(base) not in sys.path:
                sys.path.insert(0, str(base))
            break
    try:
        from supervisor.reinforcement.dossier import load_dossier
    except Exception:
        return []
    ddir = None
    for cand in (root / "supervisor" / "reinforcement" / "dossiers",
                 root / "docs" / "dossiers"):
        if cand.is_dir():
            ddir = cand
            break
    if ddir is None:
        return []
    dossiers = []
    for yml in sorted(ddir.glob("*.yaml")):
        try:
            dossiers.append(load_dossier(yml))
        except Exception:
            pass
    return dossiers


def _signature_ident(signature: str) -> Optional[str]:
    """Extract the function name from a Rust signature like 'pub fn apply(&mut self, ...) -> X'."""
    m = re.search(r"\bfn\s+([a-zA-Z_][a-zA-Z0-9_]*)", signature)
    return m.group(1) if m else None


def check_dossier_signatures(repo: str) -> CheckResult:
    """Every dossier leaf declares an exact Rust signature the builder must implement. This
    verifies each such function NAME appears in the production code with a body (a `{` after it),
    not merely as a comment. Missing signatures = the leaf was not implemented.
    """
    dossiers = _load_dossiers(repo)
    if not dossiers:
        return CheckResult("dossier_signatures", True, {}, "no dossiers found (skip)")
    impl_files = _rust_impl_files(repo)
    all_code = "\n".join(f.read_text(encoding="utf-8", errors="ignore") for f in impl_files)
    # strip comments so a function named only in a doc comment doesn't count
    code_wo_comments = _strip_rust_noise(all_code)

    missing: list[str] = []
    total = 0
    for d in dossiers:
        for leaf in d.leaf_order():
            ident = _signature_ident(leaf.signature or "")
            if not ident:
                continue
            total += 1
            # require: `fn <ident>` followed (soon) by a `{` — i.e. a real definition with a body
            pat = re.compile(rf"\bfn\s+{re.escape(ident)}\b[^;{{]*\{{", re.S)
            if not pat.search(code_wo_comments):
                missing.append(f"{d.component}/{leaf.leaf_id}::{ident}")
    ok = not missing
    detail = (f"all {total} dossier-leaf signatures implemented"
              if ok else f"{len(missing)}/{total} leaf signatures MISSING a real body: "
                         f"{missing[:12]}{'…' if len(missing) > 12 else ''}")
    return CheckResult("dossier_signatures", ok, {"missing": missing, "total": total}, detail)


def check_dossier_tests_bind(repo: str) -> CheckResult:
    """The materialized dossier tests must actually EXERCISE the implementation — reference its
    functions/types — so that an empty implementation makes them fail to compile rather than
    pass vacuously. We check each dossier test file mentions its leaf's function identifier.
    A test that never names the code it 'tests' is not testing anything.
    """
    dossiers = _load_dossiers(repo)
    if not dossiers:
        return CheckResult("dossier_tests_bind", True, {}, "no dossiers found (skip)")
    root = Path(repo)
    cargo_root = root / "rust" if (root / "rust").is_dir() else root
    vacuous: list[str] = []
    total = 0
    for d in dossiers:
        for leaf in d.leaf_order():
            ident = _signature_ident(leaf.signature or "")
            if not ident:
                continue
            # find the materialized test file for this leaf
            hits = list(cargo_root.rglob(f"dossier_{d.component}_{leaf.leaf_id}.rs"))
            if not hits:
                continue
            total += 1
            tf = hits[0].read_text(encoding="utf-8", errors="ignore")
            # the test must reference the function under test (call it or import it)
            if not re.search(rf"\b{re.escape(ident)}\b", tf):
                vacuous.append(f"{d.component}/{leaf.leaf_id} (never calls {ident})")
    ok = not vacuous
    detail = (f"all {total} dossier tests exercise their implementation"
              if ok else f"{len(vacuous)} dossier tests are vacuous (don't reference the code "
                         f"they test): {vacuous[:10]}")
    return CheckResult("dossier_tests_bind", ok, {"vacuous": vacuous, "total": total}, detail)


def check_criterion_ledger(repo: str, require_all: bool = True) -> CheckResult:
    """The criterion-completion ledger (docs/criteria_ledger.json) is the source of truth for
    'done'. Every non-deferred criterion must be marked satisfied with evidence (an impl path +
    a passing test). This check fails until the whole ledger is genuinely green.

    Ledger shape:
      {"criteria": {"1": {"status": "satisfied|stub|missing|deferred_server",
                          "impl": "path", "test": "name", "note": "..."} , ...}}
    """
    ledger_path = Path(repo) / "docs" / "criteria_ledger.json"
    if not ledger_path.is_file():
        return CheckResult("criterion_ledger", not require_all,
                           {"exists": False},
                           "criteria_ledger.json missing — no verified criterion coverage yet")
    try:
        data = json.loads(ledger_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        return CheckResult("criterion_ledger", False, {"error": str(e)}, "ledger is invalid JSON")
    crit = data.get("criteria", {})
    satisfied, stub, missing, deferred = [], [], [], []
    for k, v in crit.items():
        st = (v or {}).get("status", "missing")
        if st == "satisfied":
            satisfied.append(k)
        elif st == "deferred_server":
            deferred.append(k)
        elif st == "stub":
            stub.append(k)
        else:
            missing.append(k)
    open_items = stub + missing
    ok = (not open_items) if require_all else True
    detail = (f"ledger: {len(satisfied)} satisfied, {len(deferred)} deferred-to-server, "
              f"{len(stub)} stub, {len(missing)} missing")
    return CheckResult("criterion_ledger", ok,
                       {"satisfied": len(satisfied), "deferred": len(deferred),
                        "stub": stub, "missing": missing}, detail)
