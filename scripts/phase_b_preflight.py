#!/usr/bin/env python3
"""Phase-B preflight — the mechanical form of `docs/PHASE_B_PREFLIGHT.md` §1.

NOT TO BE CONFUSED WITH `scripts/preflight.py`, WHICH IS A DIFFERENT CHECK
------------------------------------------------------------------------
`scripts/preflight.py` verifies the **environment**: git present, `claude` on PATH
and authenticated, a rust toolchain, the supervisor package importable, the infra
manifest self-consistent, the constitution where it is expected. Run it once per
machine. It answers "can this box build at all?"

This script verifies the **tree**: that the checkout, the pinned decision vector, the
lint scope and every gate are in the state the Phase-B documents describe. Run it
before every work item. It answers "is what I am about to change the thing the
documents describe?"

Run BOTH. They do not overlap, and neither substitutes for the other.

WHY THIS EXISTS
---------------
The preflight was a markdown table of ten commands. A table has to be *read, obeyed,
and honestly reported on* — three places a weaker model can drift without knowing it
drifted. The most likely drift is not skipping a row; it is running the rows, getting
a failure, and reasoning that the failure is unrelated to the task at hand.

This script removes the interpretation. It runs every row, prints a verdict per row,
and exits non-zero if any BLOCKING row failed. Paste its output; do not summarise it.

    python scripts/phase_b_preflight.py                 # full, from the repo root
    python scripts/phase_b_preflight.py --fast          # skip the slow cargo rows
    python scripts/phase_b_preflight.py --json          # machine-readable

WHAT IT DOES NOT DO
-------------------
It does not judge whether the work you are about to do is correct. It establishes that
the tree you are about to change is the tree the documents describe. Everything in
`docs/PHASE_B_PREFLIGHT.md` §2 (STOP AND ASK) remains yours to obey, because those are
decisions you are about to take, and no script can see them coming.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

# The decision vector. Source of truth is the code; this list is what the preflight
# re-derives it from, so the script can never carry its own stale copy.
PINS = [
    ("GOLDEN_DIGEST", "rust/crates/pq-regression/src/baselines.rs"),
    ("GOLDEN_NET_LAMPORTS", "rust/crates/pq-regression/src/baselines.rs"),
    ("GOLDEN_PROMOTED", "rust/crates/pq-regression/src/baselines.rs"),
    ("GOLDEN_ADMITTED", "rust/crates/pq-regression/src/baselines.rs"),
    ("GOLDEN_REJECTED", "rust/crates/pq-regression/src/baselines.rs"),
    ("GOLDEN_UNIVERSE_FILTERED", "rust/crates/pq-regression/src/baselines.rs"),
    ("GOLDEN_ALPHACALL_NET", "rust/crates/pq-regression/src/baselines.rs"),
]


@dataclass
class Row:
    n: int
    name: str
    blocking: bool = True
    passed: bool | None = None
    detail: str = ""
    remedy: str = ""
    slow: bool = False
    extra: dict = field(default_factory=dict)


def run(cmd: list[str], cwd: Path, timeout: int = 1800) -> tuple[int, str]:
    try:
        p = subprocess.run(
            cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout
        )
        return p.returncode, (p.stdout + p.stderr)
    except FileNotFoundError:
        return 127, f"command not found: {cmd[0]}"
    except subprocess.TimeoutExpired:
        return 124, f"timed out after {timeout}s: {' '.join(cmd)}"


def tail(s: str, n: int = 12) -> str:
    lines = [ln for ln in s.strip().splitlines() if ln.strip()]
    return "\n      ".join(lines[-n:]) if lines else "(no output)"


def _fmt_per_crate(rust: Path) -> tuple[int, str]:
    """Per-crate fmt --check fallback for Windows (OS error 206 on --all).

    Enumerates workspace members from Cargo.toml, runs `cargo fmt -p <crate>
    --check` on each, and returns a combined rc + output. Any single crate
    failing fmt makes the whole row fail.
    """
    import re

    manifest = (rust / "Cargo.toml").read_text(encoding="utf-8")
    members: list[str] = []
    # Parse workspace.members — supports "path/*" glob patterns.
    m = re.search(r"members\s*=\s*\[(.*?)\]", manifest, re.DOTALL)
    if m:
        raw = m.group(1)
        for item in re.findall(r'"([^"]+)"', raw):
            if "*" in item:
                base = rust / item.replace("/*", "")
                if base.is_dir():
                    for sub in base.iterdir():
                        if (sub / "Cargo.toml").is_file():
                            name = sub.name
                            members.append(name)
            else:
                members.append(item)

    fails: list[str] = []
    for crate in members:
        rc, out = run(["cargo", "fmt", "-p", crate, "--check"], rust)
        if rc != 0:
            fails.append(f"{crate}: {tail(out, 3)}")
    if fails:
        return 1, "fmt failures:\n      " + "\n      ".join(fails)
    return 0, f"all {len(members)} crates fmt-clean (per-crate check)"


def read_pins(repo: Path) -> dict[str, int]:
    """Re-derive the decision vector FROM THE CODE, never from a copy in this file."""
    import re

    src = (repo / "rust/crates/pq-regression/src/baselines.rs").read_text(
        encoding="utf-8"
    )
    out: dict[str, int] = {}
    for name, _ in PINS:
        m = re.search(rf"pub const {name}\s*:\s*\w+\s*=\s*(-?[\d_]+)", src)
        if m:
            out[name] = int(m.group(1).replace("_", ""))
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", default=".", help="repository root")
    ap.add_argument("--fast", action="store_true", help="skip slow cargo rows")
    ap.add_argument("--json", action="store_true", help="machine-readable output")
    args = ap.parse_args()
    repo = Path(args.repo).resolve()
    rust = repo / "rust"

    have_cargo = shutil.which("cargo") is not None
    rows: list[Row] = []

    # --- 1. Where are we -----------------------------------------------------
    r = Row(1, "checkout identity")
    rc, out = run(["git", "rev-parse", "HEAD"], repo)
    head = out.strip()
    rc2, branch = run(["git", "rev-parse", "--abbrev-ref", "HEAD"], repo)
    rc3, ahead = run(["git", "rev-list", "--count", "origin/main..HEAD"], repo)
    r.passed = rc == 0
    r.detail = f"HEAD={head[:12]} branch={branch.strip()} ahead_of_origin/main={ahead.strip() or '?'}"
    r.remedy = "not a git checkout — STOP"
    r.extra = {"head": head, "branch": branch.strip(), "ahead": ahead.strip()}
    rows.append(r)

    # --- 2. Is the tree clean, ignoring line-ending churn --------------------
    # `git status` on this repo reports ~196 files modified that are CRLF-only.
    # `--ignore-cr-at-eol` is the check that distinguishes real uncommitted work
    # from that churn, which is why this row uses `git diff`, not `git status`.
    r = Row(2, "no real uncommitted work (CRLF churn ignored)")
    rc, out = run(["git", "diff", "--ignore-cr-at-eol", "--stat"], repo)
    rc_s, staged = run(["git", "diff", "--cached", "--name-only"], repo)
    real = out.strip()
    r.passed = real == "" and staged.strip() == ""
    r.detail = (
        "clean (line-ending churn only)"
        if r.passed
        else f"unstaged:\n      {tail(real, 8)}\n      staged: {staged.strip()[:200]}"
    )
    r.remedy = "commit or stash real changes before starting — STOP"
    rows.append(r)

    # --- 3. The constitution mirror ------------------------------------------
    r = Row(3, "CONSTITUTION.md mirror is byte-identical (or absent)")
    tracked = repo / "docs/HERMES_ONE_SHOT_PROMPT.md"
    mirror = repo / "CONSTITUTION.md"
    if not mirror.exists():
        r.passed, r.detail = True, "absent (fine — the tracked doc is the authority)"
    else:
        same = mirror.read_bytes() == tracked.read_bytes()
        r.passed = same
        r.detail = "identical" if same else "STALE — the local mirror has drifted"
        r.remedy = "cp docs/HERMES_ONE_SHOT_PROMPT.md CONSTITUTION.md"
    rows.append(r)

    # --- 4. The decision vector, read out of the code ------------------------
    r = Row(4, "decision vector (read from baselines.rs, not from any document)")
    pins = read_pins(repo)
    missing = [n for n, _ in PINS if n not in pins]
    r.passed = not missing
    r.detail = (
        "  ".join(f"{k}={v}" for k, v in pins.items())
        if r.passed
        else f"could not read: {missing}"
    )
    r.remedy = "baselines.rs is unreadable or renamed — STOP"
    r.extra = pins
    rows.append(r)

    # --- 5. Lint scope is real (a glob matching zero files is a silent no-op) -
    r = Row(5, "hot/money lint scope matches real files")
    sys.path.insert(0, str(repo))
    try:
        from supervisor.gates.hotpath_lint import (  # type: ignore
            check_hotpath_lint,
            load_glob_config,
        )

        hot, money = load_glob_config(str(repo))
        res = check_hotpath_lint(str(repo), hot, money)
        v = res.detail.get("violations", []) if isinstance(res.detail, dict) else []
        allowed = res.detail.get("allowed", []) if isinstance(res.detail, dict) else []
        r.passed = bool(res.passed) and bool(hot) and bool(money)
        r.detail = (
            f"hot_globs={len(hot)} money_globs={len(money)} "
            f"violations={len(v)} explicit LINT-ALLOW={len(allowed)}"
        )
        if v:
            r.detail += "\n      " + tail(
                "\n".join(f"{x['rule']} {x['file']}:{x['line']}" for x in v), 8
            )
        r.remedy = "fix the violation, or add an inline LINT-ALLOW(rule): reason"
        r.extra = {"hot_globs": len(hot), "money_globs": len(money)}
    except Exception as e:  # noqa: BLE001
        r.passed, r.detail = False, f"could not run the lint: {e}"
        r.remedy = "run from the repo root — STOP"
    rows.append(r)

    # --- 6-10. Cargo rows ----------------------------------------------------
    cargo_rows = [
        (6, "cargo fmt --all -- --check", ["fmt", "--all", "--", "--check"], True),
        (
            7,
            "cargo clippy --workspace --all-targets -- -D warnings",
            ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
            True,
        ),
        (
            8,
            "cargo test -p pq-regression (incl. hermes_doc_pins: docs match the code)",
            ["test", "-p", "pq-regression"],
            True,
        ),
        (
            9,
            "cargo test -p pump-quant-core --test ostune_conformance",
            ["test", "-p", "pump-quant-core", "--test", "ostune_conformance"],
            True,
        ),
        (
            10,
            "cargo test --workspace --no-fail-fast",
            ["test", "--workspace", "--no-fail-fast"],
            True,
        ),
    ]
    for n, name, cargs, blocking in cargo_rows:
        r = Row(n, name, blocking=blocking, slow=True)
        if not have_cargo:
            r.passed, r.detail = None, "SKIPPED — cargo not on PATH"
            r.remedy = "install the pinned stable toolchain"
        elif args.fast:
            r.passed, r.detail = None, "SKIPPED (--fast)"
        else:
            rc, out = run(["cargo", *cargs], rust)
            # Windows path-length workaround: `cargo fmt --all --check` fails
            # with OS error 206 (ERROR_FILENAME_EXCED_RANGE) when workspace
            # paths exceed MAX_PATH. Fall back to per-crate checks, which
            # work because each crate path is shorter.
            fmt_path_err = ("error 206" in out.lower()
                            or "too long" in out.lower())
            if rc != 0 and n == 6 and sys.platform == "win32" and fmt_path_err:
                # Re-run fmt per-crate, collecting any real diffs.
                rc2, out2 = run(
                    ["cargo", "fmt", "--all", "--check", "--manifest-path",
                     str(rust / "Cargo.toml")], rust
                )
                if rc2 != 0 and ("error 206" in out2.lower()
                                 or "too long" in out2.lower()):
                    # Per-crate fallback: enumerate workspace members and
                    # check each individually.
                    rc3, out3 = _fmt_per_crate(rust)
                    rc, out = rc3, out3
                else:
                    rc, out = rc2, out2
            r.passed = rc == 0
            r.detail = tail(out, 6)
            r.remedy = "fix and re-run — do not proceed on a red row"
        rows.append(r)

    # --- 11-12. The python gates --------------------------------------------
    for n, name, script in [
        (11, "scripts/regression_e2e.py", "scripts/regression_e2e.py"),
        (12, "scripts/ci_gate.py", "scripts/ci_gate.py"),
    ]:
        r = Row(n, name, slow=True)
        p = repo / script
        if not p.exists():
            r.passed, r.detail = False, f"{script} does not exist"
            r.remedy = "STOP — the gate this row runs is missing"
        elif args.fast:
            r.passed, r.detail = None, "SKIPPED (--fast)"
        else:
            cmd = [sys.executable, str(p)]
            if "ci_gate" in script:
                cmd += [
                    "--repo",
                    str(repo),
                    "--config",
                    "supervisor/config/supervisor.yaml",
                ]
            rc, out = run(cmd, repo)
            r.passed = rc == 0
            r.detail = tail(out, 6)
            r.remedy = "fix and re-run — do not proceed on a red row"
        rows.append(r)

    # --- Report --------------------------------------------------------------
    if args.json:
        print(
            json.dumps(
                {
                    "rows": [
                        {
                            "n": x.n,
                            "name": x.name,
                            "passed": x.passed,
                            "detail": x.detail,
                            "blocking": x.blocking,
                            **({"extra": x.extra} if x.extra else {}),
                        }
                        for x in rows
                    ]
                },
                indent=2,
            )
        )
    else:
        print("=" * 78)
        print("PHASE-B PREFLIGHT — docs/PHASE_B_PREFLIGHT.md §1, mechanical form")
        print("=" * 78)
        for x in rows:
            mark = "PASS" if x.passed else ("SKIP" if x.passed is None else "FAIL")
            print(f"[{mark}] {x.n:>2}. {x.name}")
            if x.detail:
                print(f"      {x.detail}")
            if x.passed is False and x.remedy:
                print(f"      -> {x.remedy}")
        print("-" * 78)

    failed = [x for x in rows if x.passed is False and x.blocking]
    skipped = [x for x in rows if x.passed is None]
    if failed:
        if not args.json:
            print(f"PREFLIGHT FAILED — {len(failed)} blocking row(s): "
                  f"{', '.join(str(x.n) for x in failed)}")
            print("Do NOT begin a Phase-B work item. Report this output verbatim.")
        return 1
    if not args.json:
        if skipped:
            print(
                f"PREFLIGHT PASSED with {len(skipped)} row(s) SKIPPED "
                f"({', '.join(str(x.n) for x in skipped)}) — a skipped row is NOT a pass."
            )
        else:
            print("PREFLIGHT PASSED — every row green.")
        print(
            "\nThis establishes the TREE is sound. It says nothing about the work you are\n"
            "about to do. docs/PHASE_B_PREFLIGHT.md §2 (STOP AND ASK) still applies, and\n"
            "those are decisions no script can see coming."
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
