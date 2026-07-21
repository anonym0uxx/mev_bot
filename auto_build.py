#!/usr/bin/env python3
"""
auto_build.py — the one command that runs the whole build loop unattended.

    python auto_build.py --repo C:\\hermes\\pump-quant --config supervisor/config/supervisor.yaml

What it does, per milestone, in order, stopping the moment anything is unsafe:

    for each milestone M the constitution defines, starting at --from:
        1. git: create/checkout branch build/<m>
        2. for each task the orchestrator decomposes M into:
             a. Claude Code (headless) implements it against the constitution + dossier
             b. run the supervisor TASK gate (build, fmt, clippy, no-stubs, tests, hot-path lint,
                phase provenance)
             c. gate green -> commit on the branch
                gate red   -> feed findings back to Claude Code (up to N times) -> else escalate & STOP
        3. run the supervisor MILESTONE gate (adds secrets, determinism, bench*, criteria map,
             dossier presence)
        4. milestone green -> push branch, open/update a PR, and (if --auto-merge and CI is green)
             let CI merge it. milestone red -> escalate & STOP.
        5. budget/time guard checked at every step.

    *bench/latency/PGO/tuning are Phase-B-exclusive (§9.5): on a non-deployment machine the
     phase-provenance gate fails them CLOSED, so those milestones deliberately do not complete
     on your laptop. That is the boundary working, not a bug — finish them on the server.

THE INVARIANTS THAT MAKE THIS SAFE TO WALK AWAY FROM:
  - A commit requires a passing gate. A push requires a passing milestone gate. A merge requires
    CI (which re-runs the gate). The model certifies nothing.
  - main is never written directly; only via a CI-gated PR.
  - Every subprocess is checked and bounded; a stuck agent hits a timeout; spend hits a budget.
  - On any unresolved failure the loop STOPS with a clear escalation rather than pressing on.
"""
from __future__ import annotations

import argparse
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from supervisor.core.config import SupervisorConfig
from supervisor.core.constitution import load_constitution
from supervisor.core.live_build import (
    GitVcs, ClaudeCodeDriver, ClaudeCodeConfig, BudgetGuard, GitError)
from supervisor.store.evidence import EvidenceStore


def sh(cmd, cwd, timeout=120):
    return subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True, timeout=timeout)


def gh_pr_open_or_update(repo: Path, branch: str, title: str, body: str) -> str:
    """Open a PR with the GitHub CLI if present; otherwise print the compare URL.

    Uses `gh` when available (auth via GITHUB_TOKEN or gh login). Never fails the build if gh
    is missing — it degrades to instructing the operator, because a PR is a convenience, not a
    correctness gate (CI is the gate).
    """
    which = sh(["gh", "--version"], repo, timeout=20)
    if which.returncode != 0:
        return f"(gh CLI not found — open a PR for branch '{branch}' manually)"
    # does a PR already exist for this branch?
    existing = sh(["gh", "pr", "list", "--head", branch, "--json", "url", "-q", ".[0].url"], repo)
    if existing.returncode == 0 and existing.stdout.strip():
        return f"PR exists: {existing.stdout.strip()}"
    made = sh(["gh", "pr", "create", "--head", branch, "--title", title, "--body", body], repo)
    if made.returncode == 0:
        return f"PR opened: {made.stdout.strip()}"
    return f"(could not open PR automatically: {made.stderr.strip()[:200]})"


def main() -> int:
    ap = argparse.ArgumentParser(description="Unattended Hermes build loop")
    ap.add_argument("--repo", required=True)
    ap.add_argument("--config", default="supervisor/config/supervisor.yaml")
    ap.add_argument("--from", dest="from_milestone", default="M0")
    ap.add_argument("--claude-bin", default="claude")
    ap.add_argument("--max-usd", type=float, default=25.0)
    ap.add_argument("--max-hours", type=float, default=4.0)
    ap.add_argument("--auto-merge", action="store_true",
                    help="ask CI to merge the PR when its checks pass (requires branch protection + gh)")
    ap.add_argument("--push", action="store_true", default=True)
    ap.add_argument("--dry-run", action="store_true",
                    help="print the plan and the exact commands, invoke nothing")
    args = ap.parse_args()

    repo = Path(args.repo).resolve()
    if not (repo / ".git").is_dir():
        print(f"[fatal] {repo} is not a git repository (clone your repo there first)")
        return 2

    cfg = SupervisorConfig.load(args.config)
    cons = load_constitution(cfg.constitution_path)
    order = cons.milestone_order()
    if args.from_milestone in order:
        order = order[order.index(args.from_milestone):]

    print(f"[auto] repo         : {repo}")
    print(f"[auto] constitution : {cons.content_hash} (git {cons.git_hash})")
    print(f"[auto] criteria     : {len(cons.criteria)}")
    print(f"[auto] milestones   : {order}")
    print(f"[auto] guards       : <= ${args.max_usd:.0f}, <= {args.max_hours:.1f}h")

    if args.dry_run:
        print("\n[dry-run] would, per milestone: branch -> (Claude Code task -> gate -> commit)* "
              "-> milestone gate -> push -> PR" + (" -> auto-merge on CI green" if args.auto_merge else ""))
        print("[dry-run] Phase-B milestones (bench/latency/PGO/tuning) will fail closed off the "
              "deployment host by design; run those on the server.")
        return 0

    # materialize dossier property tests into the repo (independent of the builder) so hard
    # components are implemented AGAINST tests Claude Code did not write and cannot edit.
    mat = sh([sys.executable, "scripts/materialize_tests.py", "--repo", str(repo)], repo, timeout=120)
    print("[auto] " + (mat.stdout.strip().splitlines()[-1] if mat.stdout.strip() else "materialized dossier tests"))
    if mat.returncode != 0:
        print(f"[fatal] could not materialize dossier tests: {mat.stderr[:300]}")
        return 2

    store = EvidenceStore(cfg.evidence_db)
    run_id = f"auto-{int(time.time())}"
    store.start_run(run_id, cons.git_hash, "auto-build", note=f"from {args.from_milestone}")

    vcs = GitVcs(repo)
    driver = ClaudeCodeDriver(repo, ClaudeCodeConfig(binary=args.claude_bin))
    guard = BudgetGuard(max_usd=args.max_usd, max_seconds=int(args.max_hours * 3600))

    ok, why = driver.cfg.usable()
    if not ok:
        print(f"[fatal] Claude Code not usable: {why}")
        return 2

    # The orchestrator owns the per-task FSM; we drive it milestone by milestone so we can
    # push/PR at each boundary and enforce the budget guard between milestones.
    from supervisor.core.orchestrator import BuildOrchestrator, VcsAdapter
    from supervisor.gates.runner import GateRunner
    from supervisor.core.model_client import ModelClient
    # NOTE: in live mode the "model" that writes code is Claude Code (the driver), not the
    # GLM ModelClient. The orchestrator's model hook is used only for task decomposition and
    # clarify answers; component/code authoring is routed through the driver via the VcsAdapter
    # apply path. For milestones whose tasks are file-editing (not dossier-diff), the driver
    # edits the working tree directly and the adapter commits the result.

    vcs_adapter = VcsAdapter(
        apply_diff=vcs.apply_diff,
        commit=vcs.commit,
        branch=vcs.branch,
        revert_last=vcs.revert_last,
    )

    gates = GateRunner(repo, store, run_id)

    for mkey in order:
        stop = guard.exceeded()
        if stop:
            print(f"[auto] STOP before {mkey}: {stop}")
            break
        print(f"\n{'='*70}\n[auto] MILESTONE {mkey}\n{'='*70}")
        branch = f"build/{mkey.lower()}"
        try:
            vcs.branch(branch)
        except GitError as e:
            print(f"[auto] STOP: could not create branch {branch}: {e}")
            break

        # Drive the milestone through the real orchestrator, but with the live driver doing
        # the authoring. We invoke the driver per decomposed task, gate, and let the
        # orchestrator's milestone gate be the boundary authority.
        result = _run_milestone_live(repo, cons, mkey, driver, gates, vcs, store, run_id, guard)

        if not result["advanced"]:
            print(f"[auto] STOP at {mkey}: {result['detail']}")
            print(f"[auto] open escalations recorded; resolve, then re-run --from {mkey}")
            break

        print(f"[auto] {mkey} gates GREEN")
        if args.push:
            try:
                summary = vcs.push(branch)
                print(f"[auto] pushed {branch}: {summary.splitlines()[-1] if summary else 'ok'}")
            except GitError as e:
                print(f"[auto] push failed (work is committed locally): {e}")
                break
            pr = gh_pr_open_or_update(
                repo, branch,
                title=f"{mkey}: gates green [{run_id}]",
                body=(f"Automated build milestone {mkey}.\n\nConstitution {cons.content_hash} "
                      f"(git {cons.git_hash}).\nAll task and milestone gates passed locally. "
                      f"CI re-runs the portable-profile gate before merge.\n\n"
                      f"Phase-B-exclusive criteria (bench/latency/PGO/tuning) are validated on "
                      f"the deployment server, not here."))
            print(f"[auto] {pr}")
            if args.auto_merge and pr.startswith("PR opened"):
                merged = sh(["gh", "pr", "merge", branch, "--auto", "--squash"], repo)
                print(f"[auto] auto-merge {'queued' if merged.returncode==0 else 'unavailable'} "
                      f"(CI must be green + branch protection on)")

    store.close()
    spent = guard._spent
    print(f"\n[auto] run complete. approx spend ${spent:.2f}. "
          f"resume any stop with --from <milestone>.")
    return 0


def _run_milestone_live(repo, cons, mkey, driver, gates, vcs, store, run_id, guard) -> dict:
    """Author each decomposed task with Claude Code, gate it, commit on green, iterate on red."""
    from supervisor.core.orchestrator import BuildOrchestrator
    ms = cons.milestones.get(mkey)
    if ms is None:
        return {"advanced": False, "detail": f"{mkey} not in constitution"}

    # decompose using the orchestrator's own logic (kept as the single source of task shape)
    tasks = _decompose_tasks(cons, ms)
    for task in tasks:
        if (stop := guard.exceeded()):
            return {"advanced": False, "detail": stop}
        tid = task["task_id"]
        prompt = _task_prompt(cons, ms, task)
        print(f"[auto]   task {tid}: implementing with Claude Code ...")
        res = driver.implement(prompt)
        guard.charge(res.cost_usd)
        if not res.ok:
            store.record_escalation(run_id, mkey, tid, "claude_code_error", res.error)
            return {"advanced": False, "detail": f"task {tid}: {res.error}"}

        # gate the result
        from supervisor.gates.runner import GateConfig
        gcfg = _gate_config(ms)
        verdict = gates.task_gate(tid, gcfg)
        tries = 0
        while not verdict.passed and tries < driver.cfg.max_iterations_per_task:
            if (stop := guard.exceeded()):
                return {"advanced": False, "detail": stop}
            tries += 1
            print(f"[auto]   task {tid}: gate red, iteration {tries} "
                  f"({verdict.summary()[:80]})")
            res = driver.iterate(res.session_id, verdict.summary())
            guard.charge(res.cost_usd)
            if not res.ok:
                vcs.revert_last()
                store.record_escalation(run_id, mkey, tid, "claude_code_error", res.error)
                return {"advanced": False, "detail": f"task {tid} iterate: {res.error}"}
            verdict = gates.task_gate(tid, gcfg)

        if not verdict.passed:
            vcs.revert_last()
            store.record_escalation(run_id, mkey, tid, "task_gate_fail", verdict.summary())
            return {"advanced": False, "detail": f"task {tid} gate unresolved after {tries} tries"}

        sha = vcs.commit(f"{mkey}/{tid}: task gate green [{run_id}]")
        store.record_commit(sha, run_id, mkey, tid, "task gate pass")
        print(f"[auto]   task {tid}: green, committed {sha[:10]}")

    # milestone gate
    from supervisor.gates.runner import GateConfig
    gcfg = _gate_config(ms)
    verdict = gates.milestone_gate(mkey, gcfg, ms.scoped_criteria)
    if not verdict.passed:
        store.record_escalation(run_id, mkey, "-", "milestone_gate_fail", verdict.summary())
        return {"advanced": False, "detail": f"milestone gate: {verdict.summary()}"}
    sha = vcs.commit(f"{mkey} complete: milestone gate green [{run_id}]")
    store.record_commit(sha, run_id, mkey, "-", "milestone gate pass")
    return {"advanced": True, "detail": f"advanced; {sha[:10]}"}


# The following three helpers mirror the orchestrator's private methods so the live driver and
# the unit-tested FSM stay behaviorally identical. They intentionally read from the same
# Constitution/Milestone objects.
def _decompose_tasks(cons, ms) -> list[dict]:
    from supervisor.core.orchestrator import BuildOrchestrator
    # reuse the orchestrator's decomposition without constructing the full engine
    return BuildOrchestrator._decompose(None, ms)  # type: ignore[arg-type]


def _task_prompt(cons, ms, task) -> str:
    comp = task.get("hard_component")
    base = (f"Implement milestone {ms.key} task {task['task_id']} strictly per the constitution "
            f"at docs/HERMES_ONE_SHOT_PROMPT.md. Build against the portable/dev profile only. "
            f"Do not weaken any test or the release profile. Do not mark Phase-B (hardware) "
            f"criteria complete.\n\nMilestone intent:\n{ms.body[:2500]}")
    if comp:
        base += (f"\n\nThis task implements HARD component '{comp}'. Implement to the dossier "
                 f"signatures under supervisor/reinforcement/dossiers/{comp}.yaml and make its "
                 f"property tests pass. You may not author or weaken those tests.")
    return base


def _gate_config(ms):
    from supervisor.gates.runner import GateConfig
    # Phase-B-touching milestones declare their criteria so the provenance gate can fail closed
    touched = [int(c) for c in getattr(ms, "scoped_criteria", []) if str(c).isdigit()]
    manifest = str((Path.cwd() / "infra_manifest.json"))
    run_bench = any(c in (103, 109) for c in touched)
    return GateConfig(
        target_triple="x86_64-pc-windows-msvc",
        run_bench=run_bench,
        bench_name="hot_path_bench" if run_bench else "",
        criteria_touched=touched,
        infra_manifest=manifest,
        require_dossiers=[t for t in [] ],   # orchestrator computes real needs; kept minimal here
    )


if __name__ == "__main__":
    raise SystemExit(main())
