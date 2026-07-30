"""Supervisor REGRESSION INVARIANTS (additive) — the term sets and self-healing
properties that must NOT silently shrink or break as the supervisor evolves.

Owned by the end-to-end regression layer, not the module authors. Three guards:

  1. §45.1 evidence-status enum + §56.10 inference ladder — exact membership and
     COUNT are pinned, the SQL CHECK constraints agree with the Python tuples,
     and every member round-trips through the store (a shrink is caught).
  2. §56.5 RootCauseEngine taxonomy — 50 classes, closed set, every one
     round-trips through the classifier (explicit passthrough) and lands in the
     distribution aggregator.
  3. Self-healing properties — the soak gate still CATCHES an injected leak
     (not vacuous), and evidence.py migrations are IDEMPOTENT (constructing the
     store twice against the same file, and calling _migrate twice, is a no-op).

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
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import soak_gate  # noqa: E402

from supervisor.analysis.root_cause import (  # noqa: E402
    ROOT_CAUSE_CLASSES,
    RootCauseEngine,
    classify,
)
from supervisor.store.evidence import (  # noqa: E402
    SCHEMA,
    SEEDED_FINDING_STATES,
    VALID_INFERENCE_STATES,
    EvidenceStore,
)

# Frozen baselines — a DROP below any of these is the regression this file exists
# to catch. Growth is allowed (add a class/status), shrink is not.
BASELINE_ROOT_CAUSE_CLASSES = 50
BASELINE_SEEDED_FINDING_STATES = 7
BASELINE_INFERENCE_STATES = 7


# --------------------------------------------------- §45.1 / §56.10 enums

class StatusEnumInvariants(unittest.TestCase):
    def test_seeded_finding_states_do_not_shrink_and_are_closed(self) -> None:
        self.assertGreaterEqual(
            len(SEEDED_FINDING_STATES), BASELINE_SEEDED_FINDING_STATES,
            "the §45.1 evidence-status enum shrank",
        )
        self.assertEqual(len(SEEDED_FINDING_STATES), len(set(SEEDED_FINDING_STATES)))
        # The exact §45.1 members must all still be present.
        for member in ("REPRODUCED", "PARTIALLY_REPRODUCED", "UNREPRODUCED",
                       "BIASED_SAMPLE", "SUPERSEDED", "FALSIFIED", "UNKNOWN"):
            self.assertIn(member, SEEDED_FINDING_STATES)

    def test_inference_states_do_not_shrink_and_are_closed(self) -> None:
        self.assertGreaterEqual(
            len(VALID_INFERENCE_STATES), BASELINE_INFERENCE_STATES,
            "the §56.10 inference ladder shrank",
        )
        self.assertEqual(len(VALID_INFERENCE_STATES), len(set(VALID_INFERENCE_STATES)))
        for member in ("Observation", "Hypothesis", "ProvisionalInference",
                       "ValidatedInference", "RejectedInference", "ExpiredInference",
                       "RegimeSpecificInference"):
            self.assertIn(member, VALID_INFERENCE_STATES)

    def test_sql_check_constraints_agree_with_python_tuples(self) -> None:
        # The schema's CHECK(... IN (...)) lists must contain every Python-tuple
        # member — otherwise a member accepted by Python is rejected by SQLite
        # (or vice-versa), a silent divergence.
        for member in SEEDED_FINDING_STATES:
            self.assertIn(f"'{member}'", SCHEMA, f"status {member} missing from schema CHECK")
        for member in VALID_INFERENCE_STATES:
            self.assertIn(f"'{member}'", SCHEMA, f"inference state {member} missing from CHECK")

    def test_every_seeded_status_round_trips_through_the_store(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            store = EvidenceStore(Path(d) / "e.db")
            store.record_seeded_finding({"id": "f1", "status": "UNKNOWN",
                                         "conclusion": "seed"})
            for status in SEEDED_FINDING_STATES:
                res = store.set_finding_status("f1", status)
                self.assertTrue(res["ok"], res)
                self.assertEqual(store.get_seeded_finding("f1")["status"], status)
            # A non-member is refused, never silently coerced.
            with self.assertRaises(ValueError):
                store.set_finding_status("f1", "NOT_A_STATUS")
            store.close()

    def test_every_inference_state_round_trips_through_the_store(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            store = EvidenceStore(Path(d) / "e.db")
            store.record_hypothesis({"hypothesis_id": "h1", "statement": "s",
                                     "competing_explanations": []})
            for state in VALID_INFERENCE_STATES:
                res = store.set_inference_state("h1", state)
                self.assertTrue(res["ok"], res)
                self.assertEqual(store.get_hypothesis("h1")["inference_state"], state)
            with self.assertRaises(ValueError):
                store.set_inference_state("h1", "NOT_A_STATE")
            store.close()


# --------------------------------------------------- §56.5 taxonomy

class TaxonomyInvariants(unittest.TestCase):
    def test_taxonomy_does_not_shrink_and_is_closed(self) -> None:
        self.assertGreaterEqual(
            len(ROOT_CAUSE_CLASSES), BASELINE_ROOT_CAUSE_CLASSES,
            "the §56.5 root-cause taxonomy shrank",
        )
        self.assertEqual(len(ROOT_CAUSE_CLASSES), len(set(ROOT_CAUSE_CLASSES)),
                         "duplicate class in taxonomy")

    def test_every_class_round_trips_through_classifier(self) -> None:
        # Explicit passthrough is the identity contract: a row asserting a valid
        # class classifies to exactly that class. If a class were dropped from
        # the set, its passthrough would fall through to UNKNOWN — caught here.
        for cls in ROOT_CAUSE_CLASSES:
            self.assertEqual(classify({"root_cause": cls}).root_cause, cls)

    def test_every_class_survives_distribution_aggregation(self) -> None:
        rows = [{"root_cause": c} for c in ROOT_CAUSE_CLASSES]
        report = RootCauseEngine().aggregate(rows)
        # Each distinct class appears exactly once in the input → present in the
        # distribution with count 1, and the fractions sum to ~1.0.
        for cls in ROOT_CAUSE_CLASSES:
            self.assertEqual(report.counts.get(cls), 1, f"class {cls} vanished from distribution")
        self.assertAlmostEqual(sum(report.fractions.values()), 1.0, places=6)


# --------------------------------------------------- self-healing props

class SelfHealingInvariants(unittest.TestCase):
    def test_soak_gate_still_catches_an_injected_leak(self) -> None:
        # The gate is not vacuous: an unbounded accumulator MUST fail both the
        # slope and spread bounds. If this ever passes, the leak detector broke.
        # warmup=6, checkpoints=14: Windows needs more warmup now that the gate
        # reads real RSS via GetProcessMemoryInfo (previously vacuous — returned 0).
        store: list = []
        res = soak_gate.run_soak(checkpoints=14, warmup=6, rounds_per_checkpoint=3,
                                 workload=lambda r: soak_gate.leaky_workload(store, r))
        self.assertFalse(res.passed, res.summary())
        # And the bounded workload still passes on the same machine.
        ok = soak_gate.run_soak(checkpoints=14, warmup=6, rounds_per_checkpoint=2)
        self.assertTrue(ok.passed, ok.summary())

    def test_evidence_migrations_are_idempotent(self) -> None:
        # Constructing the store twice against the SAME file re-runs SCHEMA +
        # _migrate; it must be a no-op (additive ADD COLUMN guarded by a column
        # check). Then call _migrate a third time directly — still a no-op.
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "e.db"
            store1 = EvidenceStore(path)
            store1.record_hypothesis({"hypothesis_id": "h1", "statement": "s",
                                      "competing_explanations": []})
            cols_before = _hypotheses_columns(store1)
            store1.close()

            # Second open on the same file — schema+migrate run again.
            store2 = EvidenceStore(path)
            self.assertEqual(_hypotheses_columns(store2), cols_before,
                             "migration changed the schema on reopen")
            # Data survived and is still readable.
            self.assertIsNotNone(store2.get_hypothesis("h1"))
            # Direct third migrate pass — no exception, no schema change.
            store2._migrate()  # noqa: SLF001 — exercising idempotency deliberately
            store2._migrate()  # noqa: SLF001
            self.assertEqual(_hypotheses_columns(store2), cols_before)
            store2.close()

    def test_labels_column_present_after_migration(self) -> None:
        # The one real migration (§45.2 hypotheses.labels) must exist after open.
        with tempfile.TemporaryDirectory() as d:
            store = EvidenceStore(Path(d) / "e.db")
            self.assertIn("labels", _hypotheses_columns(store))
            store.close()


def _hypotheses_columns(store: EvidenceStore) -> set:
    return {r[1] for r in store._db.execute("PRAGMA table_info(hypotheses)").fetchall()}  # noqa: SLF001


if __name__ == "__main__":
    unittest.main()
