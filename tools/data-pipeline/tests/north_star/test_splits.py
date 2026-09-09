"""Synthetic boundary fixtures only; none are source/admission evidence."""
import importlib
import json
from pathlib import Path
import sys

import pytest

SRC = Path(__file__).resolve().parents[2] / "src"
sys.path.insert(0, str(SRC))


def api():
    assert (SRC / "north_star" / "splits.py").exists(), "split boundary implementation missing"
    return importlib.import_module("north_star.splits")


def policy():
    return {
        "policy_version": "fixture-v1",
        "reservation_status": "RESERVED",
        "selected_at_utc_ms": 10,
        "source_assignments": {"fixture-dev": "DEVELOPMENT"},
        "exposed_sources": ["fixture-dev"],
        "entity_assignments": {},
        "windows": [{"source_id": "fixture-forward", "start_utc_ms": 100,
                     "end_utc_ms": 200, "collection_started_at_utc_ms": 100,
                     "split": "SEALED_ECONOMICS"}],
        "embargo_ms": {"label_horizon": 5, "episode": 7,
                       "feature": 3, "retrieval": 4},
        "timeless_reference_ids": [],
    }


def boundary(document=None):
    m = api()
    d = document if document is not None else policy()
    return m.EvaluationBoundary(d, expected_sha256=m.canonical_sha256(d))


def test_source_entity_time_assignments_are_outcome_blind():
    d = policy()
    d["entity_assignments"] = {"solana:fixture-mint": "CHALLENGE"}
    b = boundary(d)
    assert b.assign(source_id="fixture-dev", entity_ids=(), event_at_utc_ms=50) == "DEVELOPMENT"
    assert b.assign(source_id="fixture-forward", entity_ids=(), event_at_utc_ms=100) == "SEALED_ECONOMICS"
    assert b.assign(source_id="fixture-forward", entity_ids=(), event_at_utc_ms=200) == "QUARANTINED"
    assert b.assign(source_id="fixture-other", entity_ids=("solana:fixture-mint",), event_at_utc_ms=50) == "QUARANTINED"
    assert b.assign(source_id="fixture-dev", entity_ids=("solana:fixture-mint",), event_at_utc_ms=50) == "QUARANTINED"
    with pytest.raises(TypeError):
        b.assign(source_id="fixture-dev", entity_ids=(), event_at_utc_ms=50, outcome=999)


@pytest.mark.parametrize("time,available", [(None, True), (50, False), (-1, True), (True, True)])
def test_unavailable_or_relative_only_never_joins_chronology(time, available):
    assert boundary().assign(source_id="fixture-dev", entity_ids=(),
                             event_at_utc_ms=time, available=available) == "QUARANTINED"


@pytest.mark.parametrize("split", ["DEVELOPMENT", "SEALED_ECONOMICS", "CHALLENGE"])
def test_unknown_source_cannot_borrow_registered_entity_assignment(split):
    d = policy()
    d["entity_assignments"] = {"known": split}
    b = boundary(d)
    assert b.assign(source_id="unknown", entity_ids=["known"], event_at_utc_ms=50) == "QUARANTINED"
    assert b.assign(source_id="fixture-forward", entity_ids=["known"], event_at_utc_ms=200) == "QUARANTINED"


@pytest.mark.parametrize("parent", ["candidate", "position_episode", "content_version", "duplicate", "retrieval", "cpt_ancestor"])
def test_every_parent_restriction_inherits_without_relabeling(parent):
    b = boundary()
    assert b.inherit({"chosen_mint": "DEVELOPMENT", parent: "SEALED_ECONOMICS"}) == "QUARANTINED"
    with pytest.raises(ValueError, match="parent"):
        b.assert_before_fit(stage="curation", started_at_utc_ms=50,
                            parent_splits={"chosen_mint": "DEVELOPMENT", parent: "CHALLENGE"})
    assert b.inherit({"one": "SEALED_ECONOMICS", "two": "SEALED_ECONOMICS"}) == "SEALED_ECONOMICS"
    assert b.inherit({"one": "DEVELOPMENT", "missing": None}) == "QUARANTINED"
    assert b.inherit({}) == "QUARANTINED"


def test_reference_exemption_requires_exact_frozen_allowlist():
    d = policy()
    d["timeless_reference_ids"] = ["fixture-reference"]
    b = boundary(d)
    assert b.inherit({"empirical": "DEVELOPMENT", "fixture-reference": "TIMELESS_REFERENCE"}) == "DEVELOPMENT"
    assert b.inherit({"empirical": "DEVELOPMENT", "unlisted": "TIMELESS_REFERENCE"}) == "QUARANTINED"
    assert b.inherit({"fixture-reference": "CHALLENGE", "empirical": "DEVELOPMENT"}) == "QUARANTINED"


@pytest.mark.parametrize("stage", ["teacher_selection", "feature_fit", "cluster_fit", "execution_calibration", "curation", "retrieval_fit", "cpt", "sft", "retention"])
def test_fit_requires_reserved_policy_prior_time_and_development_only(stage):
    b = boundary()
    b.assert_before_fit(stage=stage, started_at_utc_ms=50, parent_splits={"fixture": "DEVELOPMENT"})
    with pytest.raises(ValueError, match="before"):
        b.assert_before_fit(stage=stage, started_at_utc_ms=10, parent_splits={"fixture": "DEVELOPMENT"})
    d = policy()
    d.update(reservation_status="POLICY_FROZEN_RESERVATION_PENDING", selected_at_utc_ms=None, windows=[])
    with pytest.raises(ValueError, match="reservation"):
        boundary(d).assert_before_fit(stage=stage, started_at_utc_ms=50, parent_splits={"fixture": "DEVELOPMENT"})
    with pytest.raises(ValueError, match="stage"):
        b.assert_before_fit(stage="typo", started_at_utc_ms=50, parent_splits={"fixture": "DEVELOPMENT"})


def test_horizon_overlap_and_max_dependency_embargo_fail_closed():
    b = boundary()
    assert b.training_interval_allowed(start_utc_ms=30, label_end_utc_ms=92)
    assert not b.training_interval_allowed(start_utc_ms=30, label_end_utc_ms=93)
    assert not b.training_interval_allowed(start_utc_ms=30, label_end_utc_ms=100)
    assert not b.training_interval_allowed(start_utc_ms=150, label_end_utc_ms=160)
    assert not b.training_interval_allowed(start_utc_ms=207, label_end_utc_ms=208)
    assert not b.training_interval_allowed(start_utc_ms=208, label_end_utc_ms=209)
    assert not b.training_interval_allowed(start_utc_ms=30, label_end_utc_ms=None)
    d = policy()
    d["embargo_ms"]["label_horizon"] = None
    assert not boundary(d).training_interval_allowed(start_utc_ms=30, label_end_utc_ms=50)


@pytest.mark.parametrize("mutation", ["late_selection", "exposed_protected", "overlap", "empty_reserved", "bad_split", "bad_time"])
def test_invalid_reservations_rejected(mutation):
    d = policy()
    if mutation == "late_selection":
        d["selected_at_utc_ms"] = 100
    elif mutation == "exposed_protected":
        d["windows"][0]["source_id"] = "fixture-dev"
    elif mutation == "overlap":
        d["windows"].append(dict(d["windows"][0], split="CHALLENGE"))
    elif mutation == "empty_reserved":
        d["windows"] = []
    elif mutation == "bad_split":
        d["source_assignments"]["fixture-dev"] = "TRAIN"
    else:
        d["windows"][0]["start_utc_ms"] = True
    with pytest.raises(ValueError):
        boundary(d)


@pytest.mark.parametrize("collected", [None, 9, 10, True])
def test_selection_must_precede_collection_not_only_window(collected):
    d = policy()
    d["windows"][0]["collection_started_at_utc_ms"] = collected
    with pytest.raises(ValueError, match="collection"):
        boundary(d)


def test_immutable_artifact_binding_rejects_changed_configuration():
    m = api()
    d = policy()
    b = boundary(d)
    receipt = b.bind_artifact(stage="feature_fit", started_at_utc_ms=50,
                             parent_splits={"fixture": "DEVELOPMENT"},
                             artifact_sha256="a" * 64, config_sha256="b" * 64)
    b.verify_artifact(receipt, artifact_sha256="a" * 64, config_sha256="b" * 64)
    for artifact, config in [("c" * 64, "b" * 64), ("a" * 64, "c" * 64)]:
        with pytest.raises(ValueError):
            b.verify_artifact(receipt, artifact_sha256=artifact, config_sha256=config)
    d["policy_version"] = "fixture-v2"
    with pytest.raises(ValueError):
        boundary(d).verify_artifact(receipt, artifact_sha256="a" * 64, config_sha256="b" * 64)
    assert receipt["parents_sha256"] == m.canonical_sha256({"fixture": "DEVELOPMENT"})


@pytest.mark.parametrize("field", ["source_assignments", "entity_assignments"])
@pytest.mark.parametrize("split", ["SEALED_ECONOMICS", "CHALLENGE"])
def test_pending_policy_cannot_claim_protected_assignments(field, split):
    d = policy()
    d.update(reservation_status="POLICY_FROZEN_RESERVATION_PENDING", selected_at_utc_ms=None, windows=[])
    d[field]["unselected"] = split
    with pytest.raises(ValueError, match="pending"):
        boundary(d)


def test_checked_in_freeze_is_honestly_partial():
    root = Path(__file__).resolve().parents[2]
    d = json.loads((root / "north_star" / "EVAL_FREEZE.json").read_text())
    pin = d.pop("policy_sha256")
    b = api().EvaluationBoundary(d, expected_sha256=pin)
    assert d["reservation_status"] == "POLICY_FROZEN_RESERVATION_PENDING"
    assert d["windows"] == [] and d["entity_assignments"] == {}
    assert d["selected_at_utc_ms"] is None
    assert d["exposed_sources"]
    assert all(d["source_assignments"][s] == "DEVELOPMENT" for s in d["exposed_sources"])
    with pytest.raises(ValueError, match="reservation"):
        b.assert_before_fit(stage="feature_fit", started_at_utc_ms=100, parent_splits={"legacy": "DEVELOPMENT"})


@pytest.mark.parametrize("bad", [
    {1: "DEVELOPMENT"}, {"nested": [{False: "DEVELOPMENT"}]},
    {"nested": {None: "DEVELOPMENT"}}, {"nested": ("not", "json")},
])
def test_canonical_hash_rejects_non_json_types_before_serialization(bad):
    with pytest.raises(ValueError, match="JSON"):
        api().canonical_sha256(bad)


def test_numeric_assignment_key_cannot_reuse_string_key_policy_pin():
    d = policy()
    d["entity_assignments"] = {"1": "DEVELOPMENT"}
    pin = api().canonical_sha256(d)
    d["entity_assignments"] = {1: "DEVELOPMENT"}
    with pytest.raises(ValueError, match="JSON"):
        api().EvaluationBoundary(d, expected_sha256=pin)


@pytest.mark.parametrize("path,bad", [
    (("policy_version",), 1), (("policy_version",), " "),
    (("reservation_status",), []), (("selected_at_utc_ms",), True),
    (("source_assignments",), []), (("source_assignments", "fixture-dev"), []),
    (("source_assignments", " "), "DEVELOPMENT"),
    (("entity_assignments",), []), (("entity_assignments", "id"), {}),
    (("exposed_sources",), "fixture-dev"), (("exposed_sources",), [None]),
    (("windows",), {}), (("windows",), [None]),
    (("windows", 0, "source_id"), ["fixture-forward"]),
    (("windows", 0, "source_id"), " "), (("windows", 0, "split"), []),
    (("embargo_ms",), []), (("embargo_ms", "episode"), {}),
    (("timeless_reference_ids",), "prefix-approved-suffix"),
    (("timeless_reference_ids",), {"approved": True}),
    (("timeless_reference_ids",), [None]), (("timeless_reference_ids",), [" "]),
    (("scope",), []), (("scope",), {"chain": [], "venues": ["pumpfun"]}),
    (("scope",), {"chain": "solana", "venues": "pumpfun"}),
    (("scope",), {"chain": "solana", "venues": [[]]}),
    (("materialized_protected_ids",), "id"), (("materialized_protected_ids",), [False]),
    (("future_window_status",), []), (("exposure_status",), {}),
    (("ancestry_status",), 1), (("policy_rules",), []),
    (("policy_rules",), {"assignment": []}), (("policy_sha256",), 1),
])
def test_policy_rejects_malformed_nested_types(path, bad):
    d = policy()
    target = d
    for key in path[:-1]:
        target = target[key]
    target[path[-1]] = bad
    with pytest.raises(ValueError):
        boundary(d)


@pytest.mark.parametrize("bad", [None, [], "policy", 1])
def test_policy_root_requires_object(bad):
    with pytest.raises(ValueError):
        api().EvaluationBoundary(bad, expected_sha256=api().canonical_sha256(bad))


def test_policy_digest_is_read_only_and_receipts_keep_the_verified_pin():
    b = boundary()
    pin = b.sha256
    with pytest.raises(AttributeError):
        b.sha256 = "e" * 64
    assert b.sha256 == pin
    receipt = b.bind_artifact(stage="feature_fit", started_at_utc_ms=50,
                              parent_splits={"fixture": "DEVELOPMENT"},
                              artifact_sha256="a" * 64, config_sha256="b" * 64)
    assert receipt["policy_sha256"] == pin


@pytest.mark.parametrize("bad", [None, "fixture", [None], [{}], {"fixture": True}])
def test_malformed_entity_ids_quarantine(bad):
    assert boundary().assign(source_id="fixture-dev", entity_ids=bad, event_at_utc_ms=50) == "QUARANTINED"


@pytest.mark.parametrize("bad", [None, [], {}, True, " "])
def test_malformed_source_ids_quarantine(bad):
    assert boundary().assign(source_id=bad, entity_ids=[], event_at_utc_ms=50) == "QUARANTINED"


@pytest.mark.parametrize("bad", [None, [], "DEVELOPMENT", {"": "DEVELOPMENT"},
                                 {1: "DEVELOPMENT"}, {"id": []}, {"id": {}}])
def test_malformed_parent_closure_quarantines(bad):
    assert boundary().inherit(bad) == "QUARANTINED"


def test_timeless_ids_require_exact_membership_not_substrings():
    d = policy()
    d["timeless_reference_ids"] = ["prefix-approved-suffix"]
    b = boundary(d)
    assert b.inherit({"dev": "DEVELOPMENT", "approved": "TIMELESS_REFERENCE"}) == "QUARANTINED"
    assert b.inherit({"dev": "DEVELOPMENT", "prefix-approved-suffix": "TIMELESS_REFERENCE"}) == "DEVELOPMENT"


@pytest.mark.parametrize("bad", [None, [], "receipt"])
def test_malformed_receipt_rejected(bad):
    with pytest.raises(ValueError):
        boundary().verify_artifact(bad, artifact_sha256="a" * 64, config_sha256="b" * 64)


@pytest.mark.parametrize("bad", [None, [], {}])
def test_fit_stage_type_is_validated(bad):
    with pytest.raises(ValueError):
        boundary().assert_before_fit(stage=bad, started_at_utc_ms=50,
                                     parent_splits={"dev": "DEVELOPMENT"})


def test_pending_policy_cannot_claim_materialized_protected_ids():
    d = policy()
    d.update(reservation_status="POLICY_FROZEN_RESERVATION_PENDING", selected_at_utc_ms=None,
             windows=[], materialized_protected_ids=["unselected"])
    with pytest.raises(ValueError, match="pending"):
        boundary(d)


def test_s1_artifacts_live_with_the_data_pipeline_not_at_repository_root():
    pipeline = Path(__file__).resolve().parents[2]
    repo = Path(__file__).resolve().parents[4]
    for name in ("EVAL_PROTOCOL.md", "OPERATING_CONTRACT.md", "BUILD_S1_RECEIPT.md", "EVAL_FREEZE.json"):
        assert (pipeline / "north_star" / name).is_file()
        assert not (repo / "north_star" / name).exists()


@pytest.mark.parametrize("start,end", [(201, 220), (1000, 1001), (95, 95)])
def test_training_must_precede_the_earliest_protected_window(start, end):
    assert not boundary().training_interval_allowed(start_utc_ms=start, label_end_utc_ms=end)


def test_training_cannot_use_the_gap_between_protected_windows():
    d = policy()
    d["windows"].append({**d["windows"][0], "source_id": "second",
                         "start_utc_ms": 400, "end_utc_ms": 500, "split": "CHALLENGE"})
    b = boundary(d)
    assert not b.training_interval_allowed(start_utc_ms=250, label_end_utc_ms=260)
    assert b.training_interval_allowed(start_utc_ms=0, label_end_utc_ms=90)
    d["windows"].reverse()
    assert not boundary(d).training_interval_allowed(start_utc_ms=250, label_end_utc_ms=260)


def test_policy_snapshot_is_immutable_and_requires_external_hash():
    m = api()
    d = policy()
    digest = m.canonical_sha256(d)
    b = m.EvaluationBoundary(d, expected_sha256=digest)
    d["policy_version"] = "changed"
    assert b.document["policy_version"] == "fixture-v1"
    view = b.document
    view["windows"].clear()
    assert b.document["windows"]
    with pytest.raises(ValueError, match="hash"):
        m.EvaluationBoundary(d, expected_sha256=digest)
    assert m.canonical_sha256(dict(reversed(list(policy().items())))) == digest
