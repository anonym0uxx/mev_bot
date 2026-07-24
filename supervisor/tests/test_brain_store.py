"""Unit tests for brain-evidence persistence in the EvidenceStore.

Stdlib unittest; run with `python3 -m unittest discover -s supervisor/tests`.

Covers the additive migration idiom, re-ingest idempotency, evidence_ref resolution for the
'brain*:' family, and — the point of the whole exercise — that a refused estimate lands in
SQLite as NULL and never as 0.
"""
from __future__ import annotations

import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]
if str(_ROOT) not in sys.path:
    sys.path.insert(0, str(_ROOT))

from supervisor.store.brain_analysis import parse_brain_analysis
from supervisor.store.evidence import BRAIN_TABLES, EvidenceStore
from supervisor.tests.test_brain_analysis_loader import (
    BIG_SIG, all_unknown_artifact, make_artifact, unknown_class,
)

# Estimate columns that MUST be NULL on a refused setup-class row.
_REFUSAL_NULL_COLUMNS = ("n", "median_net_lamports", "mean_net_lamports", "win_rate_bp",
                         "p25_net_lamports", "p75_net_lamports", "median_hold_ns",
                         "nearest_distance")


class BrainStoreTestCase(unittest.TestCase):
    def setUp(self) -> None:
        self._td = tempfile.TemporaryDirectory()
        self.db_path = Path(self._td.name) / "evidence.db"
        self.store = EvidenceStore(self.db_path)

    def tearDown(self) -> None:
        self.store.close()
        self._td.cleanup()

    def _raw(self, sql: str, args: tuple = ()) -> list:
        con = sqlite3.connect(self.db_path)
        try:
            return con.execute(sql, args).fetchall()
        finally:
            con.close()


class MigrationTests(BrainStoreTestCase):
    def test_all_brain_tables_exist(self) -> None:
        names = {r[0] for r in self._raw(
            "SELECT name FROM sqlite_master WHERE type='table'")}
        for t in BRAIN_TABLES:
            self.assertIn(t, names)

    def test_migration_is_idempotent_across_reopens(self) -> None:
        before = self._raw("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")
        for _ in range(3):
            s = EvidenceStore(self.db_path)
            s.close()
        after = self._raw("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")
        self.assertEqual(before, after)

    def test_hypotheses_evidence_ref_column_backfilled_on_an_old_store(self) -> None:
        """A store created before evidence_ref existed gains the column, additively."""
        old = Path(self._td.name) / "old.db"
        con = sqlite3.connect(old)
        con.executescript(
            "CREATE TABLE hypotheses ("
            " id TEXT PRIMARY KEY, statement TEXT, causal_mechanism TEXT,"
            " competing_explanations TEXT, disconfirming_evidence_sought TEXT,"
            " expected_net_sol_impact REAL, prior_probability REAL, cost_to_test TEXT,"
            " edge_half_life TEXT, inference_state TEXT DEFAULT 'Hypothesis',"
            " created_run TEXT, updated_at REAL);")
        con.commit()
        con.close()
        s = EvidenceStore(old)
        try:
            cols = [r[1] for r in s._db.execute("PRAGMA table_info(hypotheses)").fetchall()]
            self.assertIn("labels", cols)
            self.assertIn("evidence_ref", cols)
            # column order must match a fresh SCHEMA table: labels then evidence_ref last
            self.assertEqual(cols[-2:], ["labels", "evidence_ref"])
            # and the positional insert in record_hypothesis still works
            s.record_hypothesis({"hypothesis_id": "H1", "statement": "s",
                                 "causal_mechanism": "m", "competing_explanations": [],
                                 "expected_net_sol_impact": 1.0, "prior_probability": 0.5,
                                 "cost_to_test": "low", "edge_half_life": "weeks",
                                 "evidence_ref": "brain:1"}, created_run="R")
            got = s.get_hypothesis("H1")
            assert got is not None
            self.assertEqual(got["evidence_ref"], "brain:1")
        finally:
            s.close()


class IngestTests(BrainStoreTestCase):
    def test_ingest_writes_snapshot_and_child_rows(self) -> None:
        a = parse_brain_analysis(make_artifact())
        rows = self.store.ingest_brain_analysis("RUN1", a)
        # 1 snapshot + 5 classes + 3 metas + 2 flags + 2 trust + 1 follow + 1 unfollow
        self.assertEqual(rows, 15)
        snap = self.store.latest_brain_snapshot("RUN1")
        assert snap is not None
        self.assertEqual(snap["tick"], 4210)
        self.assertEqual(snap["episodes_total"], 812)
        self.assertEqual(snap["setup_classes_known"], 3)
        self.assertEqual(snap["setup_classes_unknown"], 2)
        self.assertEqual(snap["lenses_known"], 1)
        self.assertEqual(snap["lenses_unknown"], 1)

    def test_round_trip_of_a_known_row(self) -> None:
        a = parse_brain_analysis(make_artifact())
        self.store.ingest_brain_analysis("RUN1", a)
        rows = self.store.list_brain_rows("brain_setup_classes", "RUN1", tick=4210)
        by_key = {(r["signature"], r["venue_phase"]): r for r in rows}
        r = by_key[(BIG_SIG, "pool")]
        self.assertEqual(r["confidence"], "known")
        self.assertEqual(r["n"], 12)
        self.assertEqual(r["median_net_lamports"], -2_000_000)
        self.assertEqual(r["win_rate_bp"], 1_100)
        self.assertIsNone(r["unknown_reason"])

    def test_reingest_same_run_and_tick_does_not_duplicate(self) -> None:
        a = parse_brain_analysis(make_artifact())
        first = self.store.ingest_brain_analysis("RUN1", a)
        counts_first = {t: self._raw(f"SELECT COUNT(*) FROM {t}")[0][0] for t in BRAIN_TABLES}
        second = self.store.ingest_brain_analysis("RUN1", a)
        counts_second = {t: self._raw(f"SELECT COUNT(*) FROM {t}")[0][0] for t in BRAIN_TABLES}
        self.assertEqual(first, second)
        self.assertEqual(counts_first, counts_second)

    def test_reingest_of_a_shrunken_artifact_leaves_no_stale_rows(self) -> None:
        self.store.ingest_brain_analysis("RUN1", parse_brain_analysis(make_artifact()))
        smaller = parse_brain_analysis(make_artifact(setup_classes=[unknown_class()]))
        self.store.ingest_brain_analysis("RUN1", smaller)
        rows = self.store.list_brain_rows("brain_setup_classes", "RUN1", tick=4210)
        self.assertEqual(len(rows), 1)

    def test_distinct_ticks_coexist(self) -> None:
        self.store.ingest_brain_analysis("RUN1", parse_brain_analysis(make_artifact()))
        self.store.ingest_brain_analysis(
            "RUN1", parse_brain_analysis(make_artifact(tick=4211)))
        ticks = sorted({r[0] for r in self._raw("SELECT tick FROM brain_snapshots")})
        self.assertEqual(ticks, [4210, 4211])
        self.assertEqual(self.store.latest_brain_snapshot("RUN1")["tick"], 4211)

    def test_unfollow_row_stores_null_for_fields_the_artifact_omits(self) -> None:
        a = parse_brain_analysis(make_artifact())
        self.store.ingest_brain_analysis("RUN1", a)
        rows = [r for r in self.store.list_brain_rows("brain_follow_reco", "RUN1")
                if r["direction"] == "unfollow"]
        self.assertEqual(len(rows), 1)
        self.assertIsNone(rows[0]["median_lead_ns"])
        self.assertIsNone(rows[0]["trust_tier"])
        self.assertEqual(rows[0]["realized_net_attributed"], -1_900_000)


class NullFidelityTests(BrainStoreTestCase):
    def test_refused_setup_class_columns_are_sql_null(self) -> None:
        a = parse_brain_analysis(make_artifact())
        self.store.ingest_brain_analysis("RUN1", a)
        for col in _REFUSAL_NULL_COLUMNS:
            n_null = self._raw(
                f"SELECT COUNT(*) FROM brain_setup_classes "
                f"WHERE confidence='unknown' AND {col} IS NULL")[0][0]
            n_total = self._raw(
                "SELECT COUNT(*) FROM brain_setup_classes WHERE confidence='unknown'")[0][0]
            self.assertEqual(n_null, n_total, f"{col} was materialised on a refusal")

    def test_no_refused_row_carries_a_fabricated_zero(self) -> None:
        self.store.ingest_brain_analysis("RUN1", parse_brain_analysis(all_unknown_artifact()))
        for col in _REFUSAL_NULL_COLUMNS:
            zeros = self._raw(
                f"SELECT COUNT(*) FROM brain_setup_classes WHERE {col} = 0")[0][0]
            self.assertEqual(zeros, 0, f"{col}=0 appeared where the engine refused to estimate")

    def test_unproven_caller_trust_is_null_not_zero(self) -> None:
        self.store.ingest_brain_analysis("RUN1", parse_brain_analysis(all_unknown_artifact()))
        rows = self.store.list_brain_rows("brain_caller_trust", "RUN1")
        self.assertTrue(rows)
        for r in rows:
            self.assertIsNone(r["score_bp"])
            self.assertIsNone(r["n_markouts"])
            self.assertIsNone(r["platform"])


class EvidenceRefTests(BrainStoreTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.analysis = parse_brain_analysis(make_artifact())
        self.store.ingest_brain_analysis("RUN1", self.analysis)

    def test_snapshot_ref_resolves(self) -> None:
        self.assertTrue(self.store.evidence_ref_resolves("brain:4210"))
        self.assertFalse(self.store.evidence_ref_resolves("brain:9999"))

    def test_setup_class_ref_resolves(self) -> None:
        c = self.analysis.setup_classes[0]
        self.assertTrue(self.store.evidence_ref_resolves(f"brain_setup:4210/{c.store_key}"))
        self.assertFalse(self.store.evidence_ref_resolves("brain_setup:4210/1/curve"))

    def test_refused_class_ref_also_resolves(self) -> None:
        """A refusal is persisted too — a VOI hypothesis about it has resolvable evidence."""
        c = self.analysis.unknown_setup_classes()[0]
        self.assertTrue(self.store.evidence_ref_resolves(f"brain_setup:4210/{c.store_key}"))

    def test_meta_retire_and_caller_refs_resolve(self) -> None:
        self.assertTrue(self.store.evidence_ref_resolves("brain_meta:4210/7"))
        self.assertTrue(self.store.evidence_ref_resolves(
            f"brain_retire:4210/setup_class/{BIG_SIG}|pool"))
        self.assertTrue(self.store.evidence_ref_resolves("brain_caller:4210/5003"))
        self.assertTrue(self.store.evidence_ref_resolves("brain_caller:4210/5001"))

    def test_malformed_brain_refs_do_not_resolve(self) -> None:
        for ref in ("brain:", "brain:notatick", "brain_setup:4210",
                    "brain_meta:4210/notacat", "brain_caller:4210/nope",
                    "brain_nonsense:4210/x", "brain_retire:4210/lane"):
            self.assertFalse(self.store.evidence_ref_resolves(ref), ref)

    def test_amendment_intake_accepts_a_resolvable_brain_ref(self) -> None:
        res = self.store.propose_amendment(
            "strategy", "Review decaying meta 7", "brain-grounded", "brain_meta:4210/7",
            "builder")
        self.assertTrue(res["accepted"], res)

    def test_amendment_intake_rejects_an_unresolvable_brain_ref(self) -> None:
        res = self.store.propose_amendment(
            "strategy", "Review imaginary meta", "prose", "brain_meta:4210/999", "builder")
        self.assertFalse(res["accepted"])


class UnknownTableTests(BrainStoreTestCase):
    def test_list_brain_rows_rejects_an_unknown_table(self) -> None:
        with self.assertRaises(ValueError):
            self.store.list_brain_rows("brain_not_a_table")


if __name__ == "__main__":
    unittest.main()
