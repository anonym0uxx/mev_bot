"""Unit tests for the criterion-109 Phase-A/Phase-B split (§9.5, criterion 113).

The blanket `PHASE_B_EXCLUSIVE_CRITERIA = {103, 109}` used to mark ALL of
criterion 109 deployment-hardware-exclusive, masking its Phase-A-obligatory
clauses (zero-alloc harness, hot-path purity lint, unsafe-dossier, money-wrap).
These tests pin the split: 103 stays wholly Phase-B; 109's deploy clauses force
Phase-B; 109's Phase-A clauses are authoring-time-permissible; and the gate stays
fail-closed for hardware work. `_measure` is injected so the tests never depend on
the real host.

Stdlib unittest; run with `python3 -m unittest` or pytest.
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]
if str(_ROOT) not in sys.path:
    sys.path.insert(0, str(_ROOT))

from supervisor.gates import build_phase as bp  # noqa: E402


def _laptop() -> dict:
    # A non-deployment host with a strong (non-fallback) machine id.
    return {"machine_id": "laptop-abc", "id_source": "etc_machine_id",
            "cpu_model": "LaptopCPU"}


class BuildPhaseSplitTests(unittest.TestCase):
    def test_109_is_not_a_blanket_phase_b_criterion(self):
        # The criterion number alone no longer forces Phase-B (the mask is gone).
        self.assertFalse(bp.criterion_is_phase_b(109))
        # 103 is still wholly Phase-B.
        self.assertTrue(bp.criterion_is_phase_b(103))

    def test_clause_predicates_partition_109(self):
        for c in ("deploy_cpu_codegen", "pgo", "windows_tuning",
                  "latency_budgets", "submission_warmth"):
            self.assertTrue(bp.clause_109_is_phase_b(c))
            self.assertFalse(bp.clause_109_is_phase_a_obligatory(c))
        for c in ("zero_alloc_harness", "hot_path_purity_lint",
                  "unsafe_dossier", "money_wrap"):
            self.assertTrue(bp.clause_109_is_phase_a_obligatory(c))
            self.assertFalse(bp.clause_109_is_phase_b(c))
        # Case/whitespace tolerant; unknown clause is neither.
        self.assertTrue(bp.clause_109_is_phase_b("  PGO "))
        self.assertFalse(bp.clause_109_is_phase_b("nonsense"))
        self.assertFalse(bp.clause_109_is_phase_a_obligatory("nonsense"))

    def test_109_alone_is_phase_a_permissible(self):
        # A milestone touching 109 with no deploy clause passes anywhere — its
        # Phase-A clauses are obligatory at authoring time, not deferred.
        r = bp.check_phase_provenance("", [109], "missing.json", _measure=_laptop)
        self.assertTrue(r.ok)
        self.assertEqual(r.phase, "A")

    def test_109_phase_a_clause_is_permissible(self):
        r = bp.check_phase_provenance("", [109], "missing.json",
                                      clauses_109=["hot_path_purity_lint"],
                                      _measure=_laptop)
        self.assertTrue(r.ok)
        self.assertEqual(r.phase, "A")

    def test_109_deploy_clause_fails_closed_off_host(self):
        # A 109 deploy clause needs the deployment host; on a laptop with no
        # manifest it fails closed.
        r = bp.check_phase_provenance("", [109], "missing.json",
                                      clauses_109=["pgo"], _measure=_laptop)
        self.assertFalse(r.ok)

    def test_103_still_fails_closed_off_host(self):
        r = bp.check_phase_provenance("", [103], "missing.json", _measure=_laptop)
        self.assertFalse(r.ok)

    def test_bench_gate_still_phase_b_regardless_of_criteria(self):
        # Deploy work also caught by the Phase-B gate names, so nothing
        # hardware-measured slips through even without a criterion/clause.
        r = bp.check_phase_provenance("bench", [], "missing.json", _measure=_laptop)
        self.assertFalse(r.ok)


if __name__ == "__main__":
    unittest.main()
