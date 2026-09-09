"""North Star requirement coverage checks; not a corpus certifier.

Document excerpts are hash-pinned provenance, not admitted training records.
Positive evidence states require local, hashed receipts; fixture PASS alone
cannot supply corpus counts, source admission or independent acceptance.
"""
from pathlib import Path
import hashlib
import re

STATUSES = frozenset({"IMPLEMENTED_AND_TESTED", "DATA_PRESENT_UNADMITTED", "ADMITTED_WITH_SUPPORT", "MISSING_SOURCE", "UNSUPPORTED"})
OBJECTS = frozenset({"expert_source_claim_v1", "discovery_attention_event_v1", "narrative_competition_snapshot_v1", "thesis_revision_event_v1", "execution_parent_child_v1", "influence_incentive_context_v1", "session_opportunity_audit_v1", "rust_market_bridge_episode_v1"})


def _require(condition, message):
    if not condition:
        raise ValueError(message)


def _exact_ids(rows, key, expected):
    values = [r.get(key) for r in rows]
    _require(len(values) == len(set(values)), f"duplicate {key}")
    _require(set(values) == expected, f"{key}: missing {sorted(expected - set(values))}; unexpected {sorted(set(values) - expected)}")


def _citation(citation, documents):
    doc = documents.get(citation.get("document_id"))
    _require(doc is not None, "citation document missing")
    start, end = citation.get("start_line"), citation.get("end_line")
    _require(type(start) is int and type(end) is int and 1 <= start <= end <= doc["line_count"], "citation line range invalid")
    _require(all(str(i) in doc.get("cited_lines", {}) for i in range(start, end + 1)), "citation excerpt missing")


def _evidence_state(row, root):
    status = row.get("evidence_status")
    _require(status in STATUSES, "invalid evidence_status; DONE is never a coverage state")
    evidence = row.get("evidence")
    _require(isinstance(evidence, list), "evidence list required")
    kinds = set()
    for item in evidence:
        _require(isinstance(item, dict) and isinstance(item.get("path"), str)
                 and bool(item["path"].strip()) and isinstance(item.get("kind"), str),
                 "evidence receipt malformed")
        path = (root / item.get("path", "")).resolve()
        _require(path.is_relative_to(root.resolve()) and path.is_file(), "evidence local receipt missing")
        _require(hashlib.sha256(path.read_bytes()).hexdigest() == item.get("sha256"), "evidence hash mismatch")
        kinds.add(item.get("kind"))
    required = {
        "IMPLEMENTED_AND_TESTED": {"implementation", "test_result"},
        "DATA_PRESENT_UNADMITTED": {"raw_manifest"},
        "ADMITTED_WITH_SUPPORT": {"raw_manifest", "canonical_manifest", "admitted_examples", "source_admission", "independent_acceptance", "measured_support"},
    }.get(status, set())
    _require(required <= kinds, f"evidence missing for {status}: {sorted(required-kinds)}")
    production = {item["kind"] for item in evidence if item.get("scope") == "production"}
    if status == "ADMITTED_WITH_SUPPORT":
        _require(required <= production, "admitted evidence must be production, not fixture evidence")
        _require(isinstance(row.get("measured_support"), dict), "measured_support missing")
        _require(any(_independent_pass(item) for item in evidence),
                 "admitted acceptance requires independent production PASS evidence")
    if status in {"MISSING_SOURCE", "UNSUPPORTED"}:
        _require(bool(row.get("gaps")), "gaps required for missing/unsupported evidence")
    if "measured_support" in row:
        support = row["measured_support"]
        _require(isinstance(support, dict), "measured_support must be a map")
        keys = ("raw_rows", "canonical_rows", "admitted_rows", "independent_sessions", "useful_label_tokens")
        for key in keys:
            _require(key in support, f"measured_support.{key} missing")
            value = support[key]
            _require(value is None or (type(value) is int and value >= 0), f"measured_support.{key} invalid")
            if value is not None:
                _resolved_evidence(support.get("measurement_evidence"), evidence,
                                   "measured_support", independent=True)
        if status == "ADMITTED_WITH_SUPPORT":
            _require(all(type(support[key]) is int and support[key] > 0 for key in keys),
                     "measured_support requires positive measured production and independent support")
        if all(type(support[key]) is int for key in ("raw_rows", "canonical_rows", "admitted_rows")):
            _require(support["admitted_rows"] <= support["canonical_rows"] <= support["raw_rows"],
                     "measured_support raw/canonical/admitted counts inconsistent")


def _independent_pass(item):
    return (item.get("kind") == "independent_acceptance" and item.get("scope") == "production"
            and item.get("independent") is True and item.get("result") == "PASS")


def _resolved_evidence(references, evidence, kind, *, independent=False):
    """Bind literal receipt paths to the local hash-checked evidence list.

    Scope, independence and result are declarations, not authentication. No
    fixture receipt or unresolved truthy string can back production coverage.
    """
    _require(isinstance(references, list) and bool(references)
             and all(isinstance(ref, str) and bool(ref.strip()) for ref in references),
             f"{kind} evidence references must be a nonempty list of receipt paths")
    for ref in references:
        matches = [item for item in evidence if item.get("path") == ref and item.get("kind") == kind]
        _require(len(matches) == 1, f"{kind} evidence reference unresolved or ambiguous")
        item = matches[0]
        _require(item.get("scope") == "production", f"{kind} evidence must be production, not fixture")
        if independent:
            _require(item.get("independent") is True, f"{kind} evidence must be independent")
        if kind == "independent_acceptance":
            _require(_independent_pass(item), "acceptance evidence must record independent production PASS")


def validate_registry(data, root):
    """Validate the complete ledger, citation graph and evidence-qualified states.

    Full source verification remains a separate stage. If the governing files
    are available locally their hashes/excerpts are rechecked; portable review
    can retain the pinned excerpts without requiring the original drive.
    """
    root = Path(root)
    documents = data.get("documents", {})
    for key, doc in documents.items():
        _require(re.fullmatch(r"[0-9a-f]{64}", doc.get("sha256", "")) is not None, f"citation document {key} hash invalid")
        path = Path(doc.get("path", ""))
        if path.is_file():
            raw = path.read_bytes()
            _require(hashlib.sha256(raw).hexdigest() == doc["sha256"], f"citation document {key} hash mismatch")
            lines = raw.decode("utf-8").splitlines()
            _require(len(lines) == doc.get("line_count"), "citation document line_count mismatch")
            for n, text in doc.get("cited_lines", {}).items():
                _require(1 <= int(n) <= len(lines) and lines[int(n)-1] == text, "citation excerpt mismatch")
    _require({"master_v5", "amendment_v6", "approved_plan"} <= set(documents), "citation governing documents missing")
    validate_requirement_ids(data.get("requirements", []))
    _exact_ids(data.get("stages", []), "stage_id", {f"STAGE-{i}" for i in range(9)})
    _exact_ids(data.get("objects", []), "object_id", OBJECTS)
    sources = data.get("sources", [])
    source_ids = {s.get("source_id") for s in sources}
    _require(len(source_ids) == len(sources), "duplicate source_id")
    overrides = {o.get("override_id"): o for o in data.get("overrides", [])}
    override = overrides.get("OPERATOR-QWEN-TRADER-ONLY")
    _require(override is not None, "operator override missing")
    _require(override.get("rust_action_conformance_required") is True and override.get("d14_qwen_sft_allowed") is False, "D14 override must retain conformance and exclude Rust SFT")
    for row in data["requirements"]:
        for field in ("owner", "module", "source_ids", "citations", "producer", "output", "acceptance", "gaps", "source_to_field_map", "schema_contract", "temporal_contract", "task_mask_contract", "dedup_contract", "split_contract"):
            _require(field in row and bool(row[field]), f"{row['requirement_id']}: {field} missing")
        _require(set(row["source_ids"]) <= source_ids, "unresolved source reference")
        _require(set(row.get("stage_ids", [])) <= {f"STAGE-{i}" for i in range(9)}, "unresolved stage reference")
        for field in ("producer", "output"):
            _require(row[field].get("status") in {"PLANNED", "VERIFIED"}, f"{field} status missing")
            if row[field]["status"] == "VERIFIED":
                kinds = {e.get("kind") for e in row.get("evidence", [])}
                _require(field in kinds, f"{field} evidence missing")
                if field == "producer":
                    _require(bool(row[field].get("version")), "producer version missing")
                else:
                    _require(bool(row[field].get("manifest_sha256")), "output manifest hash missing")
        acceptance = row["acceptance"]
        _require(acceptance.get("fixture_id") and acceptance.get("expected_result"), "acceptance fixture and expected result required")
        _require(acceptance.get("status") in {"NOT_RUN", "PASS", "FAIL", "INCONCLUSIVE"}, "acceptance status invalid")
        if acceptance["status"] == "PASS":
            _resolved_evidence(acceptance.get("evidence"), row.get("evidence", []),
                               "independent_acceptance", independent=True)
            _require(row.get("evidence_status") not in {"MISSING_SOURCE", "UNSUPPORTED"},
                     "acceptance PASS contradicts missing/unsupported evidence_status")
        if row.get("evidence_status") == "ADMITTED_WITH_SUPPORT":
            _require(all(row[field]["status"] == "VERIFIED" for field in ("producer", "output")),
                     "admitted evidence requires VERIFIED producer and output")
            _require(acceptance["status"] == "PASS", "admitted evidence requires acceptance PASS")
            _require({"producer", "output"} <= {item.get("kind") for item in row["evidence"]
                                                if item.get("scope") == "production"},
                     "admitted producer/output require production evidence")
        if row["requirement_id"] == "D14":
            _require(row.get("scope") == "engineering_only" and row.get("override_id") == "OPERATOR-QWEN-TRADER-ONLY", "D14 operator override lost")
    bridge = next(o for o in data["objects"] if o["object_id"] == "rust_market_bridge_episode_v1")
    _require(bridge.get("scope") == "engineering_only", "D14 bridge must remain engineering_only")
    for stage in data["stages"]:
        acceptance_status = stage.get("acceptance_status")
        _require(acceptance_status in {"NOT_RUN", "PASS", "FAIL", "INCONCLUSIVE"},
                 "stage acceptance status invalid")
        if acceptance_status == "PASS":
            _require(any(_independent_pass(item) for item in stage.get("evidence", [])),
                     "stage acceptance PASS requires independent production PASS evidence")
            _require(stage.get("evidence_status") not in {"MISSING_SOURCE", "UNSUPPORTED"},
                     "stage acceptance PASS contradicts missing/unsupported evidence_status")
        if stage.get("evidence_status") == "ADMITTED_WITH_SUPPORT":
            _require(acceptance_status == "PASS", "admitted stage requires acceptance PASS")
    for row in data["requirements"] + data["stages"] + data["objects"]:
        _evidence_state(row, root)

    def walk(value):
        if isinstance(value, dict):
            if "document_id" in value:
                _citation(value, documents)
            for child in value.values():
                walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)
    walk(data)

EXPECTED = frozenset({f"D{i:02d}" for i in range(1, 15)} | {f"G{i:02d}" for i in range(1, 13)})


def validate_requirement_ids(rows):
    """Require literal, unique D/G IDs without dropping operator exceptions."""
    ids = [row["requirement_id"] for row in rows]
    if len(ids) != len(set(ids)):
        raise ValueError("duplicate requirement_id")
    found = set(ids)
    if found != EXPECTED:
        raise ValueError({"missing": sorted(EXPECTED - found), "unexpected": sorted(found - EXPECTED)})
