"""All permissions and hashes below are explicitly test-only fixtures."""
import copy
import importlib
import importlib.util
from pathlib import Path
import sys

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "src"))


def api():
    assert importlib.util.find_spec("north_star.admission") is not None, "source admission validator is missing"
    return importlib.import_module("north_star.admission")


def grant(scope):
    return {"authorized": True, "evidence_id": "test-only-approval", "approved_by": "Alon", "approved_at_utc_ms": 1, "scope": scope, "source_id": "fixture", "source_version": "v1"}


def source():
    scopes = ("collection_access", "local_transform", "training_inclusion", "training_run", "live_orders")
    return {"source_id": "fixture", "version": "v1", "owner_licensor": "test-only-owner", "original_or_derivative": "original", "parent_source_ids": [],
            "canonical_url": "https://example.invalid/test-only", "raw_sha256": "a" * 64, "access_method": "test-only", "quota_budget": "no network calls", "historical_availability": "test-only", "intended_tasks": ["action_only"],
            "license": {"identifier": "CC-BY-4.0", "version": "4.0", "evidence_uri": "test-only://license", "evidence_sha256": "b" * 64, "conflicting": False, "revoked": False, "attribution_obligations": "test-only attribution", "obligations_satisfied": True,
                        "rights": {"collection": True, "transform": True, "train": True, "redistribution": True}},
            "authoring_origin": "human", "provenance_evidence": ["test-only://origin"], "technical_task_eligible": True, "technical_evidence": ["test-only://technical"],
            "candidate_export_status": "QUARANTINED",
            "source_license_token_report": {"source_id": "fixture", "source_version": "v1", "license_identifier": "CC-BY-4.0", "license_evidence_sha256": "b" * 64, "raw_sha256": "a" * 64, "raw_tokens": 5, "model_visible_tokens": 4, "loss_bearing_tokens": 2, "disclosed_to": "Alon", "evidence_id": "test-only-report"},
            "approvals": {s: grant(s) for s in scopes}}


@pytest.mark.parametrize("section", ["approvals", "license", "source_license_token_report"])
def test_malformed_nested_objects_reject_instead_of_crashing(section):
    row = source()
    row[section] = ["not an object"]
    result = api().evaluate_source(row)
    assert not result["training_inclusion_allowed"]
    assert result["reasons"]


@pytest.mark.parametrize("row", [None, [], "not an object", {}])
def test_malformed_top_level_rejects(row):
    assert not api().evaluate_source(row)["training_inclusion_allowed"]


def test_complete_fixture_can_pass_inclusion_without_running_anything():
    result = api().evaluate_source(source())
    assert result["training_inclusion_allowed"] is True
    assert result["collection_allowed"] is True
    assert result["training_run_authorized"] is True
    assert result["live_orders_authorized"] is True
    assert result["candidate_loader_allowed"] is False
    assert result["reasons"] == []


@pytest.mark.parametrize("location", ["approval", "rights", "parent", "registry", "source_id"])
@pytest.mark.parametrize("bad", [[], ["bad"], "bad", 1, None])
def test_malformed_deep_contracts_are_explicitly_blocked(location, bad):
    row = source()
    registry = {}
    if location == "approval":
        row["approvals"]["training_inclusion"] = bad
    elif location == "rights":
        row["license"]["rights"] = bad
    elif location == "source_id":
        row["source_id"] = bad
    else:
        row["original_or_derivative"] = "derivative"
        row["parent_source_ids"] = ["parent"]
        registry = {"parent": bad} if location == "parent" else bad
    result = api().evaluate_source(row, registry)
    assert result["status"] == "BLOCKED"
    assert result["training_inclusion_allowed"] is False
    assert result["candidate_loader_allowed"] is False
    assert result["reasons"]


def test_collection_is_not_training_or_live_authority():
    row = source()
    row["approvals"] = {"collection_access": grant("collection_access")}
    row["license"]["identifier"] = "UNKNOWN"
    result = api().evaluate_source(row)
    assert result["collection_allowed"] is True
    assert result["training_inclusion_allowed"] is False
    assert result["training_run_authorized"] is False
    assert result["live_orders_authorized"] is False


@pytest.mark.parametrize("tasks", [" ", "action_only", {}, [""], [" "], [None], [1], ["action_only", " "]])
def test_intended_tasks_require_a_list_of_nonblank_identifiers(tasks):
    row = source()
    row["intended_tasks"] = tasks
    result = api().evaluate_source(row)
    assert result["status"] == "BLOCKED"
    assert not result["training_inclusion_allowed"]
    assert "missing_source_metadata" in result["reasons"]


@pytest.mark.parametrize("license_id", [None, "", "UNKNOWN", "CC-BY-NC-4.0", "CC-BY-ND-4.0", "GPL-3.0-only", "training-only", "public", "MIT OR proprietary", "mit", " MIT"])
def test_unknown_restrictive_or_nonliteral_license_rejected(license_id):
    row = source(); row["license"]["identifier"] = license_id
    assert not api().evaluate_source(row)["training_inclusion_allowed"]


@pytest.mark.parametrize("mutation", ["missing_license_evidence", "bad_hash", "revoked", "conflict", "no_train_right", "training_only_grant", "ai_origin", "human_approved_ai", "unknown_origin", "missing_provenance", "not_technical", "no_report", "no_inclusion", "wrong_scope", "wrong_source", "wrong_version", "unknown_collection", "no_transform", "stale_report", "negative_tokens", "boolean_tokens", "unfulfilled_attribution", "nonboolean_grant"])
def test_fail_closed_admission(mutation):
    row = copy.deepcopy(source())
    if mutation == "missing_license_evidence": row["license"]["evidence_uri"] = None
    elif mutation == "bad_hash": row["raw_sha256"] = "not-sha256"
    elif mutation == "revoked": row["license"]["revoked"] = True
    elif mutation == "conflict": row["license"]["conflicting"] = True
    elif mutation == "no_train_right": row["license"]["rights"]["train"] = False
    elif mutation == "training_only_grant": row["license"]["rights"]["redistribution"] = False
    elif mutation == "ai_origin": row["authoring_origin"] = "model_generated"
    elif mutation == "human_approved_ai": row["authoring_origin"] = "human_reviewed_model_generated"
    elif mutation == "unknown_origin": row["authoring_origin"] = None
    elif mutation == "missing_provenance": row["provenance_evidence"] = []
    elif mutation == "not_technical": row["technical_task_eligible"] = None
    elif mutation == "no_report": row["source_license_token_report"] = None
    elif mutation == "no_inclusion": del row["approvals"]["training_inclusion"]
    elif mutation == "wrong_scope": row["approvals"]["training_inclusion"]["scope"] = "collection_access"
    elif mutation == "wrong_source": row["approvals"]["training_inclusion"]["source_id"] = "other"
    elif mutation == "wrong_version": row["approvals"]["training_inclusion"]["source_version"] = "v2"
    elif mutation == "unknown_collection": row["approvals"]["collection_access"]["authorized"] = None
    elif mutation == "no_transform": del row["approvals"]["local_transform"]
    elif mutation == "stale_report": row["source_license_token_report"]["raw_sha256"] = "c" * 64
    elif mutation == "negative_tokens": row["source_license_token_report"]["raw_tokens"] = -1
    elif mutation == "boolean_tokens": row["source_license_token_report"]["raw_tokens"] = True
    elif mutation == "unfulfilled_attribution": row["license"]["obligations_satisfied"] = False
    elif mutation == "nonboolean_grant": row["approvals"]["training_inclusion"]["authorized"] = "true"
    result = api().evaluate_source(row)
    assert not result["training_inclusion_allowed"]
    assert result["reasons"]


def test_derivatives_inherit_all_parent_restrictions_and_cycles_fail_closed():
    child = source(); child["original_or_derivative"] = "derivative"; child["parent_source_ids"] = ["parent"]
    parent = source(); parent["source_id"] = "parent"; parent["license"]["identifier"] = "CC-BY-NC-4.0"
    assert not api().evaluate_source(child, {"parent": parent})["training_inclusion_allowed"]
    assert not api().evaluate_source(child, {})["training_inclusion_allowed"]
    child["parent_source_ids"] = ["fixture"]
    assert not api().evaluate_source(child, {"fixture": child})["training_inclusion_allowed"]


def test_inclusion_does_not_grant_training_run_or_orders_and_loader_requires_release():
    row = source()
    del row["approvals"]["training_run"]; del row["approvals"]["live_orders"]
    result = api().evaluate_source(row)
    assert result["training_inclusion_allowed"]
    assert not result["training_run_authorized"]
    assert not result["live_orders_authorized"]
    assert not result["candidate_loader_allowed"]
    row["candidate_export_status"] = "APPROVED"
    assert api().evaluate_source(row)["candidate_loader_allowed"]
