"""Unit tests for the brain-review strategy-analysis report.

Stdlib unittest; run with `python3 -m unittest discover -s supervisor/tests`.

Pins the two things a governance document must not get wrong: deterministic ordering (so a
week-on-week diff means something), and the standing statement that a NOMINATION IS NOT A
RETIREMENT — plus the refused section, which reports absent evidence as absent.
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]
if str(_ROOT) not in sys.path:
    sys.path.insert(0, str(_ROOT))

from supervisor.analysis.brain_review import (
    NOMINATION_DISCLAIMER, REFUSAL_DISCLAIMER, render_brain_review, review_brain_analysis,
)
from supervisor.store.brain_analysis import parse_brain_analysis
from supervisor.tests.test_brain_analysis_loader import (
    BIG_SIG, all_unknown_artifact, make_artifact,
)


class ReviewContentTests(unittest.TestCase):
    def setUp(self) -> None:
        self.review = review_brain_analysis(parse_brain_analysis(make_artifact()))

    def test_counts_separate_conditioned_from_refused(self) -> None:
        c = self.review["counts"]
        self.assertEqual(c["setup_classes_total"], 5)
        self.assertEqual(c["setup_classes_known"], 3)
        self.assertEqual(c["setup_classes_refused"], 2)
        self.assertEqual(c["lenses_known"], 1)
        self.assertEqual(c["lenses_refused"], 1)
        self.assertEqual(c["metas_decaying"], 2)
        self.assertEqual(c["retirement_nominations"], 2)

    def test_retirement_rows_are_nominations_with_n_and_realized_net(self) -> None:
        noms = self.review["retirement_nominations"]
        self.assertEqual(len(noms), 2)
        for n in noms:
            self.assertEqual(n["status"], "NOMINATED_FOR_REVIEW")
            self.assertEqual(n["disclaimer"], NOMINATION_DISCLAIMER)
            self.assertIsInstance(n["n"], int)
            self.assertIsInstance(n["realized_net_lamports"], int)
            self.assertTrue(n["evidence_ref"].startswith("brain_retire:4210/"))
        # worst realised net first
        self.assertEqual(noms[0]["key"], f"{BIG_SIG}|pool")

    def test_lens_section_reports_only_conditioned_cells(self) -> None:
        lenses = self.review["lens_paying"]
        self.assertEqual(len(lenses), 1)
        self.assertEqual(lenses[0]["lens"], "momentum")
        self.assertEqual(lenses[0]["venue_phase"], "pool")
        self.assertEqual(lenses[0]["n"], 63)
        self.assertAlmostEqual(lenses[0]["median_net_sol"], 61_000 / 1e9)
        self.assertEqual(self.review["best_paying_lens"]["lens"], "momentum")

    def test_decaying_metas_ordered_by_steepest_outcome_decline(self) -> None:
        metas = self.review["decaying_metas"]
        self.assertEqual([m["meta_category"] for m in metas], [7, 9])
        self.assertEqual(metas[0]["outcome_decline_bp"], -2_400)

    def test_callers_split_into_follow_and_unfollow(self) -> None:
        self.assertEqual([f["author_id"] for f in self.review["callers_follow"]], [5001])
        self.assertEqual([u["author_id"] for u in self.review["callers_unfollow"]], [5003])
        self.assertEqual(self.review["callers_unfollow"][0]["realized_net_attributed"],
                         -1_900_000)

    def test_refused_section_is_the_research_agenda_and_carries_no_numbers(self) -> None:
        refused = self.review["refused"]
        self.assertTrue(refused)
        subjects = {r["subject"] for r in refused}
        self.assertIn("setup_class", subjects)
        self.assertIn("lens", subjects)
        self.assertIn("caller", subjects)
        for r in refused:
            self.assertIsNone(r["estimate"])
            self.assertTrue(r["reason"])

    def test_support_inputs_are_reported(self) -> None:
        self.assertEqual(self.review["support_inputs_needed"],
                         [{"kind": "author_track_record", "platform": "telegram",
                           "author_id": 5003, "mint_id": None}])


class DeterminismTests(unittest.TestCase):
    def test_review_dict_is_identical_across_runs(self) -> None:
        a = review_brain_analysis(parse_brain_analysis(make_artifact()))
        b = review_brain_analysis(parse_brain_analysis(make_artifact()))
        self.assertEqual(a, b)

    def test_rendered_text_is_identical_across_runs(self) -> None:
        a = render_brain_review(review_brain_analysis(parse_brain_analysis(make_artifact())))
        b = render_brain_review(review_brain_analysis(parse_brain_analysis(make_artifact())))
        self.assertEqual(a, b)

    def test_ordering_is_independent_of_input_row_order(self) -> None:
        doc = make_artifact()
        shuffled = make_artifact(
            retirement_flags=list(reversed(doc["retirement_flags"])),
            meta_state=list(reversed(doc["meta_state"])),
            setup_classes=list(reversed(doc["setup_classes"])),
        )
        base = review_brain_analysis(parse_brain_analysis(doc))
        other = review_brain_analysis(parse_brain_analysis(shuffled))
        self.assertEqual([n["key"] for n in base["retirement_nominations"]],
                         [n["key"] for n in other["retirement_nominations"]])
        self.assertEqual([m["meta_category"] for m in base["decaying_metas"]],
                         [m["meta_category"] for m in other["decaying_metas"]])


class NominationIsNotRetirementTests(unittest.TestCase):
    def test_disclaimer_wording_names_the_governing_laws(self) -> None:
        self.assertIn("NOMINATION IS NOT A RETIREMENT", NOMINATION_DISCLAIMER)
        for law in ("§56", "§51", "§52"):
            self.assertIn(law, NOMINATION_DISCLAIMER)

    def test_disclaimer_appears_in_the_dict_and_the_rendered_report(self) -> None:
        review = review_brain_analysis(parse_brain_analysis(make_artifact()))
        self.assertEqual(review["nomination_disclaimer"], NOMINATION_DISCLAIMER)
        text = render_brain_review(review)
        self.assertIn(NOMINATION_DISCLAIMER, text)
        self.assertIn(REFUSAL_DISCLAIMER, text)
        self.assertIn("NOMINATED_FOR_REVIEW", text)

    def test_module_documents_that_it_is_an_input_not_a_substitute(self) -> None:
        from supervisor.analysis import brain_review
        doc = brain_review.__doc__ or ""
        self.assertIn("A NOMINATION IS NOT A RETIREMENT", doc)
        self.assertIn("input", doc.lower())
        self.assertIn("never a substitute", doc)
        for law in ("§56", "§51", "§52"):
            self.assertIn(law, doc)


class RefusalReportingTests(unittest.TestCase):
    def test_all_unknown_artifact_reports_refusals_not_zeros(self) -> None:
        review = review_brain_analysis(parse_brain_analysis(all_unknown_artifact()))
        self.assertEqual(review["lens_paying"], [])
        self.assertIsNone(review["best_paying_lens"])
        self.assertEqual(review["retirement_nominations"], [])
        self.assertEqual(review["counts"]["setup_classes_known"], 0)
        self.assertTrue(review["refused"])
        text = render_brain_review(review)
        self.assertIn("every cell is a REFUSAL", text)
        self.assertIn("BEST PAYING: REFUSED", text)
        self.assertIn("estimate=NONE", text)

    def test_no_artifact_is_reported_as_absence_of_evidence(self) -> None:
        review = review_brain_analysis(None)
        self.assertEqual(review["status"], "no_artifact")
        self.assertIn("ABSENCE OF EVIDENCE", review["note"])
        text = render_brain_review(review)
        self.assertIn("NO ARTIFACT", text)
        self.assertIn(NOMINATION_DISCLAIMER, text)

    def test_rendered_report_has_every_governance_section(self) -> None:
        text = render_brain_review(
            review_brain_analysis(parse_brain_analysis(make_artifact())))
        for header in ("RETIREMENT NOMINATIONS", "STYLE LENS, BY VENUE PHASE",
                       "DECAYING METAS", "SOURCES: FOLLOW", "SOURCES: UNFOLLOW",
                       "WHAT THE BRAIN REFUSED TO ANSWER", "SUPPORT INPUTS THE ENGINE LACKS"):
            self.assertIn(header, text)


if __name__ == "__main__":
    unittest.main()
