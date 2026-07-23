"""Unit tests for the §56.5 RootCauseEngine classifier + distribution aggregation.

Stdlib unittest; run with `python3 -m unittest`.
"""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]
if str(_ROOT) not in sys.path:
    sys.path.insert(0, str(_ROOT))

from supervisor.analysis.root_cause import (
    ROOT_CAUSE_CLASSES, classify, RootCauseEngine,
)
from supervisor.store.evidence import EvidenceStore


class TaxonomyTests(unittest.TestCase):
    def test_taxonomy_matches_constitution_56_5(self) -> None:
        # Spot-check the exact §56.5 named classes are present and the set is closed.
        for name in ("ENTRY_LATE", "EXIT_LATE", "CREATOR_RUG", "JITO_MISS", "NOZOMI_MISS",
                     "HELIUS_SENDER_MISS", "SOURCE_GAP", "SOURCE_SUNSET_TRANSITION",
                     "FILTER_COVERAGE_MISS", "PROVIDER_QUOTA", "SCALP_HORIZON_MISS",
                     "SCALP_COST_FLOOR_BREACH", "COPY_BAIT_LOSS", "SELF_DEALING_SIGNAL_FOLLOWED",
                     "ACCOUNT_CONSTRUCTION_ERROR", "PROGRAM_VERSION_DRIFT",
                     "UNKNOWN_PROGRAM_ERROR", "UNSELLABLE", "TERMINAL_LOSS", "UNKNOWN"):
            self.assertIn(name, ROOT_CAUSE_CLASSES)
        self.assertEqual(len(ROOT_CAUSE_CLASSES), len(set(ROOT_CAUSE_CLASSES)))
        self.assertEqual(len(ROOT_CAUSE_CLASSES), 50)


class ClassifierTests(unittest.TestCase):
    def _rc(self, ev: dict) -> str:
        return classify(ev).root_cause

    def test_explicit_root_cause_passthrough(self) -> None:
        self.assertEqual(self._rc({"root_cause": "CREATOR_RUG"}), "CREATOR_RUG")

    def test_explicit_invalid_root_cause_ignored(self) -> None:
        # a non-taxonomy explicit value must not pass through; falls to next signal / UNKNOWN
        self.assertEqual(self._rc({"root_cause": "NONSENSE"}), "UNKNOWN")

    def test_program_error_mapping(self) -> None:
        self.assertEqual(self._rc({"program_error": "account_construction"}),
                         "ACCOUNT_CONSTRUCTION_ERROR")
        self.assertEqual(self._rc({"program_error": "version_drift"}), "PROGRAM_VERSION_DRIFT")
        self.assertEqual(self._rc({"program_error": "weird"}), "UNKNOWN_PROGRAM_ERROR")

    def test_submission_miss_mapping(self) -> None:
        self.assertEqual(self._rc({"submission_miss": "jito"}), "JITO_MISS")
        self.assertEqual(self._rc({"submission_miss": "nozomi"}), "NOZOMI_MISS")
        self.assertEqual(self._rc({"submission_miss": "helius_sender"}), "HELIUS_SENDER_MISS")

    def test_latency_stage_mapping(self) -> None:
        self.assertEqual(self._rc({"latency_stage": "decode"}), "DECODE_LATENCY")
        self.assertEqual(self._rc({"latency_stage": "signing"}), "SIGNING_LATENCY")

    def test_reject_code_mapping(self) -> None:
        self.assertEqual(self._rc({"reject_code": "EconomicallyUnviable"}),
                         "SCALP_COST_FLOOR_BREACH")
        self.assertEqual(self._rc({"reject_code": "Unsellable"}), "UNSELLABLE")
        self.assertEqual(self._rc({"reject_code": "BelowMinOut"}), "SLIPPAGE")
        self.assertEqual(self._rc({"reject_code": "SocialLed"}), "SOCIAL_FALSE_POSITIVE")
        # unknown reject -> GUARD_ABORT (a deterministic guard refusal)
        self.assertEqual(self._rc({"reject_code": "SomethingNew"}), "GUARD_ABORT")

    def test_exit_reason_mapping_and_refinement(self) -> None:
        self.assertEqual(self._rc({"exit_reason": "RugPrecursor"}), "CREATOR_RUG")
        self.assertEqual(self._rc({"exit_reason": "LiquidityAbort"}), "LIQUIDITY_COLLAPSE")
        # TimeStop on the scalp lane -> SCALP_HORIZON_MISS, else EXIT_LATE
        self.assertEqual(self._rc({"exit_reason": "TimeStop", "lane": "scalp"}),
                         "SCALP_HORIZON_MISS")
        self.assertEqual(self._rc({"exit_reason": "TimeStop", "lane": "graduation"}), "EXIT_LATE")
        # ThesisInvalidation defaults TOO_LATE; forfeited upside flips to TOO_EARLY
        self.assertEqual(self._rc({"exit_reason": "ThesisInvalidation"}),
                         "THESIS_INVALIDATION_TOO_LATE")
        self.assertEqual(self._rc({"exit_reason": "ThesisInvalidation", "forfeited_upside": True}),
                         "THESIS_INVALIDATION_TOO_EARLY")
        # a nominal TakeProfit that still booked a loss on the scalp lane -> cost-floor breach
        self.assertEqual(self._rc({"exit_reason": "TakeProfit", "net_lamports": -5,
                                   "lane": "scalp"}), "SCALP_COST_FLOOR_BREACH")
        # clean profitable TakeProfit carries no loss root cause
        self.assertEqual(self._rc({"exit_reason": "TakeProfit", "net_lamports": 100}), "UNKNOWN")

    def test_gate_mapping(self) -> None:
        self.assertEqual(self._rc({"gate": "hot-path-lint"}), "DECISION_LATENCY")
        self.assertEqual(self._rc({"gate": "criteria"}), "BAD_THRESHOLD")

    def test_terminal_flags(self) -> None:
        self.assertEqual(self._rc({"unsellable": True}), "UNSELLABLE")
        self.assertEqual(self._rc({"terminal": True}), "TERMINAL_LOSS")

    def test_priority_order_program_error_over_exit(self) -> None:
        # a row with both a program error and an exit reason resolves to the more proximate cause
        self.assertEqual(self._rc({"program_error": "version_drift",
                                   "exit_reason": "StopLoss"}), "PROGRAM_VERSION_DRIFT")

    def test_empty_row_is_unknown(self) -> None:
        self.assertEqual(self._rc({}), "UNKNOWN")

    def test_every_classification_is_in_taxonomy(self) -> None:
        rows = [{"reject_code": "EconomicallyUnviable"}, {"exit_reason": "RugPrecursor"},
                {"program_error": "weird"}, {"gate": "no-stubs"}, {}]
        for r in rows:
            self.assertIn(classify(r).root_cause, ROOT_CAUSE_CLASSES)


class AggregationTests(unittest.TestCase):
    def test_distribution_counts_and_fractions(self) -> None:
        eng = RootCauseEngine()
        rows = [
            {"exit_reason": "RugPrecursor", "evidence_ref": "reconciled:1"},
            {"exit_reason": "RugPrecursor", "evidence_ref": "reconciled:2"},
            {"reject_code": "Unsellable", "evidence_ref": "reconciled:3"},
            {"exit_reason": "LiquidityAbort", "evidence_ref": "reconciled:4"},
        ]
        rep = eng.aggregate(rows)
        self.assertEqual(rep.total, 4)
        self.assertEqual(rep.counts["CREATOR_RUG"], 2)
        self.assertEqual(rep.counts["UNSELLABLE"], 1)
        self.assertAlmostEqual(rep.fractions["CREATOR_RUG"], 0.5)
        # deterministic ordering: highest count first
        self.assertEqual(list(rep.counts.keys())[0], "CREATOR_RUG")
        # linked records preserved
        self.assertEqual(rep.linked["CREATOR_RUG"], ["reconciled:1", "reconciled:2"])

    def test_deterministic_ordering_ties_alphabetical(self) -> None:
        eng = RootCauseEngine()
        rows = [{"exit_reason": "LiquidityAbort"}, {"reject_code": "Unsellable"}]
        rep = eng.aggregate(rows)
        # both count 1 -> alphabetical: LIQUIDITY_COLLAPSE before UNSELLABLE
        self.assertEqual(list(rep.counts.keys()), ["LIQUIDITY_COLLAPSE", "UNSELLABLE"])

    def test_reflection_block_shape(self) -> None:
        eng = RootCauseEngine()
        block = eng.reflection_block([{"exit_reason": "RugPrecursor"}])
        self.assertEqual(block["constitution"], "§56.5")
        self.assertEqual(block["total_classified"], 1)
        self.assertIn("CREATOR_RUG", block["distribution"])

    def test_persist_to_store(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        store = EvidenceStore(Path(tmp.name) / "ev.db")
        self.addCleanup(store.close)
        eng = RootCauseEngine(store=store, run_id="r1")
        eng.aggregate([{"exit_reason": "RugPrecursor", "evidence_ref": "reconciled:1"},
                       {"reject_code": "Unsellable", "evidence_ref": "reconciled:2"}],
                      persist=True)
        rows = store.list_root_causes(run_id="r1")
        self.assertEqual(len(rows), 2)
        self.assertEqual({r["root_cause"] for r in rows}, {"CREATOR_RUG", "UNSELLABLE"})


if __name__ == "__main__":
    unittest.main()
