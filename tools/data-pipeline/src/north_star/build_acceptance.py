"""Bounded, offline verification of declared build evidence, not corpus truth.

PASS means a complete hash/scope-bound receipt chain under the supplied matrix.
It neither authenticates reviewers nor re-runs semantic/corpus acceptance. Never
walks directories, opens source data named inside receipts, or permits training.
"""
from __future__ import annotations

import hashlib
import json
from pathlib import Path, PureWindowsPath
import re
import stat

from .requirements import validate_requirement_ids

DATA_TYPES = frozenset({"producer", "output", "raw_manifest", "canonical_manifest",
                       "admitted_examples", "source_admission", "independent_acceptance",
                       "measured_support"})
ENGINEERING_TYPES = frozenset({"runtime_conformance", "independent_acceptance"})
BINDING_FIELDS = ("artifact_id", "artifact_version", "build_id", "requirement_id",
                  "stage_ids", "scope", "evidence_type", "evidence_category",
                  "semantics", "producer_id", "reviewer_id", "result")
MAX_FILE_BYTES = 2 * 1024 * 1024
MAX_TOTAL_BYTES = 32 * 1024 * 1024
MAX_ARTIFACTS = 2048


def _require(condition, reason):
    if not condition:
        raise ValueError(reason)


def _text(value):
    return isinstance(value, str) and bool(value.strip())


def _unique_pairs(pairs):
    out = {}
    for key, value in pairs:
        _require(key not in out, f"duplicate JSON key: {key}")
        out[key] = value
    return out


def _json(raw):
    def invalid(value):
        raise ValueError(f"nonfinite JSON: {value}")
    return json.loads(raw, object_pairs_hook=_unique_pairs, parse_constant=invalid)


def _local_read(root, relative, limit):
    _require(_text(relative), "missing local file path")
    win = PureWindowsPath(relative)
    _require(not Path(relative).is_absolute() and not win.drive and not win.root
             and ".." not in win.parts and ":" not in relative,
             "file path outside supplied local root")
    path = (root / relative).resolve()
    _require(path.is_relative_to(root), "file path outside supplied local root")
    _require(path.is_file(), "missing local evidence file")
    with path.open("rb") as handle:
        import os
        info = os.fstat(handle.fileno())
        _require(stat.S_ISREG(info.st_mode), "not a regular evidence file")
        _require(info.st_size <= limit, "file byte limit exceeded")
        raw = handle.read(limit + 1)
        _require(len(raw) <= limit, "file byte limit exceeded")
    return raw


def _gate(items, needed, stages):
    missing = {stage: sorted(needed - {a["evidence_type"] for a in items
                                     if stage in a["stage_ids"]}) for stage in stages}
    missing = {stage: kinds for stage, kinds in missing.items() if kinds}
    return {"status": "BLOCKED" if missing else "PASS", "missing_by_stage": missing,
            "evidence_ids": [a["artifact_id"] for a in items]}


def _validate_matrix(matrix):
    _require(matrix.get("schema_version") == "north_star_requirements_v1", "matrix version unsupported")
    rows = matrix["requirements"]
    validate_requirement_ids(rows)  # Existing authority's exact ID integrity guard.
    # Scope and membership below come from the matrix, not a second D/G task list.
    stages = [s["stage_id"] for s in matrix["stages"]]
    _require(len(stages) == len(set(stages)), "duplicate stage_id")
    _require(set(stages) == {f"STAGE-{i}" for i in range(9)}, "missing or unknown master stage")
    _require(matrix["policy"].get("narrative_required") is True, "mandatory narrative policy missing")
    for row in rows:
        _require(row.get("mandatory", True) is True, "mandatory requirement cannot be disabled")
        _require(row.get("scope") in {"qwen_trader", "engineering_only"}, "out-of-scope matrix requirement")
        ids = row.get("stage_ids")
        _require(isinstance(ids, list) and ids and len(ids) == len(set(ids))
                 and set(ids) <= set(stages), "out-of-scope requirement stage")
    d14 = next(r for r in rows if r["requirement_id"] == "D14")
    _require(d14["scope"] == "engineering_only"
             and d14.get("override_id") == "OPERATOR-QWEN-TRADER-ONLY",
             "D14 engineering-only conformance override missing")


def _verify_artifact(item, registry, requirements, stage_ids, root, limit):
    _require(isinstance(item, dict), "artifact must be an object")
    for field in BINDING_FIELDS:
        _require(field in item, f"missing artifact {field}")
    for field in ("artifact_id", "artifact_version", "build_id", "evidence_type", "producer_id"):
        _require(_text(item[field]), f"missing artifact {field}")
    _require(item["build_id"] == registry["build_id"], "out-of-scope build_id")
    ids = item["stage_ids"]
    _require(isinstance(ids, list) and ids and len(ids) == len(set(ids))
             and set(ids) <= stage_ids, "out-of-scope evidence stage")
    rid = item["requirement_id"]
    if rid is not None:
        _require(rid in requirements, "out-of-scope evidence requirement")
        req = requirements[rid]
        _require(item["scope"] == req["scope"] and set(ids) <= set(req["stage_ids"]),
                 "out-of-scope evidence requirement/stage/scope")
    else:
        _require(item["scope"] == "whole_build", "out-of-scope stage evidence")
    _require(isinstance(item.get("sha256"), str)
             and re.fullmatch(r"[0-9a-f]{64}", item["sha256"]), "invalid SHA256 format")
    raw = _local_read(root, item["path"], limit)
    _require(hashlib.sha256(raw).hexdigest() == item["sha256"], "file hash mismatch")
    category = item["evidence_category"]
    semantics = item["semantics"]
    _require(category in {"software_component_review", "data_acceptance", "supporting_metadata"},
             "unknown evidence category")
    _require(semantics in {"synthetic", "source", "corpus", "engineering", "metadata_only"},
             "unknown evidence semantics")
    if category == "supporting_metadata":
        _require(semantics == "metadata_only" and item["result"] == "UNASSESSED"
                 and item["evidence_type"] == "existing_receipt", "metadata is not acceptance")
        return len(raw)
    receipt = _json(raw)
    _require(isinstance(receipt, dict) and receipt.get("schema_version") == "north_star_evidence_receipt_v1",
             "receipt schema version unsupported")
    for field in BINDING_FIELDS:
        _require(field in receipt and receipt[field] == item[field], f"receipt binding mismatch: {field}")
    if category == "data_acceptance":
        _require(semantics in {"source", "corpus"}, "synthetic/engineering/metadata cannot close corpus gate")
        _require(item["scope"] != "engineering_only", "out-of-scope engineering data acceptance")
        _require(_text(registry.get("corpus_id")) and item.get("corpus_id") == registry["corpus_id"]
                 and receipt.get("corpus_id") == registry["corpus_id"], "out-of-scope corpus binding")
        _require(item["evidence_type"] in DATA_TYPES | {"stage_acceptance"}, "unsupported data evidence type")
        if item["evidence_type"] not in {"raw_manifest", "source_admission", "producer"}:
            _require(semantics == "corpus", "source-only evidence cannot close corpus gate")
    if item["evidence_type"] in {"independent_acceptance", "stage_acceptance"}:
        _require(_text(item["reviewer_id"]) and item["reviewer_id"] != item["producer_id"],
                 "independent reviewer identity missing or same as producer")
    _require(item["result"] == "PASS", f"pending/failed acceptance: {item['result']}")
    return len(raw)


def audit_build(matrix, registry, root, *, max_file_bytes=MAX_FILE_BYTES,
                max_total_bytes=MAX_TOTAL_BYTES, max_artifacts=MAX_ARTIFACTS):
    """Return deterministic JSON-compatible results; invalid/missing evidence blocks.

    Only explicit local receipt files are read. Limits apply even to metadata;
    matrix and registry passed as Python objects are caller-owned bounded inputs.
    """
    result = {"schema_version": "north_star_build_acceptance_v1", "status": "BLOCKED",
              "build_id": registry.get("build_id") if isinstance(registry, dict) else None,
              "errors": [], "requirements": [], "stages": [], "artifacts": [],
              "corpus_population": None, "corpus_population_reason": "not measured by metadata-only audit; no population claim",
              "training_run_authorized": False, "numerical_only_full_pass_allowed": False,
              "verification_boundary": "hashes, declared scope and complete receipt chain only; no semantic truth or reviewer authenticity verification",
              "dependency_policy": "acceptance requires every earlier master stage; safe parallel construction is not prohibited"}
    try:
        _validate_matrix(matrix)
        _require(isinstance(registry, dict) and registry.get("schema_version") == "north_star_build_evidence_v1",
                 "registry schema version unsupported")
        _require(_text(registry.get("registry_version")) and _text(registry.get("build_id")),
                 "registry version/build_id missing")
        _require(all(type(n) is int and n >= 0 for n in (max_file_bytes, max_total_bytes, max_artifacts)),
                 "invalid verification limits")
        artifacts = registry.get("artifacts")
        _require(isinstance(artifacts, list) and len(artifacts) <= max_artifacts, "artifact count limit exceeded or list missing")
        ids = [a.get("artifact_id") for a in artifacts if isinstance(a, dict)]
        _require(len(ids) == len(artifacts) and all(_text(i) for i in ids), "artifact_id missing")
        _require(len(ids) == len(set(ids)), "duplicate artifact_id")
    except (ValueError, KeyError, TypeError, AttributeError) as exc:
        result["errors"].append(str(exc))
        return result
    root = Path(root).resolve()
    requirements = {r["requirement_id"]: r for r in matrix["requirements"]}
    stage_ids = {s["stage_id"] for s in matrix["stages"]}
    valid = []
    remaining = max_total_bytes
    for item in artifacts:
        check = {"artifact_id": item["artifact_id"], "path": item.get("path"),
                 "evidence_type": item.get("evidence_type"), "sha256": item.get("sha256"),
                 "scope": item.get("scope"), "semantics": item.get("semantics"),
                 "status": "BLOCKED", "reasons": []}
        # Reserve the full per-file allowance before verification, including bad
        # hashes/JSON, so repeated invalid inputs cannot bypass the I/O budget.
        limit = min(max_file_bytes, remaining)
        remaining -= limit
        try:
            _require(limit > 0, "total byte limit exhausted")
            consumed = _verify_artifact(item, registry, requirements, stage_ids, root, limit)
            remaining += limit - consumed
            check["status"] = "VERIFIED"
            valid.append(item)
        except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError) as exc:
            check["reasons"].append(str(exc))
        result["artifacts"].append(check)
    for rid, req in requirements.items():
        items = [a for a in valid if a["requirement_id"] == rid]
        sw = [a for a in items if a["evidence_category"] == "software_component_review"]
        data = [a for a in items if a["evidence_category"] == "data_acceptance"]
        engineering = req["scope"] == "engineering_only"
        software = _gate(sw, ENGINEERING_TYPES if engineering else {"independent_acceptance"}, req["stage_ids"])
        acceptance = _gate(data, DATA_TYPES, req["stage_ids"])
        if engineering:
            acceptance = {"status": "NOT_APPLICABLE", "reason": "matrix engineering-only override; no Qwen Rust SFT admission"}
        chosen = software if engineering else acceptance
        reasons = [] if chosen["status"] == "PASS" else ["missing mandatory scoped evidence types", chosen.get("missing_by_stage", {})]
        result["requirements"].append({"requirement_id": rid, "title": req["title"],
            "scope": req["scope"], "mandatory": True, "stage_ids": req["stage_ids"],
            "status": chosen["status"], "reasons": reasons,
            "software_component_review": software, "data_acceptance": acceptance,
            "expected_acceptance": req.get("acceptance", {}).get("expected_result")})
    for stage in sorted(matrix["stages"], key=lambda s: int(s["stage_id"].split("-")[1])):
        sid = stage["stage_id"]
        reqs = [r for r in result["requirements"] if sid in r["stage_ids"]]
        blocked = [s["stage_id"] for s in result["stages"] if s["status"] != "PASS"]
        receipts = [a["artifact_id"] for a in valid if a["requirement_id"] is None
                    and a["evidence_category"] == "data_acceptance"
                    and a["evidence_type"] == "stage_acceptance" and sid in a["stage_ids"]]
        reasons = []
        if not receipts:
            reasons.append("missing independent whole-stage acceptance receipt")
        blocked_requirements = []
        for req in reqs:
            category = "software_component_review" if req["scope"] == "engineering_only" else "data_acceptance"
            needed = ENGINEERING_TYPES if req["scope"] == "engineering_only" else DATA_TYPES
            scoped = [a for a in valid if a["requirement_id"] == req["requirement_id"]
                      and a["evidence_category"] == category]
            if _gate(scoped, needed, [sid])["status"] != "PASS":
                blocked_requirements.append(req["requirement_id"])
        if blocked_requirements:
            reasons.append("mandatory stage requirements blocked")
        if blocked:
            reasons.append("earlier stage acceptance dependencies blocked")
        if any(a["status"] == "BLOCKED" for a in result["artifacts"]):
            reasons.append("registered evidence contains invalid or pending artifacts")
        result["stages"].append({"stage_id": sid, "title": stage["title"],
            "status": "BLOCKED" if reasons else "PASS", "reasons": reasons,
            "required_requirement_ids": [r["requirement_id"] for r in reqs],
            "blocked_dependencies": blocked, "evidence_ids": receipts})
    if all(s["status"] == "PASS" for s in result["stages"]):
        result["status"] = "PASS"
    result["summary"] = {"mandatory_requirements": len(result["requirements"]),
                         "blocked_requirements": sum(r["status"] == "BLOCKED" for r in result["requirements"]),
                         "stages": len(result["stages"]),
                         "blocked_stages": sum(s["status"] == "BLOCKED" for s in result["stages"]),
                         "registered_artifacts": len(artifacts),
                         "verified_artifacts": len(valid)}
    return result


def _input(path):
    path = Path(path).resolve()
    raw = _local_read(path.parent, path.name, MAX_FILE_BYTES)
    return raw, {"path": str(path), "sha256": hashlib.sha256(raw).hexdigest(), "bytes": len(raw)}


def _tracker_registry(tracker, tracker_hash, root):
    """Index only explicit receipt metadata; do not upgrade prose/test counts."""
    _require(tracker.get("schema") == "north_star_execution_stage_tracker_v1", "unsupported tracker schema")
    reg = {"schema_version": "north_star_build_evidence_v1",
           "registry_version": "tracker-snapshot-" + tracker_hash,
           "build_id": "tracker-snapshot-" + tracker_hash, "corpus_id": None,
           "artifacts": [], "import_errors": [],
           "provenance": "explicit existing_receipts only; tracker completion assertions not acceptance"}
    rows = tracker.get("stages", [])
    _require(isinstance(rows, list) and len(rows) <= 9, "tracker stage limit")
    seen = set()
    remaining = MAX_TOTAL_BYTES
    for stage in rows:
        number = stage.get("stage")
        _require(type(number) is int and 0 <= number <= 8 and number not in seen,
                 "duplicate or invalid tracker stage")
        seen.add(number)
        receipts = stage.get("existing_receipts", [])
        _require(isinstance(receipts, list), "tracker receipts list missing")
        for path in receipts:
            _require(len(reg["artifacts"]) < MAX_ARTIFACTS, "tracker artifact count limit")
            digest = None
            limit = min(MAX_FILE_BYTES, remaining)
            remaining -= limit
            try:
                _require(limit > 0, "tracker total byte limit")
                raw = _local_read(root, path, limit)
                remaining += limit - len(raw)
                digest = hashlib.sha256(raw).hexdigest()
            except (OSError, ValueError, TypeError) as exc:
                reg["import_errors"].append({"path": path, "reason": str(exc)})
            reg["artifacts"].append({
                "artifact_id": f"tracker-{number}-{len(reg['artifacts'])}",
                "artifact_version": "sha256:" + digest if digest else "UNRESOLVED",
                "build_id": reg["build_id"], "requirement_id": None,
                "stage_ids": [f"STAGE-{number}"], "scope": "whole_build",
                "evidence_type": "existing_receipt", "evidence_category": "supporting_metadata",
                "semantics": "metadata_only", "producer_id": "tracker-metadata-import",
                "reviewer_id": None, "result": "UNASSESSED", "path": path, "sha256": digest})
    return reg


def _publish(path, value):
    # New directory + exclusive creation: never overwrite a prior audit/authority.
    raw = (json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n").encode("utf-8")
    with path.open("xb") as handle:
        handle.write(raw)
    return {"path": path.name, "sha256": hashlib.sha256(raw).hexdigest(), "bytes": len(raw)}


def run_audit(matrix_path, tracker_path, evidence_root, artifact_dir, *, registry_path=None):
    """Snapshot governing metadata and write a new, reproducible receipt directory.

    With no supplied typed registry, only existing tracker receipt paths are
    imported as UNASSESSED metadata. No claims are extracted from their prose.
    """
    root = Path(evidence_root).resolve()
    destination = Path(artifact_dir)
    if destination.exists():
        raise FileExistsError(str(destination))
    matrix_raw, matrix_pin = _input(matrix_path)
    tracker_raw, tracker_pin = _input(tracker_path)
    matrix = _json(matrix_raw)
    tracker = _json(tracker_raw)
    master_raw = _local_read(root, tracker["master"], MAX_FILE_BYTES)
    pins = {"matrix": matrix_pin, "tracker": tracker_pin,
            "master": {"path": tracker["master"], "sha256": hashlib.sha256(master_raw).hexdigest(), "bytes": len(master_raw)}}
    if registry_path is None:
        registry = _tracker_registry(tracker, tracker_pin["sha256"], root)
    else:
        registry_raw, registry_pin = _input(registry_path)
        registry = _json(registry_raw)
        pins["registry"] = registry_pin
    report = audit_build(matrix, registry, root)
    report["inputs"] = pins
    report["evidence_root"] = str(root)
    report["tracker_assertions_are_acceptance"] = False
    destination.mkdir(parents=True, exist_ok=False)
    outputs = [_publish(destination / "evidence_registry.json", registry),
               _publish(destination / "audit.json", report)]
    _publish(destination / "artifact_manifest.json", {
        "schema_version": "north_star_build_audit_artifacts_v1", "files": outputs,
        "status": report["status"], "training_run_authorized": False})
    return report


def main(argv=None):
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--matrix", required=True)
    parser.add_argument("--tracker", required=True)
    parser.add_argument("--evidence-root", required=True)
    parser.add_argument("--registry", help="Optional typed versioned evidence registry; no completion booleans")
    parser.add_argument("--artifact-dir", required=True, help="New directory; existing destination is refused")
    args = parser.parse_args(argv)
    try:
        report = run_audit(args.matrix, args.tracker, args.evidence_root, args.artifact_dir,
                           registry_path=args.registry)
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError) as exc:
        print(json.dumps({"status": "BLOCKED", "errors": [str(exc)], "training_run_authorized": False}))
        return 2
    print(json.dumps({"status": report["status"], "summary": report.get("summary"),
                      "artifact_dir": str(Path(args.artifact_dir).resolve()),
                      "training_run_authorized": False}))
    return 0 if report["status"] == "PASS" else 2


if __name__ == "__main__":
    raise SystemExit(main())
