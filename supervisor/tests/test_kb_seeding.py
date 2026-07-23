"""Unit tests for §45.1 KB seeding + §45.2 bias-audit registration and status transitions.

Stdlib unittest (the repo has no pytest); run with `python3 -m unittest`.
"""
from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]
if str(_ROOT) not in sys.path:
    sys.path.insert(0, str(_ROOT))

from supervisor.store.evidence import EvidenceStore, SEEDED_FINDING_STATES, BIAS_AUDIT_LABEL
from supervisor.research.loop import (
    seed_knowledge_base, BIAS_AUDIT_EXPERIMENT_ID, _DEFAULT_KB_SEED,
)


class SeededFindingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.store = EvidenceStore(Path(self.tmp.name) / "ev.db")

    def tearDown(self) -> None:
        self.store.close()
        self.tmp.cleanup()

    def test_status_enum_matches_constitution_45_1(self) -> None:
        # §45.1 exact set.
        self.assertEqual(
            set(SEEDED_FINDING_STATES),
            {"REPRODUCED", "PARTIALLY_REPRODUCED", "UNREPRODUCED", "BIASED_SAMPLE",
             "SUPERSEDED", "FALSIFIED", "UNKNOWN"})

    def test_record_and_get_preserves_provenance(self) -> None:
        f = {
            "id": "f1", "source_file": "docs/x.md", "date": "2026-03-28",
            "dataset": "data/t.jsonl", "sample_size": 856, "strategy_version": "v1",
            "cost_assumptions": "reconciled", "known_bias": "enrichment selection",
            "known_missingness": "failures excluded", "chain_reconciled": True,
            "reproducible": "unknown", "subsequently_contradicted": False,
            "status": "UNREPRODUCED", "conclusion": "claim text",
            "labels": ["HISTORICAL_CANDIDATE", "BIAS_AUDIT_REQUIRED"],
        }
        self.store.record_seeded_finding(f, created_run="r1")
        got = self.store.get_seeded_finding("f1")
        self.assertIsNotNone(got)
        self.assertEqual(got["sample_size"], 856)
        self.assertTrue(got["chain_reconciled"])
        self.assertFalse(got["subsequently_contradicted"])
        self.assertEqual(got["status"], "UNREPRODUCED")
        self.assertIn("BIAS_AUDIT_REQUIRED", got["labels"])
        self.assertEqual(got["conclusion"], "claim text")

    def test_record_rejects_invalid_status(self) -> None:
        with self.assertRaises(ValueError):
            self.store.record_seeded_finding({"id": "bad", "status": "PROVEN_EDGE"})

    def test_record_requires_id(self) -> None:
        with self.assertRaises(ValueError):
            self.store.record_seeded_finding({"status": "UNKNOWN"})

    def test_status_transitions_all_valid_targets(self) -> None:
        self.store.record_seeded_finding({"id": "f2", "status": "UNREPRODUCED"})
        # UNREPRODUCED can move to every other member of the enum.
        for target in SEEDED_FINDING_STATES:
            res = self.store.set_finding_status("f2", target)
            self.assertTrue(res["ok"])
            self.assertEqual(self.store.get_seeded_finding("f2")["status"], target)

    def test_status_transition_rejects_invalid(self) -> None:
        self.store.record_seeded_finding({"id": "f3", "status": "UNKNOWN"})
        with self.assertRaises(ValueError):
            self.store.set_finding_status("f3", "NOT_A_STATUS")

    def test_status_transition_unknown_id(self) -> None:
        res = self.store.set_finding_status("nope", "REPRODUCED")
        self.assertFalse(res["ok"])

    def test_list_filter_by_status(self) -> None:
        self.store.record_seeded_finding({"id": "a", "status": "UNREPRODUCED"})
        self.store.record_seeded_finding({"id": "b", "status": "REPRODUCED"})
        self.store.record_seeded_finding({"id": "c", "status": "UNREPRODUCED"})
        ids = {r["id"] for r in self.store.list_seeded_findings(status="UNREPRODUCED")}
        self.assertEqual(ids, {"a", "c"})
        with self.assertRaises(ValueError):
            self.store.list_seeded_findings(status="WAT")


class KbSeedImporterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.store = EvidenceStore(Path(self.tmp.name) / "ev.db")

    def tearDown(self) -> None:
        self.store.close()
        self.tmp.cleanup()

    def test_default_seed_source_exists_and_is_valid(self) -> None:
        self.assertTrue(_DEFAULT_KB_SEED.is_file(), f"missing seed source {_DEFAULT_KB_SEED}")
        doc = json.loads(_DEFAULT_KB_SEED.read_text(encoding="utf-8"))
        self.assertIn("findings", doc)
        self.assertGreater(len(doc["findings"]), 0)

    def test_seed_imports_findings_and_registers_bias_audit(self) -> None:
        res = seed_knowledge_base(self.store, run_id="r1")
        self.assertGreater(res["findings_seeded"], 0)
        self.assertEqual(res["bias_audit_experiment_id"], BIAS_AUDIT_EXPERIMENT_ID)
        # every finding landed
        self.assertEqual(len(self.store.list_seeded_findings()), res["findings_seeded"])
        # §45.2 experiment registered as a BIAS_AUDIT_REQUIRED-labeled hypothesis row
        h = self.store.get_hypothesis(BIAS_AUDIT_EXPERIMENT_ID)
        self.assertIsNotNone(h)
        self.assertEqual(h["labels"], BIAS_AUDIT_LABEL)
        self.assertIn(BIAS_AUDIT_LABEL, h["statement"])
        self.assertEqual(h["inference_state"], "Hypothesis")

    def test_seed_is_idempotent(self) -> None:
        seed_knowledge_base(self.store, run_id="r1")
        n1 = len(self.store.list_seeded_findings())
        seed_knowledge_base(self.store, run_id="r2")
        n2 = len(self.store.list_seeded_findings())
        self.assertEqual(n1, n2)  # stable ids -> replace, not duplicate
        self.assertEqual(len(self.store.list_hypotheses(inference_state="Hypothesis")), 1)

    def test_seed_missing_source_raises(self) -> None:
        with self.assertRaises(FileNotFoundError):
            seed_knowledge_base(self.store, run_id="r", seed_source="/no/such/seed.json")

    def test_seed_malformed_json_raises(self) -> None:
        bad = Path(self.tmp.name) / "bad.json"
        bad.write_text("{not json", encoding="utf-8")
        with self.assertRaises(ValueError):
            seed_knowledge_base(self.store, run_id="r", seed_source=bad)


class MigrationTests(unittest.TestCase):
    def test_labels_column_backfilled_on_old_store(self) -> None:
        import sqlite3
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        dbp = Path(tmp.name) / "old.db"
        # simulate a pre-labels hypotheses table (12 columns, no labels)
        con = sqlite3.connect(dbp)
        con.execute(
            "CREATE TABLE hypotheses (id TEXT PRIMARY KEY, statement TEXT, causal_mechanism TEXT,"
            " competing_explanations TEXT, disconfirming_evidence_sought TEXT,"
            " expected_net_sol_impact REAL, prior_probability REAL, cost_to_test TEXT,"
            " edge_half_life TEXT, inference_state TEXT, created_run TEXT, updated_at REAL)")
        con.commit()
        con.close()
        store = EvidenceStore(dbp)  # __init__ runs _migrate()
        self.addCleanup(store.close)
        cols = {r[1] for r in store._db.execute("PRAGMA table_info(hypotheses)").fetchall()}
        self.assertIn("labels", cols)
        # and record/get round-trips the label
        store.record_hypothesis({"hypothesis_id": "h", "statement": "s", "causal_mechanism": "m",
                                 "labels": "BIAS_AUDIT_REQUIRED"})
        self.assertEqual(store.get_hypothesis("h")["labels"], "BIAS_AUDIT_REQUIRED")


if __name__ == "__main__":
    unittest.main()
