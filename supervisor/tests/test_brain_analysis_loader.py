"""Unit tests for the `brain_analysis_v1` loader/validator.

Stdlib unittest; run with `python3 -m unittest discover -s supervisor/tests`.

The theme of this file is REFUSAL FIDELITY: a `confidence="unknown"` row must survive the
parse as a row of Nones, and any artifact that muddles the known/unknown contract must be
refused outright rather than half-read.

This module also owns the shared artifact fixture builders (`known_class`, `unknown_class`,
`known_lens`, `unknown_lens`, `make_artifact`, `write_artifact`) that the store, loop and
review test modules import.
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

from supervisor.store.brain_analysis import (
    BrainAnalysisError, RECORD_TAG, SUPPORTED_SCHEMA_VERSION,
    load_brain_analysis, parse_brain_analysis,
)


# A u128 signature that no float can hold exactly — proves the decimal-string carriage.
BIG_SIG = "340282366920938463463374607431768211455"   # u128::MAX

# All estimate fields on a setup-class row, and on a lens row.
SETUP_ESTIMATE_FIELDS = ("n", "median_net_lamports", "mean_net_lamports", "win_rate_bp",
                         "p25_net_lamports", "p75_net_lamports", "median_hold_ns",
                         "nearest_distance")
LENS_ESTIMATE_FIELDS = ("n", "median_net_lamports", "win_rate_bp")


# ------------------------------------------------------------------ fixture builders
def known_class(signature: str = "11111111111111111111", venue_phase: str = "curve",
                meta_category: int = 3, discovery_lane: str = "onchain_creation",
                n: int = 40, median: int = -180_000, mean: int = -150_000,
                win_rate_bp: int = 2_600, p25: int = -400_000, p75: int = 20_000,
                hold_ns: int = 900_000_000, distance: int = 2) -> dict:
    return {
        "signature": signature, "venue_phase": venue_phase, "meta_category": meta_category,
        "discovery_lane": discovery_lane, "confidence": "known", "unknown_reason": None,
        "n": n, "median_net_lamports": median, "mean_net_lamports": mean,
        "win_rate_bp": win_rate_bp, "p25_net_lamports": p25, "p75_net_lamports": p75,
        "median_hold_ns": hold_ns, "nearest_distance": distance,
    }


def unknown_class(signature: str = "22222222222222222222", venue_phase: str = "pool",
                  meta_category: int = 4, discovery_lane: str = "social_caller",
                  reason: str = "insufficient_sample") -> dict:
    row = {
        "signature": signature, "venue_phase": venue_phase, "meta_category": meta_category,
        "discovery_lane": discovery_lane, "confidence": "unknown", "unknown_reason": reason,
    }
    for f in SETUP_ESTIMATE_FIELDS:
        row[f] = None
    return row


def known_lens(lens: str = "momentum", venue_phase: str = "pool", n: int = 63,
               median: int = 61_000, win_rate_bp: int = 5_400) -> dict:
    return {"lens": lens, "venue_phase": venue_phase, "confidence": "known",
            "unknown_reason": None, "n": n, "median_net_lamports": median,
            "win_rate_bp": win_rate_bp}


def unknown_lens(lens: str = "contrarian", venue_phase: str = "curve",
                 reason: str = "empty_index") -> dict:
    row = {"lens": lens, "venue_phase": venue_phase, "confidence": "unknown",
           "unknown_reason": reason}
    for f in LENS_ESTIMATE_FIELDS:
        row[f] = None
    return row


def make_artifact(**over) -> dict:
    """A complete, schema-valid artifact with a mix of known and refused rows."""
    doc = {
        "record": RECORD_TAG,
        "schema_version": SUPPORTED_SCHEMA_VERSION,
        "info_time_ns": 1_753_000_000_000_000_000,
        "tick": 4210,
        "episodes_total": 812,
        "episodes_admitted": 640,
        "setup_classes": [
            known_class(),
            known_class(signature=BIG_SIG, venue_phase="pool", meta_category=7,
                        discovery_lane="active_market", n=12, median=-2_000_000,
                        mean=-1_800_000, win_rate_bp=1_100, p25=-5_000_000, p75=-100_000,
                        hold_ns=120_000_000, distance=5),
            known_class(signature="33333333333333333333", venue_phase="pool",
                        meta_category=3, discovery_lane="active_market", n=90,
                        median=45_000, mean=51_000, win_rate_bp=5_900, p25=-30_000,
                        p75=210_000, hold_ns=600_000_000, distance=1),
            unknown_class(),
            unknown_class(signature="44444444444444444444", venue_phase="curve",
                          meta_category=7, discovery_lane="whale_follow",
                          reason="no_candidate_in_radius"),
        ],
        "lens_scoreboard": [known_lens(), unknown_lens()],
        "best_paying_lens": {"lens": "momentum", "venue_phase": "pool",
                             "median_net_lamports": 61_000, "n": 63},
        "meta_state": [
            {"meta_category": 7, "phase": "decaying", "n": 88,
             "participation_decline_bp": 3_100, "outcome_decline_bp": -2_400},
            {"meta_category": 3, "phase": "hot", "n": 140,
             "participation_decline_bp": 0, "outcome_decline_bp": 400},
            {"meta_category": 9, "phase": "decaying", "n": 30,
             "participation_decline_bp": 1_200, "outcome_decline_bp": -900},
        ],
        "past_meta_matches": [
            {"current_meta": 7, "past_meta": 2, "distance": 3,
             "past_realized_net_lamports": -450_000, "n": 22},
        ],
        "caller_trust": [
            {"author_id": 5001, "platform": "x", "tier": "trusted", "score_bp": 1_800,
             "n_markouts": 41, "exposure": "full"},
            {"author_id": 5002, "platform": None, "tier": "unproven", "score_bp": None,
             "n_markouts": None, "exposure": "none"},
        ],
        "follow_recommendations": [
            {"author_id": 5001, "platform": "x", "n_calls": 41,
             "realized_net_attributed": 3_400_000, "median_lead_ns": 8_000_000_000,
             "trust_tier": "trusted"},
        ],
        "unfollow_candidates": [
            {"author_id": 5003, "platform": "telegram", "realized_net_attributed": -1_900_000,
             "n_calls": 17},
        ],
        "support_inputs_needed": [
            {"kind": "author_track_record", "platform": "telegram", "author_id": 5003,
             "mint_id": None},
        ],
        "retirement_flags": [
            {"subject": "setup_class", "key": f"{BIG_SIG}|pool", "reason": "negative_median",
             "n": 12, "realized_net_lamports": -24_000_000},
            {"subject": "source", "key": "telegram:5003", "reason": "negative_attribution",
             "n": 17, "realized_net_lamports": -1_900_000},
        ],
    }
    doc.update(over)
    return doc


def all_unknown_artifact() -> dict:
    """An artifact in which the brain refused EVERYTHING it was asked.

    No estimate exists anywhere: no conditioned class, no conditioned lens, no nomination, no
    follow/unfollow. This is the fixture the "nothing is ever coerced to 0" tests run on.
    """
    return make_artifact(
        setup_classes=[
            unknown_class(signature="90000000000000000001", venue_phase="curve",
                          meta_category=1, reason="insufficient_sample"),
            unknown_class(signature="90000000000000000002", venue_phase="pool",
                          meta_category=2, reason="empty_index"),
        ],
        lens_scoreboard=[unknown_lens(), unknown_lens(lens="momentum", venue_phase="pool",
                                                     reason="no_episode_in_scope")],
        best_paying_lens=None,
        meta_state=[{"meta_category": 1, "phase": "unknown", "n": 0,
                     "participation_decline_bp": 0, "outcome_decline_bp": 0}],
        past_meta_matches=[],
        caller_trust=[{"author_id": 7001, "platform": None, "tier": "unproven",
                       "score_bp": None, "n_markouts": None, "exposure": "none"}],
        follow_recommendations=[],
        unfollow_candidates=[],
        support_inputs_needed=[{"kind": "author_track_record", "platform": None,
                                "author_id": 7001, "mint_id": None}],
        retirement_flags=[],
    )


def write_artifact(tmpdir: str | Path, doc: dict, name: str = "brain_analysis.json") -> Path:
    p = Path(tmpdir) / name
    p.write_text(json.dumps(doc), encoding="utf-8")
    return p


# ------------------------------------------------------------------------- tests
class LoaderHappyPathTests(unittest.TestCase):
    def test_round_trip_parses_every_section(self) -> None:
        a = parse_brain_analysis(make_artifact())
        self.assertEqual(a.tick, 4210)
        self.assertEqual(a.schema_version, SUPPORTED_SCHEMA_VERSION)
        self.assertEqual(a.episodes_total, 812)
        self.assertEqual(len(a.setup_classes), 5)
        self.assertEqual(len(a.lens_scoreboard), 2)
        self.assertEqual(len(a.meta_state), 3)
        self.assertEqual(len(a.retirement_flags), 2)
        self.assertEqual(len(a.caller_trust), 2)
        self.assertEqual(len(a.follow_recommendations), 1)
        self.assertEqual(len(a.unfollow_candidates), 1)
        self.assertEqual(len(a.support_inputs_needed), 1)
        self.assertEqual(len(a.past_meta_matches), 1)

    def test_u128_signature_survives_as_exact_int(self) -> None:
        a = parse_brain_analysis(make_artifact())
        sigs = [c.signature for c in a.setup_classes]
        self.assertIn(int(BIG_SIG), sigs)
        # exactness: a float round-trip would lose the low bits
        self.assertNotEqual(int(BIG_SIG), int(float(BIG_SIG)))

    def test_load_from_disk(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            p = write_artifact(td, make_artifact())
            a = load_brain_analysis(p)
            self.assertIsNotNone(a)
            assert a is not None
            self.assertEqual(a.tick, 4210)
            self.assertEqual(a.source_path, str(p))

    def test_empty_arrays_and_null_best_lens(self) -> None:
        doc = make_artifact(setup_classes=[], lens_scoreboard=[], meta_state=[],
                            past_meta_matches=[], caller_trust=[],
                            follow_recommendations=[], unfollow_candidates=[],
                            support_inputs_needed=[], retirement_flags=[],
                            best_paying_lens=None)
        a = parse_brain_analysis(doc)
        self.assertEqual(a.setup_classes, ())
        self.assertIsNone(a.best_paying_lens)
        self.assertEqual(a.known_setup_classes(), ())
        self.assertEqual(a.refusals(), ())


class RefusalFidelityTests(unittest.TestCase):
    def test_unknown_row_preserves_every_null(self) -> None:
        a = parse_brain_analysis(make_artifact())
        unknowns = a.unknown_setup_classes()
        self.assertEqual(len(unknowns), 2)
        for c in unknowns:
            for f in SETUP_ESTIMATE_FIELDS:
                self.assertIsNone(getattr(c, f),
                                  f"{f} must stay None on a refusal, got {getattr(c, f)!r}")
            self.assertIsNotNone(c.unknown_reason)

    def test_unknown_lens_preserves_every_null(self) -> None:
        a = parse_brain_analysis(make_artifact())
        for l in a.unknown_lenses():
            for f in LENS_ESTIMATE_FIELDS:
                self.assertIsNone(getattr(l, f))

    def test_known_accessors_never_yield_a_refusal(self) -> None:
        a = parse_brain_analysis(make_artifact())
        for c in a.known_setup_classes():
            self.assertEqual(c.confidence, "known")
            self.assertIsNone(c.unknown_reason)
            for f in SETUP_ESTIMATE_FIELDS:
                self.assertIsNotNone(getattr(c, f))
        for l in a.known_lenses():
            self.assertEqual(l.confidence, "known")
            for f in LENS_ESTIMATE_FIELDS:
                self.assertIsNotNone(getattr(l, f))

    def test_caller_trust_null_score_preserved(self) -> None:
        a = parse_brain_analysis(make_artifact())
        unproven = [t for t in a.caller_trust if t.tier == "unproven"]
        self.assertTrue(unproven)
        for t in unproven:
            self.assertIsNone(t.score_bp)
            self.assertIsNone(t.n_markouts)
            self.assertIsNone(t.platform)

    def test_refusals_enumerates_the_thin_evidence_frontier(self) -> None:
        a = parse_brain_analysis(make_artifact())
        subjects = {r.subject for r in a.refusals()}
        self.assertIn("setup_class", subjects)
        self.assertIn("lens", subjects)
        self.assertIn("caller", subjects)
        for r in a.refusals():
            self.assertTrue(r.reason, "a refusal must carry its reason")

    def test_unknown_row_carrying_a_value_is_refused(self) -> None:
        bad = unknown_class()
        bad["n"] = 0                       # the exact failure this system exists to prevent
        with self.assertRaises(BrainAnalysisError) as cm:
            parse_brain_analysis(make_artifact(setup_classes=[bad]))
        self.assertIn("REFUSAL", str(cm.exception))

    def test_unknown_row_without_a_reason_is_refused(self) -> None:
        bad = unknown_class()
        bad["unknown_reason"] = None
        with self.assertRaises(BrainAnalysisError):
            parse_brain_analysis(make_artifact(setup_classes=[bad]))

    def test_known_row_with_a_null_estimate_is_refused(self) -> None:
        bad = known_class()
        bad["median_net_lamports"] = None
        with self.assertRaises(BrainAnalysisError) as cm:
            parse_brain_analysis(make_artifact(setup_classes=[bad]))
        self.assertIn("median_net_lamports", str(cm.exception))

    def test_unknown_lens_carrying_a_value_is_refused(self) -> None:
        bad = unknown_lens()
        bad["median_net_lamports"] = 0
        with self.assertRaises(BrainAnalysisError):
            parse_brain_analysis(make_artifact(lens_scoreboard=[bad]))


class AdversarialArtifactTests(unittest.TestCase):
    def test_absent_file_is_not_an_error(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            self.assertIsNone(load_brain_analysis(Path(td) / "nope.json"))

    def test_truncated_json_returns_none_and_logs(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "brain_analysis.json"
            p.write_text(json.dumps(make_artifact())[:200], encoding="utf-8")
            with self.assertLogs("supervisor.store.brain_analysis", level="ERROR") as log:
                self.assertIsNone(load_brain_analysis(p))
            self.assertTrue(any("truncated" in m or "JSON" in m for m in log.output))

    def test_wrong_record_tag_returns_none(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            p = write_artifact(td, make_artifact(record="live_status_v1"))
            with self.assertLogs("supervisor.store.brain_analysis", level="ERROR"):
                self.assertIsNone(load_brain_analysis(p))

    def test_newer_schema_version_is_refused_not_reinterpreted(self) -> None:
        doc = make_artifact(schema_version=SUPPORTED_SCHEMA_VERSION + 1)
        with self.assertRaises(BrainAnalysisError) as cm:
            parse_brain_analysis(doc)
        self.assertIn("NEWER", str(cm.exception))
        with tempfile.TemporaryDirectory() as td:
            p = write_artifact(td, doc)
            with self.assertLogs("supervisor.store.brain_analysis", level="ERROR") as log:
                self.assertIsNone(load_brain_analysis(p))
            self.assertTrue(any("REFUSED" in m for m in log.output))

    def test_older_supported_version_still_parses(self) -> None:
        # version <= SUPPORTED is fine; only NEWER is fail-closed.
        a = parse_brain_analysis(make_artifact(schema_version=SUPPORTED_SCHEMA_VERSION))
        self.assertEqual(a.schema_version, SUPPORTED_SCHEMA_VERSION)

    def test_non_object_document_is_refused(self) -> None:
        with self.assertRaises(BrainAnalysisError):
            parse_brain_analysis([1, 2, 3])

    def test_missing_required_array_is_refused(self) -> None:
        doc = make_artifact()
        del doc["retirement_flags"]
        with self.assertRaises(BrainAnalysisError) as cm:
            parse_brain_analysis(doc)
        self.assertIn("retirement_flags", str(cm.exception))

    def test_numeric_signature_is_refused(self) -> None:
        bad = known_class()
        bad["signature"] = 11111111111111111111       # a JSON number, not a decimal string
        with self.assertRaises(BrainAnalysisError):
            parse_brain_analysis(make_artifact(setup_classes=[bad]))

    def test_non_decimal_signature_is_refused(self) -> None:
        bad = known_class()
        bad["signature"] = "0xdeadbeef"
        with self.assertRaises(BrainAnalysisError) as cm:
            parse_brain_analysis(make_artifact(setup_classes=[bad]))
        self.assertIn("decimal", str(cm.exception))

    def test_bool_is_not_accepted_as_an_integer(self) -> None:
        bad = known_class()
        bad["n"] = True
        with self.assertRaises(BrainAnalysisError):
            parse_brain_analysis(make_artifact(setup_classes=[bad]))

    def test_float_estimate_is_refused(self) -> None:
        bad = known_class()
        bad["median_net_lamports"] = -180000.5
        with self.assertRaises(BrainAnalysisError):
            parse_brain_analysis(make_artifact(setup_classes=[bad]))

    def test_bad_confidence_value_is_refused(self) -> None:
        bad = known_class()
        bad["confidence"] = "probably"
        with self.assertRaises(BrainAnalysisError):
            parse_brain_analysis(make_artifact(setup_classes=[bad]))

    def test_directory_path_is_treated_as_absent(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            self.assertIsNone(load_brain_analysis(td))


if __name__ == "__main__":
    unittest.main()
