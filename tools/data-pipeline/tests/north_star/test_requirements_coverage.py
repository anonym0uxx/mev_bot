"""Test-only structural fixtures; no corpus or admission evidence."""
import copy
import hashlib
import json
import importlib
import importlib.util
from pathlib import Path
import sys

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "src"))
EXPECTED_IDS = {f"D{i:02d}" for i in range(1, 15)} | {f"G{i:02d}" for i in range(1, 13)}


def api():
    assert importlib.util.find_spec("north_star.requirements") is not None, "requirements validator is missing"
    return importlib.import_module("north_star.requirements")


def test_requirement_ids_are_exact_and_g08_cannot_disappear():
    validator = api().validate_requirement_ids
    rows = [{"requirement_id": i} for i in sorted(EXPECTED_IDS)]
    validator(rows)
    with pytest.raises(ValueError, match="G08"):
        validator([r for r in rows if r["requirement_id"] != "G08"])
    with pytest.raises(ValueError, match="duplicate"):
        validator(rows + [rows[0]])
    with pytest.raises(ValueError, match="D99"):
        validator(rows + [{"requirement_id": "D99"}])


def matrix():
    path = ROOT / "north_star/REQUIREMENTS_MATRIX.json"
    assert path.is_file(), "requirements matrix is missing"
    return json.loads(path.read_text(encoding="utf-8"))


def test_source_backed_matrix_enumerates_all_stages_objects_and_override():
    data = matrix()
    assert hasattr(api(), "validate_registry"), "registry validation is missing"
    api().validate_registry(data, ROOT)
    assert {r["requirement_id"] for r in data["requirements"]} == EXPECTED_IDS
    assert {r["stage_id"] for r in data["stages"]} == {f"STAGE-{i}" for i in range(9)}
    assert len(data["objects"]) == 8
    assert all(r["evidence_status"] != "ADMITTED_WITH_SUPPORT" for r in data["requirements"])
    assert all(r["measured_support"]["admitted_rows"] is None for r in data["requirements"])
    d14 = next(r for r in data["requirements"] if r["requirement_id"] == "D14")
    assert d14["scope"] == "engineering_only"
    assert d14["override_id"] == "OPERATOR-QWEN-TRADER-ONLY"
    assert data["overrides"][0]["rust_action_conformance_required"] is True


@pytest.mark.parametrize("field", ["owner", "module", "source_ids", "citations", "producer", "output", "acceptance", "gaps", "source_to_field_map", "schema_contract", "temporal_contract", "task_mask_contract", "dedup_contract", "split_contract"])
def test_registry_rejects_silently_missing_contracts(field):
    data = matrix()
    del data["requirements"][0][field]
    with pytest.raises(ValueError, match=field):
        api().validate_registry(data, ROOT)


@pytest.mark.parametrize("mutation,match", [
    ("missing_stage", "STAGE-8"), ("missing_object", "rust_market_bridge_episode_v1"),
    ("done", "evidence_status"), ("fake_admitted", "evidence"),
    ("fake_implemented", "evidence"), ("broken_source", "source"),
    ("broken_citation", "citation"), ("missing_override", "override"),
    ("restore_rust_sft", "D14"), ("invented_counts", "measured_support"),
    ("fake_producer", "producer"), ("fake_output", "output"), ("fake_acceptance", "acceptance"),
])
def test_registry_fails_closed_on_false_completion_and_broken_links(mutation, match):
    data = copy.deepcopy(matrix())
    row = data["requirements"][0]
    if mutation == "missing_stage": data["stages"].pop()
    elif mutation == "missing_object": data["objects"].pop()
    elif mutation == "done": row["evidence_status"] = "DONE"
    elif mutation == "fake_admitted": row["evidence_status"] = "ADMITTED_WITH_SUPPORT"
    elif mutation == "fake_implemented": row["evidence_status"] = "IMPLEMENTED_AND_TESTED"
    elif mutation == "broken_source": row["source_ids"] = ["absent"]
    elif mutation == "broken_citation": row["citations"][0]["start_line"] = 999999
    elif mutation == "missing_override": data["overrides"] = []
    elif mutation == "restore_rust_sft": data["requirements"][13]["scope"] = "qwen_training"
    elif mutation == "invented_counts": row["measured_support"]["admitted_rows"] = 100
    elif mutation == "fake_producer": row["producer"]["status"] = "VERIFIED"
    elif mutation == "fake_output": row["output"]["status"] = "VERIFIED"
    elif mutation == "fake_acceptance": row["acceptance"]["status"] = "PASS"
    with pytest.raises(ValueError, match=match):
        api().validate_registry(data, ROOT)


def admitted_registry(tmp_path):
    """Simulated production declarations, only in pytest's temporary directory."""
    data = matrix()
    row = data["requirements"][0]
    row["evidence_status"] = "ADMITTED_WITH_SUPPORT"
    for kind in ("raw_manifest", "canonical_manifest", "admitted_examples", "source_admission",
                 "independent_acceptance", "measured_support", "producer", "output"):
        path = tmp_path / (kind + ".json")
        path.write_text(json.dumps({"test_only": True, "kind": kind}))
        row["evidence"].append({"kind": kind, "path": path.name,
                                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                                "scope": "production", "independent": True, "result": "PASS"})
    row["producer"].update(status="VERIFIED", version="test-v1")
    row["output"].update(status="VERIFIED", manifest_sha256=row["evidence"][1]["sha256"])
    row["acceptance"].update(status="PASS", evidence=["independent_acceptance.json"])
    row["measured_support"].update(raw_rows=10, canonical_rows=8, admitted_rows=5,
                                   independent_sessions=2, useful_label_tokens=20,
                                   measurement_evidence=["measured_support.json"])
    return data, row


@pytest.mark.parametrize("mutation", ["missing_support", "null_sessions", "zero_sessions", "zero_tokens",
                                      "missing_raw", "boolean_admitted", "planned_producer",
                                      "planned_output", "not_run", "fail", "inconclusive"])
def test_admitted_state_requires_verified_chain_pass_and_measured_support(tmp_path, mutation):
    data, row = admitted_registry(tmp_path)
    if mutation == "missing_support": del row["measured_support"]
    elif mutation == "null_sessions": row["measured_support"]["independent_sessions"] = None
    elif mutation == "zero_sessions": row["measured_support"]["independent_sessions"] = 0
    elif mutation == "zero_tokens": row["measured_support"]["useful_label_tokens"] = 0
    elif mutation == "missing_raw": del row["measured_support"]["raw_rows"]
    elif mutation == "boolean_admitted": row["measured_support"]["admitted_rows"] = True
    elif mutation == "planned_producer": row["producer"]["status"] = "PLANNED"
    elif mutation == "planned_output": row["output"]["status"] = "PLANNED"
    else: row["acceptance"]["status"] = mutation.upper()
    with pytest.raises(ValueError):
        api().validate_registry(data, tmp_path)


def test_declared_production_chain_is_structurally_valid_not_corpus_certification(tmp_path):
    data, _ = admitted_registry(tmp_path)
    api().validate_registry(data, tmp_path)


@pytest.mark.parametrize("mutation", ["fixture_chain", "fixture_counts", "dependent_counts", "dependent_acceptance",
                                      "failed_receipt", "missing_result", "unresolved_measurement",
                                      "unresolved_acceptance", "string_measurement", "string_acceptance",
                                      "inverted_counts", "fixture_counts_unadmitted", "missing_receipt"])
def test_support_and_pass_resolve_to_production_independent_evidence(tmp_path, mutation):
    data, row = admitted_registry(tmp_path)
    by_kind = {e["kind"]: e for e in row["evidence"]}
    if mutation == "fixture_chain": by_kind["raw_manifest"]["scope"] = "fixture"
    elif mutation in ("fixture_counts", "fixture_counts_unadmitted"):
        by_kind["measured_support"]["scope"] = "fixture"
        if mutation == "fixture_counts_unadmitted": row["evidence_status"] = "IMPLEMENTED_AND_TESTED"; row["evidence"].extend([dict(by_kind["producer"], kind="implementation"), dict(by_kind["producer"], kind="test_result")])
    elif mutation == "dependent_counts": by_kind["measured_support"]["independent"] = False
    elif mutation == "dependent_acceptance": by_kind["independent_acceptance"]["independent"] = False
    elif mutation == "failed_receipt": by_kind["independent_acceptance"]["result"] = "FAIL"
    elif mutation == "missing_result": del by_kind["independent_acceptance"]["result"]
    elif mutation == "unresolved_measurement": row["measured_support"]["measurement_evidence"] = ["absent.json"]
    elif mutation == "unresolved_acceptance": row["acceptance"]["evidence"] = ["absent.json"]
    elif mutation == "string_measurement": row["measured_support"]["measurement_evidence"] = "measured_support.json"
    elif mutation == "string_acceptance": row["acceptance"]["evidence"] = "independent_acceptance.json"
    elif mutation == "inverted_counts": row["measured_support"]["canonical_rows"] = 1
    elif mutation == "missing_receipt": (tmp_path / "measured_support.json").unlink()
    with pytest.raises(ValueError):
        api().validate_registry(data, tmp_path)


@pytest.mark.parametrize("status", ["PASS", "BOGUS", None])
def test_stage_acceptance_is_validated_and_pass_requires_evidence(status):
    data = matrix()
    data["stages"][0]["acceptance_status"] = status
    with pytest.raises(ValueError, match="acceptance"):
        api().validate_registry(data, ROOT)


def test_stage_pass_requires_production_independent_pass_receipt(tmp_path):
    data, row = admitted_registry(tmp_path)
    stage = data["stages"][0]
    stage.update(evidence_status="ADMITTED_WITH_SUPPORT", acceptance_status="PASS",
                 evidence=copy.deepcopy(row["evidence"]), measured_support=copy.deepcopy(row["measured_support"]))
    api().validate_registry(data, tmp_path)
    stage["acceptance_status"] = "NOT_RUN"
    with pytest.raises(ValueError, match="acceptance"):
        api().validate_registry(data, tmp_path)


@pytest.mark.parametrize("kind", ["producer", "output"])
def test_admitted_producer_and_output_cannot_be_fixture_only(tmp_path, kind):
    data, row = admitted_registry(tmp_path)
    next(item for item in row["evidence"] if item["kind"] == kind)["scope"] = "fixture"
    with pytest.raises(ValueError, match="production"):
        api().validate_registry(data, tmp_path)
