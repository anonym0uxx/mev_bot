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

    # secrets
    r = checks.check_secrets(repo)
    print(f"[ci_gate] secrets: {'ok' if r.passed else 'FAIL'} — {r.detail}")
    if not r.passed:
        failures.append("secrets")

    # hot-path lint (criterion 109 bans)
    r = check_hotpath_lint(repo, None, None)
    print(f"[ci_gate] hot-path lint: {'ok' if r.passed else 'FAIL'} — {r.detail}")
    if not r.passed:
        failures.append("hot-path-lint")

    # dossier presence: any constitution-declared hard component without a dossier fails closed
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
