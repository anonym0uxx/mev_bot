"""Unit tests for brain-grounded hypothesis generation in the research loop.

Stdlib unittest; run with `python3 -m unittest discover -s supervisor/tests`.

Three things are pinned here:

  1. The deterministic (model-free) hypotheses are reproducible, their impacts come from
     OBSERVED quantities, and their evidence_refs resolve in the evidence store (§68/§111).
  2. THE LOAD-BEARING PROPERTY: no null is ever coerced to 0 anywhere in the chain. Fed an
     artifact in which the brain refused everything, no hypothesis claims a numeric impact and
     no persisted row carries a fabricated zero.
  3. The brain is an ENHANCEMENT, not a dependency: with no artifact — or an unparseable one —
     the loop runs exactly as it did before brain grounding existed.
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

from supervisor.research.loop import (
    BRAIN_GROUNDED_LABEL, BRAIN_MIN_N_SETUP_CLASS, Hypothesis, NO_IMPACT_ESTIMATE_LABEL,
    ResearchAdapters, ResearchLoop, _brain_grounded_hypotheses, _median_int, _prior_from_n,
    brain_evidence_digest,
)
from supervisor.store.brain_analysis import parse_brain_analysis
from supervisor.store.evidence import EvidenceStore
from supervisor.tests.test_brain_analysis_loader import (
    BIG_SIG, all_unknown_artifact, make_artifact, write_artifact,
)


class _FakeModel:
    """Duck-typed ModelClient: records the prompts it was handed, returns a fixed hypothesis."""

    def __init__(self, obj: dict | None = None) -> None:
        self.calls: list[tuple[str, str]] = []
        self.obj = obj or {
            "hypothesis_id": "MODEL-H1",
            "statement": "Tighten the entry filter.",
            "causal_mechanism": "Late entries pay the move.",
            "expected_net_sol_impact": 1.5,
            "prior_probability": 0.4,
            "cost_to_test": "medium",
            "edge_half_life": "days",
            "competing_explanations": ["noise"],
            "disconfirming_evidence_sought": ["no improvement in replay"],
        }

    def constrained(self, system: str, user: str, schema: dict) -> dict:
        self.calls.append((system, user))
        return dict(self.obj)


class _FakeEscalate:
    def __init__(self) -> None:
        self.raised: list = []

    def raise_escalation(self, e) -> None:  # noqa: ANN001
        self.raised.append(e)


def _adapters(brain_path=None, passed: bool = False) -> ResearchAdapters:
    return ResearchAdapters(
        ingest_reconciled_outcomes=lambda: {"trades": 10, "net_lamports": -5000},
        seal_and_run_experiment=lambda spec: {"passed": passed},
        evaluator_hash=lambda: "HASH",
        expected_evaluator_hash="HASH",
        brain_analysis_path=brain_path,
    )


class DeterministicHypothesisTests(unittest.TestCase):
    def setUp(self) -> None:
        self.analysis = parse_brain_analysis(make_artifact())
        self.hyps = _brain_grounded_hypotheses(self.analysis)

    def test_no_artifact_yields_no_hypotheses(self) -> None:
        self.assertEqual(_brain_grounded_hypotheses(None), [])

    def test_rules_fire_for_each_evidence_kind(self) -> None:
        ids = [h.hypothesis_id for h in self.hyps]
        self.assertTrue(any(i.startswith("BRAIN-4210-SETUPCLASS-") for i in ids), ids)
        self.assertTrue(any(i.startswith("BRAIN-4210-META-") for i in ids), ids)
        self.assertTrue(any(i.startswith("BRAIN-4210-SOURCE-") for i in ids), ids)
        self.assertTrue(any(i.startswith("BRAIN-4210-RETIRE-") for i in ids), ids)
        self.assertTrue(any(i.startswith("BRAIN-4210-VOI-SETUPCLASS-") for i in ids), ids)

    def test_output_is_deterministic_across_calls(self) -> None:
        again = _brain_grounded_hypotheses(parse_brain_analysis(make_artifact()))
        self.assertEqual([h.hypothesis_id for h in self.hyps],
                         [h.hypothesis_id for h in again])
        self.assertEqual([h.expected_net_sol_impact for h in self.hyps],
                         [h.expected_net_sol_impact for h in again])
        self.assertEqual([h.to_record() for h in self.hyps], [h.to_record() for h in again])

    def test_every_hypothesis_carries_tick_and_row_key_in_its_evidence_ref(self) -> None:
        for h in self.hyps:
            self.assertTrue(h.evidence_ref.startswith("brain"), h.evidence_ref)
            self.assertIn("4210", h.evidence_ref)
            self.assertIn("/", h.evidence_ref)
            self.assertIn(BRAIN_GROUNDED_LABEL, h.labels)
            self.assertTrue(h.impact_basis)

    def test_negative_setup_class_impact_is_observed_median_times_n(self) -> None:
        h = next(h for h in self.hyps if h.evidence_ref == f"brain_setup:4210/{BIG_SIG}/pool")
        # the fixture row: median -2_000_000 lamports over n=12
        self.assertAlmostEqual(h.expected_net_sol_impact, 2_000_000 * 12 / 1e9)
        self.assertEqual(h.prior_probability, _prior_from_n(12))
        self.assertIn("median_net_lamports(-2000000)", h.impact_basis)
        self.assertIn("n(12)", h.impact_basis)

    def test_profitable_setup_class_produces_no_exclusion_hypothesis(self) -> None:
        refs = [h.evidence_ref for h in self.hyps]
        self.assertNotIn("brain_setup:4210/33333333333333333333/pool", refs)

    def test_thin_setup_class_below_min_n_is_not_a_finding(self) -> None:
        thin = make_artifact()
        thin["setup_classes"] = [dict(thin["setup_classes"][0], n=BRAIN_MIN_N_SETUP_CLASS - 1)]
        hyps = _brain_grounded_hypotheses(parse_brain_analysis(thin))
        self.assertEqual([h for h in hyps if h.hypothesis_id.startswith("BRAIN-4210-SETUP")], [])

    def test_unfollow_impact_is_observed_attributed_net(self) -> None:
        h = next(h for h in self.hyps if h.evidence_ref == "brain_caller:4210/5003")
        self.assertAlmostEqual(h.expected_net_sol_impact, 1_900_000 / 1e9)
        self.assertEqual(h.cost_to_test, "none")

    def test_retirement_hypothesis_says_a_nomination_is_not_a_retirement(self) -> None:
        h = next(h for h in self.hyps if h.hypothesis_id.startswith("BRAIN-4210-RETIRE-"))
        self.assertIn("NOMINATION IS NOT A RETIREMENT", h.statement)
        self.assertIn("§51", h.statement)
        self.assertIn("§52", h.statement)

    def test_decaying_meta_impact_is_attributed_from_conditioned_classes_only(self) -> None:
        h = next(h for h in self.hyps if h.evidence_ref == "brain_meta:4210/7")
        # meta 7 has exactly one conditioned negative class: median -2_000_000 over n=12
        self.assertAlmostEqual(h.expected_net_sol_impact, 2_000_000 * 12 / 1e9)
        self.assertTrue(h.impact_basis.startswith("observed:"))

    def test_decaying_meta_without_conditioned_classes_estimates_nothing(self) -> None:
        h = next(h for h in self.hyps if h.evidence_ref == "brain_meta:4210/9")
        self.assertEqual(h.expected_net_sol_impact, 0.0)
        self.assertIn(NO_IMPACT_ESTIMATE_LABEL, h.labels)
        self.assertTrue(h.impact_basis.startswith("none:"))
        self.assertIn("No net-SOL impact is estimated", h.statement)

    def test_output_is_ordered_by_descending_voi(self) -> None:
        scores = [h.voi_score() for h in self.hyps]
        self.assertEqual(scores, sorted(scores, reverse=True))

    def test_prior_mapping_is_monotone_in_observed_n(self) -> None:
        priors = [_prior_from_n(n) for n in (4, 8, 16, 32, 64, 512)]
        self.assertEqual(priors, sorted(priors))
        self.assertEqual(_prior_from_n(64), 0.80)
        self.assertEqual(_prior_from_n(8), 0.50)

    def test_median_int_has_no_median_of_nothing(self) -> None:
        self.assertIsNone(_median_int([]))
        self.assertEqual(_median_int([3, 1, 2]), 2)
        self.assertEqual(_median_int([4, 1, 2, 3]), 2)   # lower-middle, no float averaging


class ValueOfInformationTests(unittest.TestCase):
    def test_refusal_hypothesis_prices_information_from_conditioned_peers(self) -> None:
        a = parse_brain_analysis(make_artifact())
        hyps = _brain_grounded_hypotheses(a)
        voi = [h for h in hyps if h.hypothesis_id.startswith("BRAIN-4210-VOI-")]
        self.assertEqual(len(voi), 2)
        # peers: |−180000|, |−2000000|, |45000| -> median 180000
        expected = 180_000 * BRAIN_MIN_N_SETUP_CLASS / 1e9
        for h in voi:
            self.assertAlmostEqual(h.expected_net_sol_impact, expected)
            self.assertEqual(h.prior_probability, 0.50)     # maximum entropy: genuine unknown
            self.assertEqual(h.cost_to_test, "none")
            self.assertEqual(h.edge_half_life, "unknown")
            self.assertIn("information value", h.impact_basis)
            self.assertIn("NO claim about this cell", h.impact_basis)
            self.assertIn("UNMEASURED", h.statement)

    def test_low_cost_is_what_makes_a_refusal_rank_high(self) -> None:
        """Same impact, same prior: the cheap experiment outranks the costly one 10:1."""
        cheap = Hypothesis("A", "s", "m", 1.0, 0.5, "none", "unknown")
        costly = Hypothesis("B", "s", "m", 1.0, 0.5, "low", "unknown")
        self.assertGreater(cheap.voi_score(), costly.voi_score())
        self.assertAlmostEqual(cheap.voi_score(), costly.voi_score() * 10.0)

    def test_a_cheap_refusal_probe_outranks_a_larger_but_costlier_finding(self) -> None:
        """The VOI must come from cheapness + genuine uncertainty, not an inflated impact."""
        hyps = _brain_grounded_hypotheses(parse_brain_analysis(make_artifact()))
        probe = next(h for h in hyps if h.hypothesis_id.startswith("BRAIN-4210-VOI-"))
        finding = next(
            h for h in hyps
            if h.evidence_ref == "brain_setup:4210/11111111111111111111/curve")
        # The finding's OBSERVED impact is five times the probe's information value ...
        self.assertGreater(finding.expected_net_sol_impact,
                           probe.expected_net_sol_impact * 4)
        # ... and yet the probe ranks ahead of it, purely on cost and honest uncertainty.
        self.assertGreater(probe.voi_score(), finding.voi_score())
        self.assertLess(hyps.index(probe), hyps.index(finding))


class NoNullEverBecomesZeroTests(unittest.TestCase):
    """THE load-bearing test: an artifact of pure refusals must produce no fabricated number."""

    def setUp(self) -> None:
        self.analysis = parse_brain_analysis(all_unknown_artifact())
        self.hyps = _brain_grounded_hypotheses(self.analysis)

    def test_some_hypotheses_are_still_produced(self) -> None:
        # refusals are information: they must still drive a research agenda
        self.assertTrue(self.hyps)
        for h in self.hyps:
            self.assertTrue(h.hypothesis_id.startswith("BRAIN-4210-VOI-"), h.hypothesis_id)

    def test_no_hypothesis_claims_a_numeric_impact(self) -> None:
        for h in self.hyps:
            self.assertEqual(h.expected_net_sol_impact, 0.0)
            self.assertIn(NO_IMPACT_ESTIMATE_LABEL, h.labels,
                          "a declined estimate must be labelled as declined, not read as 0")
            self.assertTrue(h.impact_basis.startswith("none:"), h.impact_basis)
            self.assertIn("No impact is estimated", h.statement)

    def test_no_hypothesis_quotes_a_lamport_figure(self) -> None:
        for h in self.hyps:
            self.assertNotIn("median_net=", h.statement)
            self.assertNotIn("lamports", h.statement)

    def test_digest_prints_refusals_as_refusals_never_as_zero(self) -> None:
        d = brain_evidence_digest(self.analysis)
        self.assertIn("n=REFUSED", d)
        self.assertIn("median_net=REFUSED", d)
        self.assertIn("REFUSED TO ANSWER", d)
        self.assertNotIn("n=0 ", d)
        self.assertIn("A REFUSED value is a refusal", d)

    def test_no_persisted_row_carries_a_fabricated_zero(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            store = EvidenceStore(Path(td) / "e.db")
            try:
                store.ingest_brain_analysis("RUN1", self.analysis)
                for r in store.list_brain_rows("brain_setup_classes", "RUN1"):
                    for col in ("n", "median_net_lamports", "mean_net_lamports", "win_rate_bp",
                                "p25_net_lamports", "p75_net_lamports", "median_hold_ns",
                                "nearest_distance"):
                        self.assertIsNone(r[col], f"{col} was fabricated as {r[col]!r}")
                for r in store.list_brain_rows("brain_caller_trust", "RUN1"):
                    self.assertIsNone(r["score_bp"])
                    self.assertIsNone(r["n_markouts"])
                # and the hypotheses persisted from these refusals are labelled as estimate-free
                for h in self.hyps:
                    store.record_hypothesis(h.to_record(), created_run="RUN1")
                    got = store.get_hypothesis(h.hypothesis_id)
                    assert got is not None
                    self.assertEqual(got["expected_net_sol_impact"], 0.0)
                    self.assertIn(NO_IMPACT_ESTIMATE_LABEL, got["labels"])
            finally:
                store.close()


class DigestTests(unittest.TestCase):
    def test_digest_is_deterministic(self) -> None:
        a = parse_brain_analysis(make_artifact())
        b = parse_brain_analysis(make_artifact())
        self.assertEqual(brain_evidence_digest(a), brain_evidence_digest(b))

    def test_digest_states_findings_and_refusals(self) -> None:
        d = brain_evidence_digest(parse_brain_analysis(make_artifact()))
        self.assertIn("CONDITIONED SETUP CLASSES", d)
        self.assertIn("LENS SCOREBOARD", d)
        self.assertIn("META STATE", d)
        self.assertIn("RETIREMENT NOMINATIONS", d)
        self.assertIn("WHAT THE BRAIN REFUSED TO ANSWER", d)
        self.assertIn("insufficient_sample", d)
        self.assertIn("no_candidate_in_radius", d)
        self.assertIn("empty_index", d)
        self.assertIn("NOT zero", d)
        self.assertIn("buys the most information", d)

    def test_digest_without_an_artifact_forbids_speculation(self) -> None:
        d = brain_evidence_digest(None)
        self.assertIn("UNAVAILABLE", d)
        self.assertIn("do not speculate", d)

    def test_digest_reaches_the_model_prompt(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            p = write_artifact(td, make_artifact())
            store = EvidenceStore(Path(td) / "e.db")
            model = _FakeModel()
            try:
                loop = ResearchLoop(model, store, _FakeEscalate(), _adapters(p), "RUN1")
                loop.cycle()
            finally:
                store.close()
            self.assertEqual(len(model.calls), 1)
            system, user = model.calls[0]
            self.assertIn("refusal, not a zero", system)
            self.assertIn("BRAIN EVIDENCE DIGEST", user)
            self.assertIn("Reconciled outcomes summary", user)


class LoopIntegrationTests(unittest.TestCase):
    def setUp(self) -> None:
        self._td = tempfile.TemporaryDirectory()
        self.dir = Path(self._td.name)
        self.store = EvidenceStore(self.dir / "e.db")

    def tearDown(self) -> None:
        self.store.close()
        self._td.cleanup()

    def test_cycle_without_a_brain_path_behaves_exactly_as_before(self) -> None:
        model = _FakeModel()
        loop = ResearchLoop(model, self.store, _FakeEscalate(), _adapters(None), "RUN1")
        self.assertIsNone(loop.cycle())
        # the model hypothesis is still recorded and marked rejected by the failing experiment
        got = self.store.get_hypothesis("MODEL-H1")
        assert got is not None
        self.assertEqual(got["inference_state"], "RejectedInference")
        self.assertEqual(got["evidence_ref"], "")
        self.assertIsNone(self.store.latest_brain_snapshot("RUN1"))
        self.assertIn("UNAVAILABLE", model.calls[0][1])

    def test_cycle_with_an_unparseable_artifact_degrades_to_the_no_brain_path(self) -> None:
        bad = self.dir / "brain_analysis.json"
        bad.write_text(json.dumps(make_artifact())[:120], encoding="utf-8")
        model = _FakeModel()
        loop = ResearchLoop(model, self.store, _FakeEscalate(), _adapters(bad), "RUN1")
        with self.assertLogs("supervisor.store.brain_analysis", level="ERROR"):
            self.assertIsNone(loop.cycle())
        self.assertIsNone(self.store.latest_brain_snapshot("RUN1"))
        self.assertIsNotNone(self.store.get_hypothesis("MODEL-H1"))

    def test_cycle_with_a_newer_schema_artifact_degrades_to_the_no_brain_path(self) -> None:
        p = write_artifact(self.dir, make_artifact(schema_version=99))
        model = _FakeModel()
        loop = ResearchLoop(model, self.store, _FakeEscalate(), _adapters(p), "RUN1")
        with self.assertLogs("supervisor.store.brain_analysis", level="ERROR"):
            loop.cycle()
        self.assertIsNone(self.store.latest_brain_snapshot("RUN1"))

    def test_cycle_with_an_artifact_persists_evidence_and_resolvable_refs(self) -> None:
        p = write_artifact(self.dir, make_artifact())
        loop = ResearchLoop(_FakeModel(), self.store, _FakeEscalate(), _adapters(p), "RUN1")
        loop.cycle()
        snap = self.store.latest_brain_snapshot("RUN1")
        assert snap is not None
        self.assertEqual(snap["tick"], 4210)
        grounded = _brain_grounded_hypotheses(parse_brain_analysis(make_artifact()))
        self.assertTrue(grounded)
        for h in grounded:
            stored = self.store.get_hypothesis(h.hypothesis_id)
            self.assertIsNotNone(stored, h.hypothesis_id)
            assert stored is not None
            self.assertEqual(stored["evidence_ref"], h.evidence_ref)
            self.assertTrue(self.store.evidence_ref_resolves(h.evidence_ref),
                            f"evidence_ref does not resolve: {h.evidence_ref}")

    def test_brain_hypotheses_compete_in_the_voi_queue_and_can_win(self) -> None:
        """A grounded hypothesis is ranked against the model's on the same VOI scale."""
        p = write_artifact(self.dir, make_artifact())
        seen: list[dict] = []

        def runner(spec: dict) -> dict:
            seen.append(spec)
            return {"passed": False}

        adapters = ResearchAdapters(
            ingest_reconciled_outcomes=lambda: {"trades": 1},
            seal_and_run_experiment=runner,
            evaluator_hash=lambda: "HASH",
            expected_evaluator_hash="HASH",
            brain_analysis_path=p,
        )
        # a model proposal with a weak expected impact must lose to observed evidence
        weak = _FakeModel()
        weak.obj["expected_net_sol_impact"] = 0.0001
        loop = ResearchLoop(weak, self.store, _FakeEscalate(), adapters, "RUN1")
        loop.cycle()
        self.assertEqual(len(seen), 1)
        self.assertEqual(seen[0]["hypothesis_id"], "BRAIN-4210-META-7", seen[0])

        # ... and a strong model proposal still wins, so grounding does not hijack the queue
        seen.clear()
        strong = _FakeModel()
        loop = ResearchLoop(strong, self.store, _FakeEscalate(), adapters, "RUN2")
        loop.cycle()
        self.assertEqual(seen[0]["hypothesis_id"], "MODEL-H1")

    def test_reingest_across_two_cycles_is_idempotent(self) -> None:
        p = write_artifact(self.dir, make_artifact())
        loop = ResearchLoop(_FakeModel(), self.store, _FakeEscalate(), _adapters(p), "RUN1")
        loop.cycle()
        first = self.store.list_brain_rows("brain_setup_classes", "RUN1")
        loop.cycle()
        self.assertEqual(first, self.store.list_brain_rows("brain_setup_classes", "RUN1"))


if __name__ == "__main__":
    unittest.main()
