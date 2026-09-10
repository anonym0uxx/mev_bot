"""Synthetic fixtures are parser tests only, never teaching examples."""
import hashlib
import importlib.util
from copy import deepcopy
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


def api():
    path = ROOT / "src/north_star/narrative_context.py"
    assert path.exists(), "narrative context gate not implemented"
    spec = importlib.util.spec_from_file_location("narrative_context_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def evidence_record():
    raw = b"synthetic raw fixture only"
    evidence = {"raw_id": "raw:v1", "content_id": "content", "content_version": "v1",
                "sha256": hashlib.sha256(raw).hexdigest(), "span_start": 0,
                "span_end": len(raw), "span_unit": "BYTE"}
    return {"evidence": evidence}, {"raw:v1": raw}


def test_immutable_raw_bytes_hash_and_exact_span_are_required():
    row, objects = evidence_record()
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects)
    assert "RAW_EVIDENCE_MISSING" not in result["reason_codes"]
    assert "RAW_EVIDENCE_INVALID" not in result["reason_codes"]
    for field, value in [("sha256", "a" * 64), ("span_end", 999),
                         ("span_start", True), ("span_start", -1),
                         ("span_end", 0), ("content_version", ""),
                         ("span_unit", "CHAR"), ("raw_id", "other")]:
        changed = {"evidence": dict(row["evidence"], **{field: value})}
        rejected = api().evaluate_context(changed, cutoff_ms=100, raw_objects=objects)
        assert "RAW_EVIDENCE_INVALID" in rejected["reason_codes"], field


def proof(state="VERIFIED"):
    return {"state": state, "version": "v1", "evidence_ref": "proof:v1",
            "clock": "UTC_KNOWN", "available_at_ms": 40}


def identity_record():
    row, objects = evidence_record()
    row.update(source_id="source:v1", source_confidence=proof(),
               speaker_confidence=proof(), entity_confidence=proof(),
               rights=dict(proof("CLEARED"), scope="CONTEXT_ASSEMBLY"),
               mint_link=dict(proof(), mint="1" * 32, chain="solana",
                              content_id="content", content_version="v1"),
               numeric_snapshot=dict(proof(), snapshot_id="snapshot:v1",
                                     mint="1" * 32, decision_cutoff_ms=100))
    return row, objects


def test_identity_rights_and_verified_same_mint_snapshot_proofs():
    row, objects = identity_record()
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects)
    assert result["source_id"] is None
    assert result["source_null_reason"] == "SOURCE_UNKNOWN"
    assert result["reason_codes"] == ["AVAILABILITY_MISSING", "ORIGIN_RELATIONS_MISSING"]
    for field, reason in [("source_confidence", "SOURCE_CONFIDENCE_UNVERIFIED"),
                          ("speaker_confidence", "SPEAKER_CONFIDENCE_UNVERIFIED"),
                          ("entity_confidence", "ENTITY_CONFIDENCE_UNVERIFIED"),
                          ("rights", "RIGHTS_UNRESOLVED"),
                          ("mint_link", "MINT_LINK_UNVERIFIED"),
                          ("numeric_snapshot", "NUMERIC_SNAPSHOT_UNVERIFIED")]:
        altered = dict(row, **{field: dict(row[field], state="UNKNOWN")})
        assert reason in api().evaluate_context(altered, cutoff_ms=100,
                                               raw_objects=objects)["reason_codes"]
    for field, changes, reason in [
        ("rights", {"evidence_ref": ""}, "RIGHTS_UNRESOLVED"),
        ("rights", {"scope": "DISPLAY"}, "RIGHTS_UNRESOLVED"),
        ("mint_link", {"content_version": "future"}, "MINT_LINK_UNVERIFIED"),
        ("mint_link", {"mint": "not-a-mint"}, "MINT_LINK_UNVERIFIED"),
        ("numeric_snapshot", {"mint": "2" * 32}, "MINT_MISMATCH"),
        ("numeric_snapshot", {"decision_cutoff_ms": 99}, "CUTOFF_MISMATCH")]:
        altered = dict(row, **{field: dict(row[field], **changes)})
        assert reason in api().evaluate_context(altered, cutoff_ms=100,
                                               raw_objects=objects)["reason_codes"]


def timed_record():
    row, objects = identity_record()
    row["availability"] = {
        "clock": "UTC_KNOWN", "decision_cutoff_ms": 100,
        "content_id": "content", "content_version": "v1", "modality": "TEXT",
        "publish_ms": 10, "retrieved_ms": 20, "claim_available_ms": 30,
        "media_available_ms": 25, "compute_delay_ms": 3,
        "dependencies": {"verified-input:v1": dict(proof(), available_at_ms=60)}}
    row["origin"] = dict(proof(), canonical_content_id="content", relations=[])
    return row, objects


def test_join_uses_latest_dependency_plus_compute_delay():
    row, objects, registry = registered_record()
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects,
                                    trusted_proofs=registry)
    assert result["context_eligible"] is True
    assert result["training_admitted"] is False
    assert result["available_at_ms"] == 63
    assert result["reason_codes"] == []
    assert result["independent_testimony"] is True
    assert result["testimony_group"] == "content"
    assert result["graph_relations"] == []


@pytest.mark.parametrize("field", ["publish_ms", "retrieved_ms", "claim_available_ms",
                                   "media_available_ms", "asr_completed_ms"])
def test_late_media_claim_and_asr_block_early_decisions(field):
    row, objects = timed_record()
    row["availability"][field] = 101
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects)
    assert not result["context_eligible"]
    assert result["available_at_ms"] == 104
    assert "AFTER_CUTOFF" in result["reason_codes"]


@pytest.mark.parametrize("field", ["source_confidence", "speaker_confidence", "entity_confidence",
                                   "rights", "mint_link", "numeric_snapshot", "origin"])
def test_proof_versions_cannot_backdate_identity_rights_snapshot_or_origin(field):
    row, objects = timed_record()
    row[field]["available_at_ms"] = 101
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects)
    assert result["available_at_ms"] == 104
    assert "AFTER_CUTOFF" in result["reason_codes"]


@pytest.mark.parametrize("changes,reason", [
    ({"clock": "RELATIVE_ONLY"}, "CLOCK_UNRESOLVED"),
    ({"decision_cutoff_ms": 99}, "CUTOFF_MISMATCH"),
    ({"content_version": "later"}, "CONTENT_VERSION_MISMATCH"),
    ({"retrieved_ms": None}, "AVAILABILITY_MISSING"),
    ({"retrieved_ms": True}, "AVAILABILITY_INVALID"),
    ({"compute_delay_ms": -1}, "AVAILABILITY_INVALID"),
    ({"modality": "ASR"}, "AVAILABILITY_MISSING"),
    ({"dependencies": {}}, "DEPENDENCIES_MISSING"),
    ({"dependencies": {"x": None}}, "DEPENDENCY_UNVERIFIED")])
def test_unresolved_clock_or_dependencies_fail_closed(changes, reason):
    row, objects = timed_record()
    row["availability"].update(changes)
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects)
    assert not result["context_eligible"]
    assert reason in result["reason_codes"]


def test_asr_requires_media_epoch_proof_and_all_timings():
    row, objects, _ = registered_record()
    row["availability"].update(modality="ASR", asr_completed_ms=70)
    assert "MEDIA_EPOCH_UNRESOLVED" in api().evaluate_context(
        row, cutoff_ms=100, raw_objects=objects)["reason_codes"]
    row["availability"]["media_epoch"] = dict(proof(), state="RESOLVED")
    assert api().evaluate_context(row, cutoff_ms=100, raw_objects=objects,
                                  trusted_proofs=registry_fixture(row))["context_eligible"]


def test_reposts_are_graph_relations_not_independent_testimony():
    row, objects, _ = registered_record()
    edge = {"kind": "REPOST_OF", "target_content_id": "original"}
    row["origin"].update(canonical_content_id="original", relations=[edge])
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects,
                                    trusted_proofs=registry_fixture(row))
    assert result["context_eligible"]
    assert result["independent_testimony"] is False
    assert result["testimony_group"] == "original"
    assert result["graph_relations"] == [edge]
    row["origin"]["relations"] = []
    assert "ORIGIN_RELATIONS_INVALID" in api().evaluate_context(
        row, cutoff_ms=100, raw_objects=objects)["reason_codes"]


def test_same_timestamp_requires_explicit_causal_order_proof():
    row, objects, _ = registered_record()
    row["availability"]["compute_delay_ms"] = 40
    assert "SAME_TIME_ORDER_UNPROVEN" in api().evaluate_context(
        row, cutoff_ms=100, raw_objects=objects)["reason_codes"]
    row["availability"]["same_time_order"] = dict(proof(), decision_cutoff_ms=100)
    assert api().evaluate_context(row, cutoff_ms=100, raw_objects=objects,
                                  trusted_proofs=registry_fixture(row))["context_eligible"]


@pytest.mark.parametrize("cutoff", [None, True, -1, "100"])
def test_no_guessed_or_invalid_decision_cutoff(cutoff):
    row, objects = timed_record()
    result = api().evaluate_context(row, cutoff_ms=cutoff, raw_objects=objects)
    assert not result["context_eligible"]
    assert "DECISION_CUTOFF_UNRESOLVED" in result["reason_codes"]


def test_source_unknown_never_retains_identity():
    row, objects = timed_record()
    row["source_confidence"]["state"] = "UNKNOWN"
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects)
    assert result["source_id"] is None
    assert result["source_null_reason"] == "SOURCE_UNKNOWN"


def test_bounded_metadata_audit_is_read_only_and_does_not_generate_prose(tmp_path):
    import json
    source = tmp_path / "claims.jsonl"
    lines = [json.dumps({"claim_id": str(i), "claim_text": "synthetic fixture",
                         "account_handle": "handle", "admission_status": "GOLD",
                         "publish_time_ms": 1, "first_seen_ms": 2}) + "\n"
             for i in range(51)]
    payload = "".join(lines).encode()
    source.write_bytes(payload)
    result = api().audit_creator_claims(source, limit=50)
    assert source.read_bytes() == payload
    assert result["rows_audited"] == 50
    assert result["source_sha256"] == hashlib.sha256(payload).hexdigest()
    assert result["prefix_sha256"] == hashlib.sha256("".join(lines[:50]).encode()).hexdigest()
    assert result["context_eligible_count"] == 0
    assert result["training_admitted_count"] == 0
    assert result["scope"] == "DEVELOPMENT_EXPOSED_METADATA_ONLY"
    assert result["reason_counts"]["RAW_EVIDENCE_MISSING"] == 50
    assert result["reason_counts"]["DECISION_CUTOFF_UNRESOLVED"] == 50
    assert result["rows"][-1]["claim_id"] == "49"
    assert "synthetic fixture" not in json.dumps(result)
    assert result["rows"][0]["legacy_metadata"]["account_handle"] == "handle"
    assert result["rows"][0]["row_sha256"] == hashlib.sha256(lines[0].encode()).hexdigest()


@pytest.mark.parametrize("target", ["dependency", "rights", "numeric_snapshot", "media_epoch"])
def test_relative_or_unknown_proof_clocks_cannot_join_utc(target):
    row, objects = timed_record()
    if target == "dependency":
        row["availability"]["dependencies"]["verified-input:v1"]["clock"] = "RELATIVE_ONLY"
    elif target == "media_epoch":
        row["availability"].update(modality="ASR", asr_completed_ms=70,
                                   media_epoch=dict(proof("RESOLVED"), clock="UNKNOWN"))
    else:
        row[target]["clock"] = "UNKNOWN"
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects)
    assert not result["context_eligible"]
    assert "CLOCK_UNRESOLVED" in result["reason_codes"]


def test_repost_cannot_claim_itself_as_canonical():
    row, objects = timed_record()
    row["origin"]["relations"] = [{"kind": "DUPLICATE_OF", "target_content_id": "other"}]
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects)
    assert not result["context_eligible"]
    assert "ORIGIN_RELATIONS_INVALID" in result["reason_codes"]


def test_heuristic_labels_never_supply_missing_context_proofs():
    row = {"temporal_class": "EX_ANTE", "admission_status": "GOLD",
           "narrative_themes": ["entry_analysis"], "claim_text": "synthetic"}
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects={})
    assert result["context_eligible"] is False
    assert result["training_admitted"] is False
    assert result["available_at_ms"] is None
    assert result["source_id"] is None
    assert result["source_null_reason"] == "SOURCE_UNKNOWN"
    assert set(result["reason_codes"]) == {
        "RAW_EVIDENCE_MISSING", "SOURCE_UNKNOWN", "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING", "ENTITY_CONFIDENCE_MISSING", "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED", "NUMERIC_SNAPSHOT_MISSING", "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"}


def test_declarations_without_separate_registry_are_structural_only():
    row, objects = timed_record()
    row["trusted_proofs"] = {"proof:v1": proof()}
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects)
    assert result["context_eligible"] is False
    assert result["structural_valid"] is True
    assert result["verification_status"] == "STRUCTURAL_ONLY"
    assert result["source_id"] is None
    assert result["independent_testimony"] is False


def registry_fixture(row):
    """Test-only attestations, never an upstream verifier or real proof producer."""
    timing = row["availability"]
    proofs = {k: row[k] for k in ("source_confidence", "speaker_confidence",
              "entity_confidence", "rights", "mint_link", "numeric_snapshot", "origin")}
    proofs.update({"dependency:" + k: v for k, v in timing["dependencies"].items()})
    for key in ("media_epoch", "same_time_order"):
        if key in timing:
            proofs[key] = timing[key]
    for kind, declaration in proofs.items():
        declaration["evidence_ref"] = "test-only:" + kind
    subject = {"source_id": row["source_id"], "speaker_id": row["speaker_id"],
               "entity_id": row["entity_id"], "content_id": row["evidence"]["content_id"],
               "content_version": row["evidence"]["content_version"],
               "chain": row["mint_link"]["chain"], "mint": row["mint_link"]["mint"],
               "snapshot_id": row["numeric_snapshot"]["snapshot_id"],
               "decision_cutoff_ms": timing["decision_cutoff_ms"]}
    return {p["evidence_ref"]: {"kind": kind, "subject": deepcopy(subject),
            "evidence": deepcopy(row["evidence"]),
            "declaration": deepcopy(p), "availability": deepcopy(timing)}
            for kind, p in proofs.items()}


def registered_record():
    row, objects = timed_record()
    row.update(speaker_id="speaker:test-only", entity_id="entity:test-only")
    row["numeric_snapshot"].update(chain="solana", snapshot_version="snapshot-v1",
                                    observed_at_ms=35)
    return row, objects, registry_fixture(row)


def test_separate_registry_is_required_and_exactly_resolved():
    row, objects, registry = registered_record()
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects,
                                    trusted_proofs=registry)
    assert result["context_eligible"] is True
    assert result["verification_status"] == "REGISTRY_VERIFIED"
    assert result["source_id"] == row["source_id"]
    assert result["independent_testimony"] is True
    assert result["training_admitted"] is False


@pytest.mark.parametrize("change", ["missing_ref", "wrong_kind", "subject_source",
    "subject_speaker", "subject_entity", "content_version", "proof_version",
    "proof_clock", "proof_time", "row_source", "row_speaker", "row_entity",
    "backdated_publish", "order", "origin"])
def test_n1_registry_mismatch_never_promotes(change):
    row, objects, registry = registered_record()
    entry = registry[row["source_confidence"]["evidence_ref"]]
    if change == "missing_ref":
        registry.clear()
    elif change == "wrong_kind":
        entry["kind"] = "speaker_confidence"
    elif change.startswith("subject_"):
        entry["subject"][change.removeprefix("subject_") + "_id"] = "different"
    elif change == "content_version":
        entry["subject"]["content_version"] = "different"
    elif change.startswith("proof_"):
        key = {"proof_version": "version", "proof_clock": "clock",
               "proof_time": "available_at_ms"}[change]
        entry["declaration"][key] = "different"
    elif change.startswith("row_"):
        row[change.removeprefix("row_") + "_id"] = "different"
    elif change == "backdated_publish":
        entry["availability"]["publish_ms"] = 1000
    elif change == "order":
        row["availability"].update(compute_delay_ms=40,
                                    same_time_order=dict(proof(), decision_cutoff_ms=100))
    elif change == "origin":
        row["origin"]["canonical_content_id"] = "other"
        row["origin"]["relations"] = [{"kind": "REPOST_OF", "target_content_id": "other"}]
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects,
                                    trusted_proofs=registry)
    assert not result["context_eligible"]
    assert "PROOF_REGISTRY_MISMATCH" in result["reason_codes"]
    assert not result["independent_testimony"]


@pytest.mark.parametrize("change", ["span", "raw", "raw_id", "proof_hash", "proof_span"])
def test_n2_exact_raw_object_and_interval_binding(change):
    row, objects, registry = registered_record()
    if change == "span":
        row["evidence"].update(span_start=1, span_end=2)
    elif change == "raw":
        objects["raw:v1"] = b"unrelated test bytes"
        row["evidence"].update(sha256=hashlib.sha256(objects["raw:v1"]).hexdigest(),
                               span_end=len(objects["raw:v1"]))
    elif change == "raw_id":
        objects["other"] = objects["raw:v1"]
        row["evidence"]["raw_id"] = "other"
    else:
        entry = registry[row["mint_link"]["evidence_ref"]]
        entry["evidence"]["sha256" if change == "proof_hash" else "span_end"] = "different"
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects,
                                    trusted_proofs=registry)
    assert not result["context_eligible"]
    assert "PROOF_REGISTRY_MISMATCH" in result["reason_codes"]


@pytest.mark.parametrize("field,value", [("chain", "ethereum"), ("observed_at_ms", 1000),
    ("observed_at_ms", True), ("observed_at_ms", None), ("observed_at_ms", 41),
    ("snapshot_version", ""), ("as_of_ms", 1000), ("unknown_clock", "UTC_KNOWN")])
def test_n3_snapshot_metadata_must_be_supported_even_when_registry_matches(field, value):
    row, objects, _ = registered_record()
    row["numeric_snapshot"][field] = value
    registry = registry_fixture(row)
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects,
                                    trusted_proofs=registry)
    assert not result["context_eligible"]
    assert "PROOF_METADATA_INVALID" in result["reason_codes"]


@pytest.mark.parametrize("field,value", [("source_id", "other"), ("content_id", "other"),
    ("content_version", "other"), ("sha256", "0" * 64), ("span_start", 1),
    ("publish_ms", 1000), ("unsupported_assertion", "VERIFIED")])
def test_n3_contradictory_source_metadata_never_becomes_verified(field, value):
    row, objects, _ = registered_record()
    row["source_confidence"][field] = value
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects,
                                    trusted_proofs=registry_fixture(row))
    assert not result["context_eligible"]
    assert "PROOF_METADATA_INVALID" in result["reason_codes"]


@pytest.mark.parametrize("field,value", [("snapshot_id", "does-not-exist"),
    ("snapshot_version", "future"), ("observed_at_ms", 34), ("chain", "ethereum"),
    ("mint", "So11111111111111111111111111111111111111112")])
def test_n3_snapshot_changes_cannot_reuse_registry_attestation(field, value):
    row, objects, registry = registered_record()
    row["numeric_snapshot"][field] = value
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects,
                                    trusted_proofs=registry)
    assert not result["context_eligible"]
    assert "PROOF_REGISTRY_MISMATCH" in result["reason_codes"]


@pytest.mark.parametrize("kind", ["source_confidence", "speaker_confidence", "entity_confidence",
    "rights", "mint_link", "numeric_snapshot", "origin", "dependency:verified-input:v1",
    "media_epoch", "same_time_order"])
def test_every_proof_kind_requires_its_own_resolved_attestation(kind):
    row, objects, _ = registered_record()
    row["availability"].update(modality="ASR", asr_completed_ms=70,
        media_epoch=proof("RESOLVED"), same_time_order=dict(proof(), decision_cutoff_ms=100))
    registry = registry_fixture(row)
    del registry["test-only:" + kind]
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects,
                                    trusted_proofs=registry)
    assert not result["context_eligible"]
    assert "PROOF_REGISTRY_MISMATCH" in result["reason_codes"]


@pytest.mark.parametrize("target", ["timing_cutoff", "snapshot_cutoff", "order_cutoff",
    "timing_extra", "source_metadata"])
def test_registered_metadata_cannot_hide_invalid_types_or_unknown_fields(target):
    row, objects, _ = registered_record()
    timing = row["availability"]
    if target == "timing_cutoff":
        timing["decision_cutoff_ms"] = 100.0
    elif target == "snapshot_cutoff":
        row["numeric_snapshot"]["decision_cutoff_ms"] = 100.0
    elif target == "order_cutoff":
        timing.update(compute_delay_ms=40,
            same_time_order=dict(proof(), decision_cutoff_ms=100.0))
    elif target == "timing_extra":
        timing["observed_at_ms"] = 1000
    else:
        row["source_confidence"]["publish_ms"] = 1000
    result = api().evaluate_context(row, cutoff_ms=100, raw_objects=objects,
                                    trusted_proofs=registry_fixture(row))
    assert not result["context_eligible"]
    assert "PROOF_METADATA_INVALID" in result["reason_codes"]
    assert result["source_id"] is None

