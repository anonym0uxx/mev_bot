#!/usr/bin/env python3
"""
materialize_tests.py — render each HARD component's dossier property tests into the repo as
real, LOCKED Rust test files BEFORE the builder touches that component.

Why this exists (the independence boundary, made physical):
    A dossier's property test is "the sole authority over correctness" for its component. That
    only holds if the BUILDER does not author the test — otherwise the model grades its own
    homework, the exact circularity the constitution forbids. In the GLM/reinforcement loop the
    test was fed to the model as text and written to a scratch crate by the orchestrator. For a
    Claude-Code-driven build there is no such loop, so this script does that job independently:

      1. It reads every dossier (the same discovery the supervisor uses).
      2. For each leaf it writes the property_test verbatim into
         rust/<crate>/tests/dossier_<component>_<leaf>.rs
      3. It records a manifest of SHA-256 hashes of every materialized test.
      4. `--verify` re-hashes and fails if any materialized test was altered — so a builder that
         edits a test to make it pass is caught mechanically. The build driver and CI call
         --verify before trusting a green result.

    Combined with `.claude/settings.json` denying edits to these files, the builder provably
    implements AGAINST tests it did not write and cannot change.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

# resolve the supervisor package from any placement (scaffold, assembled repo, package root)
_HERE = Path(__file__).resolve()
for _base in [_HERE.parent.parent, *_HERE.parents]:
    if (_base / "supervisor" / "reinforcement" / "dossier.py").is_file():
        sys.path.insert(0, str(_base))
        break

try:
    from supervisor.reinforcement.dossier import load_dossier, available_dossiers
except Exception as e:  # noqa: BLE001
    print(f"[materialize] cannot import supervisor dossier loader: {e}")
    sys.exit(2)


# component -> crate that owns its tests. Components without an explicit mapping fall back to a
# shared integration crate. Kept conservative; the assembler's workspace defines these crates.
COMPONENT_CRATE = {
    "reducer": "pump-quant-core",
    "shred": "pump-quant-core",
    "replay": "pump-quant-core",
    "lockfree": "pump-quant-core",
    "fixedpoint": "pump-quant-core",
    "scalp_position": "pump-quant-strategy",
    "exit_ladder": "pump-quant-strategy",
    "economic_gate": "pump-quant-strategy",
    "evaluator_stats": "pump-quant-evaluator",
    "cpu_numa_tuning": "pump-quant-core",
    "safety_integrity": "pump-quant-strategy",
    "active_market_universe": "pump-quant-signals",
    "attention_decay": "pump-quant-narrative",
    "attention_state": "pump-quant-narrative",
    "catalyst_classifier": "pump-quant-narrative",
    "convexity_ledger": "pump-quant-evaluator",
    "decode": "pump-quant-protocol",
    "driver": "pump-quant-replay",
    "edge_decomposition": "pump-quant-evaluator",
    "entry_arbitration": "pump-quant-strategy",
    "entry_zone": "pump-quant-evaluator",
    "fdr": "pump-quant-evaluator",
    "fee_plausibility": "pump-quant-signals",
    "market_structure": "pump-quant-features",
    "memory_pressure": "pump-quant-core",
    "metrics": "pump-quant-evaluator",
    "overfitting": "pump-quant-evaluator",
    "risk_type": "pump-quant-strategy",
    "setup_archetype": "pump-quant-strategy",
    "setup_classifier": "pump-quant-signals",
    "sizing_validator": "pump-quant-evaluator",
    "clock": "pump-quant-clock",
    "config": "pump-quant-app",
    "determinants": "pump-quant-social",
    "envelope": "pump-quant-governance",
    "hazard": "pump-quant-simulator",
    "lifecycle": "pump-quant-domain",
    "manifest": "pump-quant-journal",
    "micro": "pump-quant-features",
    "narrative": "pump-quant-narrative",
    "rank": "pump-quant-watchlist",
    "regime": "pump-quant-market-state",
    "smart_money": "pump-quant-wallet-graph",
    "types": "pump-quant-canonical",
    "voi": "pump-quant-memory",
}
DEFAULT_CRATE = "pump-quant-core"

HEADER = (
    "// GENERATED FROM DOSSIER — DO NOT EDIT.\n"
    "// This property test is the correctness authority for the '{comp}' component "
    "(leaf '{leaf}').\n"
    "// It was materialized independently of the builder. Editing it is a build-integrity\n"
    "// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.\n"
    "// To change a component's contract, change its dossier and re-materialize — never edit here.\n"
    "// The glob import below brings the leaf's public items into scope; integration tests in\n"
    "// tests/ are a separate crate, so the implementation must be `pub` and reachable here.\n"
    "#![allow(unused_imports, dead_code, clippy::all)]\n"
    "use {crate_ident}::{module}::*;\n\n"
)


def _dossier_dir(repo: Path) -> Path:
    # dossiers ship under supervisor/reinforcement/dossiers in the assembled repo
    for cand in (repo / "supervisor" / "reinforcement" / "dossiers",
                 Path(__file__).resolve().parent.parent / "supervisor"
                 / "reinforcement" / "dossiers"):
        if cand.is_dir():
            return cand
    return repo / "supervisor" / "reinforcement" / "dossiers"


def _test_path(repo: Path, component: str, leaf_id: str) -> Path:
    crate = COMPONENT_CRATE.get(component, DEFAULT_CRATE)
    # crates live under rust/crates/<crate>/ (matching scaffold_workspace.py); integration tests
    # go in that crate's tests/ dir so `cargo test` actually compiles and runs them.
    return repo / "rust" / "crates" / crate / "tests" / f"dossier_{component}_{leaf_id}.rs"


def materialize(repo: Path, verify: bool) -> int:
    ddir = _dossier_dir(repo)
    if not ddir.is_dir():
        print(f"[materialize] no dossiers dir at {ddir}")
        return 2
    manifest_path = repo / ".hermes_dossier_tests.json"
    prior: dict[str, str] = {}
    if manifest_path.is_file():
        try:
            prior = json.loads(manifest_path.read_text(encoding="utf-8")).get("hashes", {})
        except json.JSONDecodeError:
            prior = {}

    current: dict[str, str] = {}
    written, mismatches, missing = 0, [], []

    for yml in sorted(ddir.glob("*.yaml")):
        try:
            d = load_dossier(yml)
        except Exception as e:  # noqa: BLE001
            print(f"[materialize] skip {yml.name}: {e}")
            continue
        for leaf in d.leaf_order():
            crate_name = COMPONENT_CRATE.get(d.component, DEFAULT_CRATE)
            crate_ident = crate_name.replace("-", "_")
            body = HEADER.format(comp=d.component, leaf=leaf.leaf_id,
                                 crate_ident=crate_ident, module=d.component) \
                   + leaf.property_test.rstrip() + "\n"
            key = f"{d.component}/{leaf.leaf_id}"
            sha = hashlib.sha256(body.encode("utf-8")).hexdigest()
            current[key] = sha
            path = _test_path(repo, d.component, leaf.leaf_id)

            if verify:
                if not path.is_file():
                    missing.append(key)
                    continue
                actual = hashlib.sha256(path.read_text(encoding="utf-8").encode("utf-8")).hexdigest()
                # the file must match what THIS dossier renders (catches edits AND drift)
                if actual != sha:
                    mismatches.append(key)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(body, encoding="utf-8")
                written += 1

    if verify:
        problems = []
        if missing:
            problems.append(f"missing materialized tests: {missing}")
        if mismatches:
            problems.append(f"ALTERED materialized tests (builder must not edit these): {mismatches}")
        # also catch a test that was materialized before but a dossier/leaf vanished (tamper)
        removed = sorted(set(prior) - set(current))
        if removed:
            problems.append(f"tests present in prior manifest but no longer backed by a dossier: {removed}")
        if problems:
            for p in problems:
                print(f"[materialize:verify] FAIL — {p}")
            return 1
        print(f"[materialize:verify] OK — {len(current)} dossier tests intact and unmodified")
        return 0

    manifest_path.write_text(
        json.dumps({"hashes": current, "count": len(current)}, indent=2), encoding="utf-8")
    print(f"[materialize] wrote {written} dossier property tests + manifest "
          f"({len(current)} leaves). These are the correctness authority; the builder "
          f"implements against them and may not edit them.")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--verify", action="store_true",
                    help="re-hash materialized tests and fail if any were altered or are missing")
    args = ap.parse_args()
    return materialize(Path(args.repo).resolve(), args.verify)


if __name__ == "__main__":
    raise SystemExit(main())
