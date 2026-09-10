"""Receipt fixtures are software tests, never actual North Star corpus evidence."""
import copy
import hashlib
import json
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
MODULE = ROOT / "src/north_star/build_acceptance.py"


def runner():
    assert MODULE.exists(), "all-stage acceptance runner has not been implemented"
    from north_star.build_acceptance import audit_build
    return audit_build


def matrix():
    return json.loads((ROOT / "north_star/REQUIREMENTS_MATRIX.json").read_text(encoding="utf-8"))


def registry():
    return {"schema_version": "north_star_build_evidence_v1", "registry_version": "test-v1",
            "build_id": "test-build", "artifacts": []}


def artifact(tmp_path, reg, requirement, kind="independent_acceptance", semantics="synthetic", category="software_component_review"):
    rid = requirement["requirement_id"]
    item = {"artifact_id": f"{rid}-{kind}-{len(reg['artifacts'])}", "artifact_version": "v1",
            "build_id": reg["build_id"], "requirement_id": rid,
            "stage_ids": requirement["stage_ids"], "scope": requirement["scope"],
            "evidence_type": kind, "evidence_category": category, "semantics": semantics,
            "producer_id": "fixture-producer", "reviewer_id": "fixture-reviewer",
            "result": "PASS"}
    payload = dict(item, schema_version="north_star_evidence_receipt_v1")
    path = tmp_path / (item["artifact_id"] + ".json")
    raw = json.dumps(payload).encode()
    path.write_bytes(raw)
    item.update(path=path.name, sha256=hashlib.sha256(raw).hexdigest())
    reg["artifacts"].append(item)
    return item


def rewrite(tmp_path, item, **changes):
    item.update(changes)
    payload = {k: v for k, v in item.items() if k not in {"sha256", "path"}}
    raw = json.dumps(dict(payload, schema_version="north_star_evidence_receipt_v1")).encode()
    (tmp_path / item["path"]).write_bytes(raw)
    item["sha256"] = hashlib.sha256(raw).hexdigest()


@pytest.mark.parametrize("mutation, reason", [
    ("hash", "hash mismatch"), ("scope", "out-of-scope"),
    ("stage", "out-of-scope"), ("missing_file", "missing"),
    ("path_escape", "outside"), ("version", "version"),
    ("binding", "binding"), ("pending", "pending"),
])
def test_bad_evidence_blocks(tmp_path, mutation, reason):
    m, reg = matrix(), registry()
    item = artifact(tmp_path, reg, m["requirements"][0])
    if mutation == "hash":
        item["sha256"] = "0" * 64
    elif mutation == "scope":
        rewrite(tmp_path, item, scope="engineering_only")
    elif mutation == "stage":
        rewrite(tmp_path, item, stage_ids=["STAGE-8"])
    elif mutation == "missing_file":
        (tmp_path / item["path"]).unlink()
    elif mutation == "path_escape":
        item["path"] = "../outside.json"
    elif mutation == "version":
        item.pop("artifact_version")
    elif mutation == "binding":
        item["result"] = "FAIL"
    elif mutation == "pending":
        rewrite(tmp_path, item, result="PENDING")
    result = runner()(m, reg, tmp_path)
    assert result["status"] == "BLOCKED"
    text = json.dumps(result).lower()
    assert reason in text
    assert result["requirements"][0]["software_component_review"]["status"] == "BLOCKED"


@pytest.mark.parametrize("mutation, reason", [("duplicate", "duplicate"), ("missing", "missing"), ("optional", "mandatory")])
def test_requirement_inventory_cannot_be_weakened(tmp_path, mutation, reason):
    m = matrix()
    if mutation == "duplicate":
        m["requirements"].append(copy.deepcopy(m["requirements"][0]))
    elif mutation == "missing":
        m["requirements"].pop()
    else:
        m["requirements"][0]["mandatory"] = False
    result = runner()(m, registry(), tmp_path)
    assert result["status"] == "BLOCKED"
    assert reason in json.dumps(result["errors"]).lower()


def test_synthetic_claimed_as_data_cannot_pass(tmp_path):
    m, reg = matrix(), registry()
    artifact(tmp_path, reg, m["requirements"][0], category="data_acceptance")
    result = runner()(m, reg, tmp_path)
    assert result["requirements"][0]["data_acceptance"]["status"] == "BLOCKED"
    assert "synthetic" in json.dumps(result["artifacts"]).lower()


def test_file_and_registry_limits_are_enforced(tmp_path):
    m, reg = matrix(), registry()
    artifact(tmp_path, reg, m["requirements"][0])
    result = runner()(m, reg, tmp_path, max_file_bytes=1)
    assert result["status"] == "BLOCKED"
    assert "limit" in json.dumps(result).lower()
    result = runner()(m, reg, tmp_path, max_artifacts=0)
    assert result["status"] == "BLOCKED"
    assert "limit" in json.dumps(result).lower()


def test_duplicate_artifact_ids_block(tmp_path):
    m, reg = matrix(), registry()
    item = artifact(tmp_path, reg, m["requirements"][0])
    reg["artifacts"].append(copy.deepcopy(item))
    result = runner()(m, reg, tmp_path)
    assert "duplicate artifact_id" in json.dumps(result["errors"])


def test_synthetic_receipt_is_software_review_not_corpus_gate(tmp_path):
    m, reg = matrix(), registry()
    artifact(tmp_path, reg, m["requirements"][0])
    result = runner()(m, reg, tmp_path)
    row = result["requirements"][0]
    assert row["software_component_review"]["status"] == "PASS"
    assert row["data_acceptance"]["status"] == "BLOCKED"
    assert row["status"] == "BLOCKED"
    assert result["artifacts"][0]["status"] == "VERIFIED"


DATA_TYPES = ("producer", "raw_manifest", "canonical_manifest", "admitted_examples",
              "source_admission", "independent_acceptance", "measured_support", "output")


def complete_registry(tmp_path, m):
    reg = registry()
    reg["corpus_id"] = "fixture-corpus-v1"
    for req in m["requirements"]:
        engineering = req["scope"] == "engineering_only"
        kinds = ("runtime_conformance", "independent_acceptance") if engineering else DATA_TYPES
        for kind in kinds:
            item = artifact(tmp_path, reg, req, kind=kind,
                            semantics="engineering" if engineering else "corpus",
                            category="software_component_review" if engineering else "data_acceptance")
            rewrite(tmp_path, item, corpus_id=reg["corpus_id"])
    for stage in m["stages"]:
        item = artifact(tmp_path, reg,
                        {"requirement_id": None, "stage_ids": [stage["stage_id"]], "scope": "whole_build"},
                        kind="stage_acceptance", semantics="corpus", category="data_acceptance")
        rewrite(tmp_path, item, corpus_id=reg["corpus_id"])
    return reg


def test_complete_declared_receipt_chain_passes_but_never_authorizes_training(tmp_path):
    m = matrix()
    reg = complete_registry(tmp_path, m)
    result = runner()(m, reg, tmp_path)
    assert result["status"] == "PASS"
    assert all(s["status"] == "PASS" for s in result["stages"])
    assert result["training_run_authorized"] is False
    assert result["corpus_population"] is None
    assert "authenticity" in result["verification_boundary"]


@pytest.mark.parametrize("mutation", ["missing_requirement", "narrative", "dependency", "wrong_corpus"])
def test_partial_chain_cannot_fullpass(tmp_path, mutation):
    m = matrix()
    reg = complete_registry(tmp_path, m)
    if mutation == "missing_requirement":
        reg["artifacts"] = [a for a in reg["artifacts"] if a["requirement_id"] != "D02"]
    elif mutation == "narrative":
        reg["artifacts"] = [a for a in reg["artifacts"] if a["requirement_id"] != "D07"]
    elif mutation == "dependency":
        reg["artifacts"] = [a for a in reg["artifacts"] if not (a["evidence_type"] == "stage_acceptance" and a["stage_ids"] == ["STAGE-0"])]
    else:
        rewrite(tmp_path, reg["artifacts"][0], corpus_id="different-build-corpus")
    result = runner()(m, reg, tmp_path)
    assert result["status"] == "BLOCKED"
    assert result["stages"][-1]["status"] == "BLOCKED"
    if mutation == "dependency":
        assert "STAGE-0" in result["stages"][1]["blocked_dependencies"]


def test_earlier_stage_does_not_require_future_stage_evidence(tmp_path):
    m = matrix()
    reg = complete_registry(tmp_path, m)
    reg["artifacts"] = [a for a in reg["artifacts"] if "STAGE-0" in a["stage_ids"]]
    for item in reg["artifacts"]:
        rewrite(tmp_path, item, stage_ids=["STAGE-0"])
    report = runner()(m, reg, tmp_path)
    assert report["status"] == "BLOCKED"
    assert report["stages"][0]["status"] == "PASS"
    assert report["stages"][1]["status"] == "BLOCKED"


def test_current_tracker_import_is_metadata_only_and_immutable(tmp_path):
    runner()
    from north_star.build_acceptance import run_audit
    root = tmp_path / "inputs"
    root.mkdir()
    (root / "matrix.json").write_text(json.dumps(matrix()), encoding="utf-8")
    (root / "master.md").write_text("Local governing master snapshot", encoding="utf-8")
    (root / "component.md").write_text("100 tests PASS; software only", encoding="utf-8")
    tracker = {"schema": "north_star_execution_stage_tracker_v1", "master": "master.md",
               "stages": [{"stage": 1, "status": "COMPLETE", "existing_receipts": ["component.md"]}]}
    (root / "tracker.json").write_text(json.dumps(tracker), encoding="utf-8")
    before = {p.name: p.read_bytes() for p in root.iterdir()}
    output = tmp_path / "new-audit"
    result = run_audit(root / "matrix.json", root / "tracker.json", root, output)
    assert result["status"] == "BLOCKED"
    assert result["summary"]["verified_artifacts"] == 1
    assert result["summary"]["blocked_requirements"] == len(matrix()["requirements"])
    assert json.loads((output / "audit.json").read_text())["status"] == "BLOCKED"
    assert json.loads((output / "evidence_registry.json").read_text())["artifacts"][0]["semantics"] == "metadata_only"
    assert before == {p.name: p.read_bytes() for p in root.iterdir()}
    with pytest.raises(FileExistsError):
        run_audit(root / "matrix.json", root / "tracker.json", root, output)


def test_cli_writes_machine_readable_blocked_result(tmp_path):
    import subprocess
    import sys
    import os
    env = dict(os.environ, PYTHONPATH=str(ROOT / "src"))
    run = subprocess.run([sys.executable, "-m", "north_star.build_acceptance",
        "--matrix", "north_star/REQUIREMENTS_MATRIX.json", "--tracker", "north_star/BUILD_STAGE_TRACKER.json",
        "--evidence-root", "north_star", "--artifact-dir", str(tmp_path / "audit")],
        cwd=ROOT, capture_output=True, text=True, env=env)
    assert run.returncode == 2, run.stdout + run.stderr
    summary = json.loads(run.stdout)
    assert summary["status"] == "BLOCKED"
    assert (tmp_path / "audit/audit.json").is_file()


def test_json_duplicate_keys_and_nonfinite_receipts_rejected(tmp_path):
    m, reg = matrix(), registry()
    item = artifact(tmp_path, reg, m["requirements"][0])
    for raw in (b'{"result":"PASS","result":"FAIL"}', b'{"value":NaN}'):
        (tmp_path / item["path"]).write_bytes(raw)
        item["sha256"] = hashlib.sha256(raw).hexdigest()
        report = runner()(m, reg, tmp_path)
        assert report["artifacts"][0]["status"] == "BLOCKED"


def test_missing_evidence_enumerates_every_requirement_and_stage(tmp_path):
    m = matrix()
    result = runner()(m, registry(), tmp_path)
    assert result["status"] == "BLOCKED"
    assert {r["requirement_id"] for r in result["requirements"]} == {r["requirement_id"] for r in m["requirements"]}
    assert len(result["stages"]) == len(m["stages"])
    assert all(r["status"] == "BLOCKED" and r["reasons"] for r in result["requirements"])
    assert result["corpus_population"] is None
    assert result["training_run_authorized"] is False
    assert result["numerical_only_full_pass_allowed"] is False
