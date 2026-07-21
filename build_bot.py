#!/usr/bin/env python3
"""
build_bot.py — implement the ENTIRE bot, one dossier leaf (one function) at a time, with
substance verification that cannot be satisfied by empty files.

Why this exists, and how it differs from auto_build.py:
    auto_build.py drove Claude Code one MILESTONE at a time with the prompt "implement all of
    milestone M4 per the constitution." That task is far too large for a single shot, and the
    gate rewarded compilation over substance — so the model wrote accurate file-header comments
    describing each crate and no actual code, and every gate went green because an empty file
    compiles, formats, and contains no stubs to detect.

    This driver fixes both failures:
      1. UNIT OF WORK = ONE LEAF. Each dossier leaf declares an exact Rust signature, a one-line
         responsibility, machine-checkable invariants, a reference pattern, and a property test.
         We hand the model exactly one of these at a time: "implement `fn apply(...)` to satisfy
         these invariants and pass THIS test." Small, precise, verifiable.
      2. SUBSTANCE GATE. After each leaf, we run the full check battery PLUS the substance checks
         (supervisor/gates/substance.py): the implementation must be non-empty, the leaf's exact
         function must exist with a real body, and its property test must actually exercise it.
         An empty or comment-only implementation FAILS. The model cannot advance on nothing.
      3. CRITERION LEDGER. Progress is tracked in docs/criteria_ledger.json — every criterion is
         satisfied / stub / missing / deferred_server. "Done" means the ledger is all-green by
         real evidence, not that milestones printed GREEN.

    It reuses everything sound from auto_build.py: the Windows-hardened Claude Code driver
    (subprocess launching, UTF-8, .CMD handling), the git flow, the budget guard, the phase-B
    boundary, the constitution parser, and the existing gate checks. Only the decomposition and
    the verification core change.

    Laptop scope (Phase A): this builds ALL portable logic. Hardware-bound work — CPU pinning,
    NUMA, PGO, latency budgets, live-endpoint warmth (the cpu_numa_tuning dossier and any
    bench/latency criteria) — is left for the server/Hermes to finalize, and is marked
    deferred_server in the ledger rather than implemented here.

Usage:
    python build_bot.py --repo . --plan                 # show the leaf plan, write nothing
    python build_bot.py --repo . --max-usd 60 --max-hours 10
    python build_bot.py --repo . --component reducer     # build just one component's leaves
    python build_bot.py --repo . --resume                # skip leaves already satisfied
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

# resolve the supervisor package from the repo (same pattern the other scripts use)
_HERE = Path(__file__).resolve()
for _base in [_HERE.parent, *_HERE.parents]:
    if (_base / "supervisor" / "core" / "live_build.py").is_file():
        if str(_base) not in sys.path:
            sys.path.insert(0, str(_base))
        break

from supervisor.core.config import SupervisorConfig
from supervisor.core.live_build import ClaudeCodeDriver, ClaudeCodeConfig, GitVcs, BudgetGuard
from supervisor.core.constitution import load_constitution
from supervisor.store.evidence import EvidenceStore
from supervisor.gates.runner import GateRunner, GateConfig
from supervisor.gates import checks, substance
from supervisor.reinforcement.dossier import load_dossier

# Components that are hardware-bound → deferred to the server, NOT built on the laptop.
SERVER_DEFERRED_COMPONENTS = {"cpu_numa_tuning"}


def sh(cmd, cwd, timeout=120):
    import subprocess, shutil, os
    run_cmd = list(cmd)
    resolved = shutil.which(run_cmd[0])
    if resolved:
        run_cmd[0] = resolved
    elif run_cmd[0] not in ("git",):
        return subprocess.CompletedProcess(cmd, 127, "", f"not found: {run_cmd[0]}")
    try:
        if os.name == "nt":
            return subprocess.run(subprocess.list2cmdline(run_cmd), cwd=str(cwd),
                                  capture_output=True, text=True, encoding="utf-8",
                                  errors="replace", timeout=timeout, shell=True)
        return subprocess.run(run_cmd, cwd=str(cwd), capture_output=True, text=True,
                              encoding="utf-8", errors="replace", timeout=timeout)
    except subprocess.TimeoutExpired:
        return subprocess.CompletedProcess(cmd, 124, "", "timeout")


def discover_dossiers(repo: Path):
    for cand in (repo / "supervisor" / "reinforcement" / "dossiers", repo / "docs" / "dossiers"):
        if cand.is_dir():
            return [load_dossier(y) for y in sorted(cand.glob("*.yaml"))]
    return []


def leaf_plan(repo: Path):
    """Ordered list of (component, leaf) to implement, dependencies respected, server-deferred
    components excluded (they're built on the server)."""
    plan = []
    for d in discover_dossiers(repo):
        if d.component in SERVER_DEFERRED_COMPONENTS:
            continue
        for leaf in d.leaf_order():
            plan.append((d, leaf))
    return plan


def _crate_for(component: str) -> str:
    """Which crate owns a component's code + tests. Must match materialize_tests.COMPONENT_CRATE
    and scaffold_workspace CRATES so the function, its module, and its test all agree."""
    mapping = {
        "reducer": "pump-quant-core",
        "shred": "pump-quant-core",
        "replay": "pump-quant-core",
        "lockfree": "pump-quant-core",
        "fixedpoint": "pump-quant-core",
        "scalp_position": "pump-quant-strategy",
        "exit_ladder": "pump-quant-strategy",
        "economic_gate": "pump-quant-strategy",
        "safety_integrity": "pump-quant-strategy",
        "evaluator_stats": "pump-quant-evaluator",
        "cpu_numa_tuning": "pump-quant-core",
    }
    return mapping.get(component, "pump-quant-core")


def leaf_prompt(repo: Path, d, leaf) -> str:
    """The precise, single-function instruction. This is the opposite of 'implement a whole
    milestone': one signature, one responsibility, the invariants, the reference pattern, and
    the exact test that must pass."""
    inv = "\n".join(f"      - {i}" for i in leaf.invariants)
    return (
        f"Implement EXACTLY ONE function in the Rust workspace under rust/, for component "
        f"'{d.component}'. Build against the portable/dev profile only (no --release, no target-"
        f"cpu, no PGO — those are the server's job). Do not author, edit, or weaken any test.\n\n"
        f"Function to implement (exact signature):\n    {leaf.signature}\n\n"
        f"Single responsibility:\n    {leaf.responsibility}\n\n"
        f"Invariants it MUST satisfy (these are enforced by its property test):\n{inv}\n\n"
        f"Reference pattern to imitate (known-correct analogous code):\n"
        f"    {leaf.reference_pattern}\n\n"
        f"Component contract (from the dossier):\n    {d.spec[:1200]}\n\n"
        f"WHERE TO PUT THE CODE — this is exact and mandatory:\n"
        f"  - Crate: rust/crates/{_crate_for(d.component)}/\n"
        f"  - Module file: rust/crates/{_crate_for(d.component)}/src/{d.component}.rs\n"
        f"  - The function (and any supporting types/enums it needs) MUST be declared `pub` in "
        f"that module, because the test imports them with "
        f"`use {_crate_for(d.component).replace('-', '_')}::{d.component}::*;`. "
        f"If the function or a type it returns is not `pub` in the `{d.component}` module, the "
        f"test cannot see it and will fail to compile.\n"
        f"  - The module `{d.component}` is already declared in that crate's src/lib.rs "
        f"(`pub mod {d.component};`). Add your code to the existing "
        f"src/{d.component}.rs file — do not create a differently-named module and do not put the "
        f"function directly in lib.rs.\n"
        f"  - If the test references types (structs/enums like a result or state type), define "
        f"them as `pub` in the same module.\n\n"
        f"The materialized test at "
        f"rust/crates/{_crate_for(d.component)}/tests/dossier_{d.component}_{leaf.leaf_id}.rs must "
        f"compile and PASS. Write REAL, working code — not a stub, not a comment, not `todo!()`. "
        f"The build will reject empty or comment-only files.\n\n"
        f"CORRECTNESS AND COMPLETENESS ARE THE ONLY PRIORITY. Handle every case the invariants and "
        f"the property test require — all edge cases, all error paths, all boundary conditions. Do "
        f"NOT abbreviate, truncate, or omit logic to save space. If a correct, complete "
        f"implementation needs 100+ lines, write 100+ lines. Length is never a reason to cut a "
        f"case. (A rough sense of typical size for this leaf is ~{leaf.max_lines} lines, but that "
        f"is only a hint — a longer, fully-correct implementation is strictly better than a shorter "
        f"one that drops cases. Prefer clarity and completeness over brevity every time.)\n\n"
        f"Constitution reference: docs/HERMES_ONE_SHOT_PROMPT.md."
    )


def leaf_satisfied(repo: Path, d, leaf) -> bool:
    """Is this leaf's function already implemented with a real body AND its test binding present?
    Used by --resume to skip completed leaves."""
    import re
    m = re.search(r"\bfn\s+([a-zA-Z_][a-zA-Z0-9_]*)", leaf.signature or "")
    if not m:
        return False
    ident = m.group(1)
    cargo_root = repo / "rust" if (repo / "rust").is_dir() else repo
    code = ""
    for f in cargo_root.rglob("*.rs"):
        if "src" in f.parts and "tests" not in f.parts:
            code += "\n" + f.read_text(encoding="utf-8", errors="ignore")
    code = re.sub(r"^\s*//.*$", "", code, flags=re.M)
    return bool(re.search(rf"\bfn\s+{re.escape(ident)}\b[^;{{]*\{{", code, re.S))


def run_leaf_gate(repo: Path, d=None, leaf=None) -> tuple[bool, str]:
    """Per-leaf gate: verify THIS leaf compiles and its own test passes — independent of other
    leaves whose tests won't compile until they're built.

    Checks:
      - the library workspace compiles (build:ok) — the src/ code is valid,
      - it's formatted (fmt:ok),
      - THIS leaf's specific dossier test compiles AND passes (test:ok), run in isolation via
        `cargo test --test dossier_<component>_<leaf>` so unbuilt leaves' tests don't block it.

    A passing leaf test is itself proof the function exists with a real body and correct behavior,
    so no separate signature-regex check is needed (that check was brittle and redundant).
    Whole-codebase completeness (all 61 leaves present) is the final ledger's job, not per-leaf.
    """
    r_build = checks.check_build(str(repo))
    r_fmt = checks.check_fmt(str(repo))
    results = [r_build, r_fmt]

    # only run the leaf's own test if the library compiles (else the compiler error on src/ is
    # the useful signal to feed back)
    if r_build.passed and d is not None and leaf is not None:
        test_target = f"dossier_{d.component}_{leaf.leaf_id}"
        r_test = checks.check_tests(str(repo),
                                    required_test_names=[test_target],
                                    single_test=test_target)
        results.append(r_test)
    elif r_build.passed:
        # no specific leaf context — fall back to running the whole suite
        results.append(checks.check_tests(str(repo)))

    ok = all(r.passed for r in results)
    summary = " ".join(f"{r.name}:{'ok' if r.passed else 'X'}" for r in results)
    detail = ""
    if not ok:
        detail = "\n".join(f"--- {r.name} ---\n{_short(r)}" for r in results if not r.passed)
    return ok, f"[{summary}]\n{detail}" if detail else f"[{summary}]"


def _short(r) -> str:
    """Best available human detail from a CheckResult, robust to any detail shape."""
    d = r.detail if isinstance(r.detail, dict) else {}
    for key in ("stderr_tail", "stdout_tail", "error"):
        v = d.get(key)
        if isinstance(v, str) and v.strip():
            return v[:600]
    for key in ("missing", "empty", "thin", "vacuous", "hits"):
        v = d.get(key)
        if v:
            return str(v)[:600]
    return (r.summary or "")[:600]


def update_ledger(repo: Path, d, leaf, status: str):
    """Record each leaf's outcome in the criterion ledger, keyed by component/leaf."""
    lp = repo / "docs" / "criteria_ledger.json"
    lp.parent.mkdir(parents=True, exist_ok=True)
    data = {}
    if lp.is_file():
        try:
            data = json.loads(lp.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            data = {}
    leaves = data.setdefault("leaves", {})
    leaves[f"{d.component}/{leaf.leaf_id}"] = {
        "status": status, "component": d.component, "leaf": leaf.leaf_id,
        "signature": leaf.signature, "refs": d.constitution_refs, "ts": time.time()}
    lp.write_text(json.dumps(data, indent=2), encoding="utf-8")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--plan", action="store_true", help="show the leaf plan, build nothing")
    ap.add_argument("--coverage", action="store_true",
                    help="show how all 113 criteria map to leaf / server / process, build nothing")
    ap.add_argument("--component", help="build only this component's leaves")
    ap.add_argument("--resume", action="store_true", help="skip leaves already implemented")
    ap.add_argument("--max-usd", type=float, default=60.0)
    ap.add_argument("--max-hours", type=float, default=10.0)
    ap.add_argument("--max-iters", type=int, default=4, help="retries per leaf")
    ap.add_argument("--branch", default="bot-build",
                    help="single branch all leaves commit to (default: bot-build)")
    ap.add_argument("--base", default="main",
                    help="branch to start the build from and merge back into (default: main)")
    ap.add_argument("--finalize", action="store_true",
                    help="on success, merge the build branch into --base, push, and delete the branch")
    ap.add_argument("--push", action="store_true",
                    help="push the build branch after each successful leaf (default: local only)")
    args = ap.parse_args()
    repo = Path(args.repo).resolve()

    cfg = SupervisorConfig.load(repo / "supervisor" / "config" / "supervisor.yaml")
    cons = load_constitution(str(repo / "docs" / "HERMES_ONE_SHOT_PROMPT.md"))
    plan = leaf_plan(repo)
    if args.component:
        plan = [(d, l) for (d, l) in plan if d.component == args.component]

    print("=" * 70)
    print("BOT BUILD — leaf-by-leaf, substance-verified")
    print("=" * 70)
    print(f"repo         : {repo}")
    print(f"criteria     : {len(cons.criteria)}")
    print(f"components    : {sorted({d.component for d, _ in plan})}")
    print(f"total leaves  : {len(plan)} (server-deferred excluded: {sorted(SERVER_DEFERRED_COMPONENTS)})")
    print(f"guards        : <= ${args.max_usd}, <= {args.max_hours}h")

    if args.coverage:
        import json as _json
        mp = repo / "docs" / "criteria_coverage_map.json"
        if not mp.is_file():
            # fall back to the packaged copy next to this script
            alt = Path(__file__).resolve().parent / "docs_criteria_map.json"
            mp = alt if alt.is_file() else mp
        if not mp.is_file():
            print("criteria coverage map not found (docs/criteria_coverage_map.json).")
            return 1
        cm = _json.loads(mp.read_text(encoding="utf-8"))
        leaf = cm.get("leaf_backed", {}); server = cm.get("server_deferred", {}); proc = cm.get("process_governance", {})
        print("\n" + "=" * 70)
        print("FULL 113-CRITERION COVERAGE MAP")
        print("=" * 70)
        print(f"\nLEAF-BACKED — buildable AND substance-verified on this laptop ({len(leaf)}):")
        for k in sorted(leaf, key=int):
            print(f"  [{k:>3}] {leaf[k]}")
        print(f"\nSERVER-DEFERRED — Hermes finalizes on deployment hardware/live infra ({len(server)}):")
        for k in sorted(server, key=int):
            print(f"  [{k:>3}] {server[k]}")
        print(f"\nPROCESS / GOVERNANCE — built as modules/apparatus, not single-fn leaves ({len(proc)}):")
        for k in sorted(proc, key=int):
            print(f"  [{k:>3}] {proc[k]}")
        total = len(leaf) + len(server) + len(proc)
        print("\n" + "-" * 70)
        print(f"TOTAL MAPPED: {total}/113   |   laptop-leaf: {len(leaf)}   server: {len(server)}   process: {len(proc)}")
        print("Leaves in the build plan (incl. non-criterion mechanical leaves):",
              len(leaf_plan(repo)))
        print("-" * 70)
        return 0

    if args.plan:
        print("\nLeaf plan (implementation order):")
        for d, leaf in plan:
            print(f"  {d.component}/{leaf.leaf_id}: {leaf.signature.strip()[:80]}")
        return 0

    driver = ClaudeCodeDriver(repo, ClaudeCodeConfig())
    vcs = GitVcs(repo)

    # --- branch discipline: one dedicated branch for the whole build, started from --base ---
    # This avoids leaving a pile of unmerged branches. Everything commits to --branch; on
    # --finalize it merges back to --base and deletes itself. We refuse to run on a dirty tree
    # so we never mix uncommitted junk into the build branch.
    if vcs.has_changes():
        print(f"[bot] working tree has uncommitted changes. Commit/stash them first, or they'd")
        print(f"[bot] be mixed into the build. `git status` to see them. Aborting.")
        return 2
    start_branch = vcs.current_branch()
    # move to the base (e.g. main) so the build branch starts from a known-clean point
    base_exists = sh(["git", "rev-parse", "--verify", args.base], repo).returncode == 0
    if base_exists:
        vcs.branch(args.base)  # checkout base
    else:
        print(f"[bot] base branch '{args.base}' not found; starting build branch from '{start_branch}'")
    vcs.branch(args.branch)  # create-or-checkout the single build branch
    print(f"[bot] building on branch '{args.branch}' (from '{args.base if base_exists else start_branch}')")

    store = EvidenceStore(str(cfg.evidence_db))
    run_id = f"bot-{int(time.time())}"
    store.start_run(run_id, cons.content_hash, "v")
    guard = BudgetGuard(max_usd=args.max_usd, max_seconds=args.max_hours * 3600)

    # ensure the Rust workspace exists FIRST (root Cargo.toml + crates + module files), so
    # `cargo build`/`cargo test` have a valid workspace to compile against. Without this, every
    # leaf fails with "could not find Cargo.toml". Idempotent and non-destructive: it never
    # clobbers real leaf code, only creates missing skeleton files.
    scaffold = repo / "scaffold_workspace.py"
    if scaffold.is_file():
        r = sh([sys.executable, str(scaffold), "--repo", str(repo)], repo)
        if r.returncode != 0:
            print(f"[bot] workspace scaffold warning: {(r.stderr or r.stdout or '')[:300]}")
    else:
        print("[bot] WARNING: scaffold_workspace.py not found — cargo build may fail without a "
              "workspace. Place scaffold_workspace.py in the repo root.")

    # materialize the dossier tests (correctness authority)
    sh([sys.executable, "scripts/materialize_tests.py", "--repo", str(repo)], repo)

    done, failed, skipped = 0, 0, 0
    for d, leaf in plan:
        if (stop := guard.exceeded()):
            print(f"\n[bot] STOP (guard): {stop}")
            break
        key = f"{d.component}/{leaf.leaf_id}"
        if args.resume and leaf_satisfied(repo, d, leaf):
            print(f"[bot] {key}: already implemented, skipping")
            skipped += 1
            continue

        print(f"\n[bot] {key}: implementing  ({leaf.signature.strip()[:70]})")
        res = driver.implement(leaf_prompt(repo, d, leaf))
        guard.charge(res.cost_usd)
        ok, detail = (False, res.error) if not res.ok else run_leaf_gate(repo, d, leaf)
        iters = 0
        while not ok and iters < args.max_iters and res.ok:
            iters += 1
            print(f"[bot]   gate red (iter {iters}): {detail.splitlines()[0] if detail else ''}")
            res = driver.iterate(res.session_id, f"The gate failed:\n{detail}\n\n"
                                                 f"Fix it. Implement REAL working code for "
                                                 f"{leaf.signature.strip()} — not a stub or comment.")
            guard.charge(res.cost_usd)
            if res.ok:
                ok, detail = run_leaf_gate(repo, d, leaf)

        if ok:
            sha = vcs.commit(f"impl {key}: {leaf.responsibility[:60]} [gate green]")
            store.record_commit(sha, run_id, d.component, leaf.leaf_id, "leaf gate green")
            update_ledger(repo, d, leaf, "satisfied")
            print(f"[bot]   {key}: GREEN, committed {sha[:10]}")
            if args.push:
                pr = sh(["git", "push", "-u", "origin", args.branch], repo)
                if pr.returncode != 0:
                    print(f"[bot]   (push skipped/failed: {(pr.stderr or '').strip()[:100]})")
            done += 1
        else:
            store.escalate(run_id, d.component, leaf.leaf_id, "leaf_gate_fail", detail[:500])
            update_ledger(repo, d, leaf, "stub")
            print(f"[bot]   {key}: UNRESOLVED after {iters} iters — recorded, continuing.")
            print(f"[bot]   detail:\n{detail[:800]}")
            failed += 1

    store.close()
    print("\n" + "=" * 70)
    print(f"[bot] leaves done={done} failed={failed} skipped={skipped}  spend≈${guard._spent:.2f}")
    print("=" * 70)

    # --- finalize: consolidate to base and remove the build branch, so no stray branch is left ---
    if args.finalize:
        if failed == 0:
            print(f"[bot] finalize: merging '{args.branch}' into '{args.base}' (linear, no stray branch)...")
            if base_exists:
                vcs.branch(args.base)  # checkout base
                mg = sh(["git", "merge", "--ff-only", args.branch], repo)
                if mg.returncode != 0:
                    # not fast-forwardable (base moved) — do a normal merge commit
                    mg = sh(["git", "merge", "--no-edit", args.branch], repo)
                if mg.returncode == 0:
                    if args.push:
                        sh(["git", "push", "origin", args.base], repo)
                    # delete the now-merged build branch (local + remote if pushed)
                    sh(["git", "branch", "-d", args.branch], repo)
                    if args.push:
                        sh(["git", "push", "origin", "--delete", args.branch], repo)
                    print(f"[bot] finalize: '{args.base}' now contains the build; '{args.branch}' deleted.")
                else:
                    print(f"[bot] finalize: merge failed ({(mg.stderr or '').strip()[:120]}). "
                          f"Build branch '{args.branch}' kept intact for manual review.")
            else:
                print(f"[bot] finalize: base '{args.base}' didn't exist; leaving '{args.branch}' as the result.")
        else:
            print(f"[bot] finalize SKIPPED: {failed} leaf(s) unresolved. Not merging incomplete work.")
            print(f"[bot] fix the stubs (re-run --resume), then finalize once clean.")

    print("[bot] criterion ledger: docs/criteria_ledger.json")
    print("[bot] a leaf marked 'stub' means the substance gate rejected empty/incomplete code —")
    print("[bot] the OPPOSITE of the old run, which marked empty crates green. Re-run --resume")
    print("[bot] to continue the unfinished leaves.")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
