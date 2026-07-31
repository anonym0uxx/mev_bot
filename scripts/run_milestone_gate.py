#!/usr/bin/env python3
"""Invoke milestone_gate for the first time and save the honest board."""
import sys, os, json, time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
os.chdir(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import yaml
from supervisor.gates.runner import GateRunner, GateConfig, CRITERION_BINDINGS
from supervisor.store.evidence import EvidenceStore

with open("supervisor/config/supervisor.yaml") as f:
    cfg_data = yaml.safe_load(f)

gate_cfg = cfg_data["gate"]
cfg = GateConfig(
    target_triple=gate_cfg.get("target_triple"),
    production_globs=gate_cfg.get("production_globs", ["rust/**/src/**/*.rs"]),
    required_tests=gate_cfg.get("required_tests", []),
    run_bench=gate_cfg.get("run_bench", False),
    bench_name=gate_cfg.get("bench_name", ""),
    bench_budgets_ns=gate_cfg.get("bench_budgets_ns", {}),
    run_determinism=gate_cfg.get("run_determinism", False),
    replay_bin=gate_cfg.get("replay_bin", ""),
    replay_fixture=gate_cfg.get("replay_fixture", ""),
    run_hotpath_lint=True,
)

store = EvidenceStore("supervisor_evidence.db")
run_id = "milestone_gate_first_invocation_2026-07-31"
runner = GateRunner(repo=".", store=store, run_id=run_id)

scoped = [52, 69, 81, 85, 96, 97, 98, 99, 102, 103, 109, 110, 111, 112, 113, 114, 115, 116]

t0 = time.time()
verdict = runner.milestone_gate("phase_b_first_milestone", cfg, scoped)
elapsed = time.time() - t0

output = {
    "passed": verdict.passed,
    "elapsed_s": round(elapsed, 1),
    "trust_mismatches": verdict.trust_mismatches,
    "results": [
        {"name": r.name, "passed": r.passed, "summary": r.summary,
         "detail_keys": list(r.detail.keys()) if isinstance(r.detail, dict) else str(type(r.detail))}
        for r in verdict.results
    ],
    "criterion_board": [
        {"criterion": k, "binding_type": v.binding_type,
         "check_name": v.check_name, "note": v.note}
        for k, v in sorted(CRITERION_BINDINGS.items())
    ],
}

print(json.dumps(output, indent=2, default=str))

# Also save to file
with open("docs/milestone_gate_result.json", "w") as f:
    json.dump(output, f, indent=2, default=str)
