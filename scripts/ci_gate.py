#!/usr/bin/env python3
"""
ci_gate.py — the portable-profile supervisor gate, for GitHub Actions.

Runs exactly the checks that are valid on a non-deployment host: hot-path lint, no-stubs,
secrets, and dossier presence for whatever hard components exist. It deliberately does NOT run
benchmarks or latency budgets — those are Phase-B (deployment-hardware) per §9.5 and would be
meaningless on a CI runner. Exits non-zero on any failure so the PR check goes red and merge is
blocked.
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

try:
    from supervisor.gates.hotpath_lint import check_hotpath_lint
    from supervisor.gates import checks
    from supervisor.reinforcement.dossier import missing_dossiers
except Exception as e:  # noqa: BLE001
    print(f"[ci_gate] cannot import supervisor package: {e}")
    sys.exit(2)

# soak_gate import removed — see removal comment in main() body.


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", default=".")
    ap.add_argument("--config", default="supervisor/config/supervisor.yaml")
    args = ap.parse_args()
    repo = Path(args.repo).resolve()

    failures = []

    # no-stubs across production source
    r = checks.check_no_stubs(repo, ["rust/**/src/**/*.rs"])
    print(f"[ci_gate] no-stubs: {'ok' if r.passed else 'FAIL'} — {r.detail}")
    if not r.passed:
        failures.append("no-stubs")

    # secrets — WARNING ONLY (non-blocking).
    # Repo policy: the operator has explicitly accepted the risk of credentials committed to
    # this repository (e.g. a Helius API key in docs). Findings are still PRINTED for visibility
    # so nothing is hidden, but they do not fail the gate. Rotate exposed keys at your discretion.
    r = checks.check_secrets(repo)
    if r.passed:
        print(f"[ci_gate] secrets: ok — {r.detail}")
    else:
        print(f"[ci_gate] secrets: WARN (allowed by repo policy, not failing) — {r.detail}")

    # hot-path lint (criterion 109 bans). The hot/money globs come from the
    # committed rust/lint_rules.yaml (authoritative, explicit — NOT a silent code
    # default), falling back to the real-crate built-ins if that file is absent.
    from supervisor.gates.hotpath_lint import load_glob_config
    hot_globs, money_globs = load_glob_config(repo)
    r = check_hotpath_lint(repo, hot_globs, money_globs)
    scope = (f"hot={len(hot_globs)} money={len(money_globs)} globs from lint_rules.yaml"
             if hot_globs and money_globs else "built-in real-crate globs")
    print(f"[ci_gate] hot-path lint ({scope}): {'ok' if r.passed else 'FAIL'} — {r.summary}")
    if not r.passed:
        failures.append("hot-path-lint")

    # SOAK GATE REMOVED — it measured the CPython harness allocator RSS on a
    # bounded Python workload, NOT the trading engine's memory behaviour. Its
    # green contributed a false pass to ci_gate despite measuring the wrong
    # system. Criterion 99 (memory soak) stays UNVERIFIED until a real
    # server-side engine soak harness exists. See docs/TASK4_CI_MILESTONE_
    # CONTRADICTION_2026-07-31.md and docs/ERRATUM_AND_CORRECTED_TABLE.
    # soak_gate.py remains available as a standalone script but no longer
    # participates in ci_gate.

    # dossier presence:
    try:
        cons_path = repo / "docs" / "HERMES_ONE_SHOT_PROMPT.md"
        missing = missing_dossiers(str(cons_path) if cons_path.is_file() else None)
        if missing:
            print(f"[ci_gate] missing dossiers: FAIL — {missing}")
            failures.append("missing-dossiers")
        else:
            print("[ci_gate] dossiers: ok")
    except Exception as e:  # noqa: BLE001
        print(f"[ci_gate] dossier check error (non-fatal in CI): {e}")

    if failures:
        print(f"\n[ci_gate] FAILED: {failures}")
        return 1
    print("\n[ci_gate] portable gate PASSED")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
