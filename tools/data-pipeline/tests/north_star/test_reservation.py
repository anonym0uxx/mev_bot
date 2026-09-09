"""Synthetic routing fixtures, never sealed-capture or source approval evidence."""
import importlib
import json
from pathlib import Path
import sys

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "src"))
from north_star.splits import EvaluationBoundary, canonical_sha256


def api():
    assert (ROOT / "src/north_star/reservation.py").exists(), "reservation implementation missing"
    return importlib.import_module("north_star.reservation")


def freeze():
    document = json.loads((ROOT / "north_star/EVAL_FREEZE.json").read_text())
    pin = document.pop("policy_sha256")
    return EvaluationBoundary(document, expected_sha256=pin)


def policy():
    return {
        "policy_version": "north_star_prospective_reservation_v3",
        "reservation_status": "PROSPECTIVE_COLLECTION_RESERVED_FIT_BLOCKED",
        "selected_at_utc_ms": 10,
        "base_policy_sha256": freeze().sha256,
        "window": {"start_utc_ms": 100, "end_utc_ms": 604800100,
                   "split": "SEALED_ECONOMICS", "source_scope": "ALL_FUTURE_SUPPORTED_CAPTURE_SOURCES"},
        "collection_authorized": False,
        "fit_authorized": False,
        "materialized_protected_ids": [],
        "dependency_horizons_status": "UNKNOWN",
        "collector_custody_status": "DEPLOYMENT_PENDING",
    }


def reservation(document=None):
    d = policy() if document is None else document
    return api().ProspectiveReservation(d, expected_sha256=canonical_sha256(d), base_boundary=freeze())


def test_policy_is_pinned_snapshot_and_does_not_mutate_pending_freeze():
    original = (ROOT / "north_star/EVAL_FREEZE.json").read_bytes()
    d = policy()
    r = reservation(d)
    pin = canonical_sha256(d)
    d["window"]["start_utc_ms"] = 11
    assert r.sha256 == pin
    copy = r.document
    copy["window"]["end_utc_ms"] = 12
    assert r.document["window"] == policy()["window"]
    with pytest.raises(ValueError, match="hash"):
        api().ProspectiveReservation(d, expected_sha256=pin, base_boundary=freeze())
    assert (ROOT / "north_star/EVAL_FREEZE.json").read_bytes() == original
    assert freeze().document["reservation_status"] == "POLICY_FROZEN_RESERVATION_PENDING"


@pytest.mark.parametrize("field,value", [
    ("selected_at_utc_ms", None), ("selected_at_utc_ms", True),
    ("selected_at_utc_ms", 100), ("selected_at_utc_ms", -1),
    ("base_policy_sha256", "0" * 64), ("policy_version", "unknown"),
    ("collection_authorized", True), ("fit_authorized", True),
    ("materialized_protected_ids", ["invented-future-source"]),
    ("dependency_horizons_status", "READY"), ("collector_custody_status", "DEPLOYED"),
    ("reservation_status", "RESERVED"),
])
def test_invalid_or_authority_escalating_policy_rejected(field, value):
    d = policy()
    d[field] = value
    with pytest.raises(ValueError):
        reservation(d)


@pytest.mark.parametrize("change", [
    {"start_utc_ms": True}, {"start_utc_ms": 10}, {"end_utc_ms": None},
    {"end_utc_ms": 200}, {"split": "DEVELOPMENT"},
    {"source_scope": "specific-provider"}, {"source_id": "invented-future-source"},
])
def test_window_is_one_finite_future_source_neutral_week(change):
    d = policy()
    d["window"].update(change)
    with pytest.raises(ValueError):
        reservation(d)


def metadata(**changes):
    d = dict(source_id="fixture-future-a", entity_ids=(), event_at_utc_ms=110,
             available_at_utc_ms=111, captured_at_utc_ms=112,
             collection_started_at_utc_ms=101, supported_capture=True,
             previously_exposed=False)
    d.update(changes)
    return d


@pytest.mark.parametrize("source", ["fixture-future-a", "fixture-future-b", "fixture-future-c"])
@pytest.mark.parametrize("time,expected", [(99, "QUARANTINED"), (100, "SEALED_ECONOMICS"),
    (604800099, "SEALED_ECONOMICS"), (604800100, "QUARANTINED"), (604800101, "QUARANTINED")])
def test_source_neutral_half_open_before_within_after(source, time, expected):
    assert reservation().assign(**metadata(source_id=source, event_at_utc_ms=time,
        available_at_utc_ms=time, captured_at_utc_ms=time,
        collection_started_at_utc_ms=50)) == expected


@pytest.mark.parametrize("field", ["event_at_utc_ms", "available_at_utc_ms", "captured_at_utc_ms",
                                  "collection_started_at_utc_ms"])
@pytest.mark.parametrize("value", [None, True, -1, "110", 110.0])
def test_every_required_clock_is_exact_known_absolute_utc(field, value):
    assert reservation().assign(**metadata(**{field: value})) == "QUARANTINED"


@pytest.mark.parametrize("changes", [
    {"event_at_utc_ms": 99}, {"available_at_utc_ms": 604800100},
    {"captured_at_utc_ms": 604800100}, {"available_at_utc_ms": 109},
    {"captured_at_utc_ms": 110}, {"collection_started_at_utc_ms": 113},
    {"collection_started_at_utc_ms": 10}, {"collection_started_at_utc_ms": 9},
    {"supported_capture": False}, {"supported_capture": 1},
    {"previously_exposed": None}, {"previously_exposed": True},
    {"source_id": ""}, {"source_id": None}, {"entity_ids": "mint"},
])
def test_uncertain_or_conflicting_metadata_quarantines(changes):
    assert reservation().assign(**metadata(**changes)) == "QUARANTINED"


@pytest.mark.parametrize("source", freeze().document["exposed_sources"])
def test_registered_exposure_cannot_be_laundered_by_caller(source):
    assert reservation().assign(**metadata(source_id=source)) == "QUARANTINED"
    # Existing v2 continues classifying observed history as exposed DEVELOPMENT.
    assert freeze().assign(source_id=source, entity_ids=(), event_at_utc_ms=50) == "DEVELOPMENT"


@pytest.mark.parametrize("interval,expected", [((100, 150), "SEALED_ECONOMICS"),
    ((99, 150), "QUARANTINED"), ((100, 604800100), "QUARANTINED"),
    ((99, 604800101), "QUARANTINED"), ((150, 100), "QUARANTINED"),
    ((None, 150), "QUARANTINED"), ((None, None), "QUARANTINED")])
def test_explicit_interval_bounds_must_be_known_and_not_span_boundary(interval, expected):
    assert reservation().assign(**metadata(), interval_start_utc_ms=interval[0],
                                interval_end_utc_ms=interval[1]) == expected


@pytest.mark.parametrize("source", freeze().document["exposed_sources"])
@pytest.mark.parametrize("depth", [0, 1, 3])
@pytest.mark.parametrize("claimed", ["SEALED_ECONOMICS", "DEVELOPMENT", "CHALLENGE",
                                     "TIMELESS_REFERENCE", None])
def test_inherit_known_exposed_identity_quarantines_at_every_depth(source, depth, claimed):
    graph, splits, root = lineage(source, depth, claimed)
    assert reservation().inherit(root=root, graph=graph, node_splits=splits) == "QUARANTINED"


def lineage(identity, depth, split):
    graph = {identity: []}
    root = identity
    for index in range(depth):
        parent = root
        root = f"fixture-artifact:{index}"
        graph[root] = [parent]
    return graph, {node: split for node in graph}, root


def reservation_with_base_assignments(**changes):
    # Independent synthetic base; never edits the pinned pending base file.
    base = freeze().document
    base.update(reservation_status="RESERVED", selected_at_utc_ms=10,
                windows=[{"source_id": "fixture-window", "start_utc_ms": 100,
                          "end_utc_ms": 200, "collection_started_at_utc_ms": 50,
                          "split": "SEALED_ECONOMICS"}])
    base.update(changes)
    boundary = EvaluationBoundary(base, expected_sha256=canonical_sha256(base))
    d = policy()
    d["base_policy_sha256"] = boundary.sha256
    return api().ProspectiveReservation(d, expected_sha256=canonical_sha256(d),
                                       base_boundary=boundary)


@pytest.mark.parametrize("field", ["source_assignments", "entity_assignments"])
@pytest.mark.parametrize("assigned", ["DEVELOPMENT", "SEALED_ECONOMICS", "CHALLENGE", "QUARANTINED"])
@pytest.mark.parametrize("claimed", ["DEVELOPMENT", "SEALED_ECONOMICS", "CHALLENGE",
                                     "QUARANTINED", "TIMELESS_REFERENCE", None])
@pytest.mark.parametrize("depth", [0, 3])
def test_inherit_reconciles_exact_base_assignments(field, assigned, claimed, depth):
    identity = "fixture-registered-identity"
    assignments = freeze().document[field]
    assignments[identity] = assigned
    r = reservation_with_base_assignments(**{field: assignments},
                                          timeless_reference_ids=[identity])
    graph, splits, root = lineage(identity, depth, claimed)
    expected = assigned if assigned == claimed else "QUARANTINED"
    assert r.inherit(root=root, graph=graph, node_splits=splits) == expected


@pytest.mark.parametrize("field", ["source_assignments", "entity_assignments"])
@pytest.mark.parametrize("assigned", ["DEVELOPMENT", "SEALED_ECONOMICS", "CHALLENGE", "QUARANTINED"])
def test_assign_still_respects_exact_base_restrictions(field, assigned):
    identity = "fixture-restricted-source-or-entity"
    assignments = freeze().document[field]
    assignments[identity] = assigned
    r = reservation_with_base_assignments(**{field: assignments})
    changes = {"source_id": identity} if field == "source_assignments" else {"entity_ids": [identity]}
    expected = "SEALED_ECONOMICS" if assigned == "SEALED_ECONOMICS" else "QUARANTINED"
    assert r.assign(**metadata(**changes)) == expected


@pytest.mark.parametrize("depth", [0, 3])
def test_inherit_reconciles_source_and_entity_restrictions_without_key_collisions(depth):
    identity = "fixture-shared-identity"
    assignments = freeze().document["source_assignments"]
    assignments[identity] = "SEALED_ECONOMICS"
    r = reservation_with_base_assignments(source_assignments=assignments,
        entity_assignments={identity: "DEVELOPMENT"})
    graph, splits, root = lineage(identity, depth, "SEALED_ECONOMICS")
    assert r.inherit(root=root, graph=graph, node_splits=splits) == "QUARANTINED"


def test_inherit_unknown_artifact_identity_is_not_unknown_source_admission():
    r = reservation()
    # No source clocks, support or exposure evidence: a source cannot be routed.
    assert r.assign(source_id="fixture-unknown-source", entity_ids=()) == "QUARANTINED"
    # An artifact need not be a registered source. This only propagates declared
    # restrictions, not certification of its source metadata or completeness.
    graph = {"fixture-artifact": ["fixture-record"], "fixture-record": []}
    splits = {node: r.assign(**metadata()) for node in graph}
    assert r.inherit(root="fixture-artifact", graph=graph, node_splits=splits) == "SEALED_ECONOMICS"
    splits["fixture-record"] = None
    assert r.inherit(root="fixture-artifact", graph=graph, node_splits=splits) == "QUARANTINED"


def test_inherit_preserves_registered_timeless_references_and_ignores_unrelated_nodes():
    r = reservation_with_base_assignments(timeless_reference_ids=["fixture-reference"])
    exposed = freeze().document["exposed_sources"][0]
    graph = {"fixture-artifact": ["fixture-reference"], "fixture-reference": [], exposed: []}
    splits = {"fixture-artifact": "SEALED_ECONOMICS", "fixture-reference": "TIMELESS_REFERENCE",
              exposed: "SEALED_ECONOMICS"}
    assert r.inherit(root="fixture-artifact", graph=graph, node_splits=splits) == "SEALED_ECONOMICS"
    graph["fixture-artifact"].append(exposed)
    assert r.inherit(root="fixture-artifact", graph=graph, node_splits=splits) == "QUARANTINED"


def test_routed_protected_record_blocks_transitive_development_context():
    r = reservation()
    protected = r.assign(**metadata())
    graph = {"dev-context": ["candidate"], "candidate": ["future-capture"], "future-capture": []}
    assert r.inherit(root="dev-context", graph=graph, node_splits={
        "dev-context": "DEVELOPMENT", "candidate": "DEVELOPMENT",
        "future-capture": protected}) == "QUARANTINED"


def test_missing_clock_arguments_default_to_quarantine_not_fabricated_time():
    assert reservation().assign(source_id="fixture", entity_ids=()) == "QUARANTINED"


def test_reservation_metadata_operations_do_not_write_or_connect(monkeypatch):
    import socket
    r = reservation()

    def forbidden(*args, **kwargs):
        pytest.fail("reservation attempted file/network side effect")

    monkeypatch.setattr("builtins.open", forbidden)
    monkeypatch.setattr(Path, "open", forbidden)
    monkeypatch.setattr(socket, "socket", forbidden)
    assert r.assign(**metadata()) == "SEALED_ECONOMICS"
    assert r.inherit(root="fixture", graph={"fixture": []},
                     node_splits={"fixture": "SEALED_ECONOMICS"}) == "SEALED_ECONOMICS"


def test_no_outcome_or_source_approval_arguments():
    with pytest.raises(TypeError):
        reservation().assign(**metadata(), outcome=1)
    with pytest.raises(TypeError):
        reservation().assign(**metadata(), source_approved=True)
    assert reservation().document["collection_authorized"] is False


@pytest.mark.parametrize("ancestor", ["SEALED_ECONOMICS", "CHALLENGE", None])
def test_transitive_protected_parent_conflicts_via_existing_dependency_closure(ancestor):
    graph = {"root": ["candidate"], "candidate": ["retrieval"], "retrieval": []}
    node_splits = {"root": "DEVELOPMENT", "candidate": "DEVELOPMENT", "retrieval": ancestor}
    assert reservation().inherit(root="root", graph=graph, node_splits=node_splits) == "QUARANTINED"


def test_unanimous_protection_inherits_and_missing_cycle_or_root_cannot_pass():
    r = reservation()
    graph = {"root": ["parent"], "parent": []}
    splits = {"root": "SEALED_ECONOMICS", "parent": "SEALED_ECONOMICS"}
    assert r.inherit(root="root", graph=graph, node_splits=splits) == "SEALED_ECONOMICS"
    for broken in ({"root": ["absent"]}, {"root": ["root"]}, {}):
        assert r.inherit(root="root", graph=broken, node_splits=splits) == "QUARANTINED"
    assert r.inherit(root="root", graph=graph, node_splits={"parent": "SEALED_ECONOMICS"}) == "QUARANTINED"


@pytest.mark.parametrize("stage", ["teacher_selection", "feature_fit", "cluster_fit", "execution_calibration",
    "curation", "retrieval_fit", "cpt", "sft", "retention"])
def test_reservation_never_overrides_freeze_pending_fit_gate(stage):
    r = reservation()
    with pytest.raises(ValueError, match="reservation pending"):
        r.assert_before_fit(stage=stage, started_at_utc_ms=50, parent_splits={"dev": "DEVELOPMENT"})
    assert not r.training_interval_allowed(start_utc_ms=20, label_end_utc_ms=30)


def test_checked_in_reservation_is_fixed_prospective_and_not_capture_evidence():
    from datetime import datetime, timezone
    from zoneinfo import ZoneInfo
    import hashlib

    path = ROOT / "north_star/EVAL_RESERVATION_V3.json"
    assert path.exists(), "preregistered reservation missing"
    d = json.loads(path.read_text())
    pin = d.pop("reservation_sha256")
    # Corrected precommit provenance pin; supersession trail is in the receipt.
    assert pin == "8d018de9c530b97595d5a3ec9c69d92df497be16c882df12662398b6a8476fb8"
    r = api().ProspectiveReservation(d, expected_sha256=pin, base_boundary=freeze())
    start = datetime(2026, 9, 16, tzinfo=ZoneInfo("America/Los_Angeles"))
    end = datetime(2026, 9, 23, tzinfo=ZoneInfo("America/Los_Angeles"))
    assert d["window"]["start_utc_ms"] == int(start.timestamp() * 1000)
    assert d["window"]["end_utc_ms"] == int(end.timestamp() * 1000)
    selected = datetime.fromisoformat(d["selection_evidence"]["observed_at_utc"])
    assert d["selected_at_utc_ms"] == int(selected.timestamp() * 1000)
    assert selected < start.astimezone(timezone.utc)
    assert d["selection_evidence"]["requested_dates_changed"] is False
    assert d["materialized_protected_ids"] == []
    assert d["fit_authorized"] is False and d["collection_authorized"] is False
    assert d["selection_evidence"]["base_file_sha256"] == hashlib.sha256(
        (ROOT / "north_star/EVAL_FREEZE.json").read_bytes()).hexdigest()
    assert pin in (ROOT / "north_star/EVAL_RESERVATION_V3.md").read_text()
    assert r.assign(**metadata(event_at_utc_ms=int(start.timestamp() * 1000),
        available_at_utc_ms=int(start.timestamp() * 1000),
        captured_at_utc_ms=int(start.timestamp() * 1000),
        collection_started_at_utc_ms=int(start.timestamp() * 1000))) == "SEALED_ECONOMICS"
    assert d["coverage_policy"]["decision_basis"] == "ACQUISITION_HEALTH_ONLY"
    assert d["coverage_policy"]["retain_partial_and_outages"] is True
    assert d["coverage_policy"]["move_dates_or_replace_on_outcomes"] is False
    assert d["permissions"]["new_exact_source_provider_approvals"] == []
