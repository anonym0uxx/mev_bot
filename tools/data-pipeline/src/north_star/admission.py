"""Pure, fail-closed source-policy evaluation; performs no collection or training.

Evidence pointers must be authenticated by the caller's evidence store. This
validator checks the admission contract, not legal authenticity or corpus QA.
Unknown licenses are blocked, not guessed from public access or a subscription.
"""
import re

PERMISSIVE_LICENSES = frozenset({"MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "CC0-1.0", "CC-BY-4.0", "Unlicense"})
ALLOWED_ORIGINS = frozenset({"human", "observed_action", "deterministic_source_backed"})


def _text(value):
    return isinstance(value, str) and bool(value.strip())


def _hash(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def _approval(row, scope):
    approval = (row.get("approvals") or {}).get(scope) or {}
    return (approval.get("authorized") is True
            and approval.get("scope") == scope
            and approval.get("source_id") == row.get("source_id")
            and approval.get("source_version") == row.get("version")
            and approval.get("approved_by") == "Alon"
            and _text(approval.get("evidence_id"))
            and type(approval.get("approved_at_utc_ms")) is int
            and approval["approved_at_utc_ms"] >= 0)


def evaluate_source(row, source_registry=None, *, _ancestors=frozenset()):
    """Return independent stage gates and explicit reasons; never mutate input.

    Parent sources must independently pass inclusion: a derivative does not
    launder restrictive grants, absent lineage, generated prose or revocation.
    Run/order authorization fields report ONLY the supplied scoped grants;
    they are not launch APIs or substitutes for operational/risk gates.
    """
    malformed = not isinstance(row, dict)
    if not malformed:
        malformed = any(row.get(k) is not None and not isinstance(row[k], dict)
                        for k in ("approvals", "license", "source_license_token_report"))
    if not malformed:
        approvals = row.get("approvals") or {}
        license_info = row.get("license") or {}
        malformed = (not _text(row.get("source_id"))
                     or any(not isinstance(value, dict) for value in approvals.values())
                     or ("rights" in license_info and not isinstance(license_info["rights"], dict))
                     or (source_registry is not None and not isinstance(source_registry, dict)))
    if malformed:
        return {"status": "BLOCKED", "source_id": row.get("source_id") if isinstance(row, dict) else None,
                "collection_allowed": False, "local_transform_allowed": False,
                "rights_admissible": False, "technical_task_eligible": False,
                "training_inclusion_allowed": False, "training_run_authorized": False,
                "live_orders_authorized": False, "candidate_loader_allowed": False,
                "reasons": ["malformed_source_contract"]}
    registry = source_registry or {}
    reasons = []
    source_id = row.get("source_id")
    identity_ok = all(_text(row.get(k)) for k in (
        "source_id", "version", "owner_licensor", "canonical_url", "access_method",
        "quota_budget", "historical_availability"))
    tasks = row.get("intended_tasks")
    tasks_ok = isinstance(tasks, list) and bool(tasks) and all(_text(task) for task in tasks)
    if not identity_ok or not tasks_ok:
        reasons.append("missing_source_metadata")
    collection = identity_ok and _approval(row, "collection_access")
    transform = collection and _approval(row, "local_transform")
    if not collection:
        reasons.append("collection_access_not_authorized")
    if not transform:
        reasons.append("local_transform_not_authorized")
    license_info = row.get("license") or {}
    license_id = license_info.get("identifier")
    rights = license_info.get("rights") or {}
    rights_ok = (isinstance(license_id, str) and license_id in PERMISSIVE_LICENSES
                 and _text(license_info.get("version"))
                 and _text(license_info.get("evidence_uri"))
                 and _hash(license_info.get("evidence_sha256"))
                 and license_info.get("conflicting") is False
                 and license_info.get("revoked") is False
                 and _text(license_info.get("attribution_obligations"))
                 and license_info.get("obligations_satisfied") is True
                 and all(rights.get(k) is True for k in ("collection", "transform", "train", "redistribution")))
    if not rights_ok:
        reasons.append("rights_unknown_restrictive_revoked_or_unproven")
    origin = row.get("authoring_origin")
    provenance_ok = (_hash(row.get("raw_sha256")) and isinstance(origin, str)
                     and origin in ALLOWED_ORIGINS and bool(row.get("provenance_evidence")))
    if not provenance_ok:
        reasons.append("provenance_missing_or_generated_origin")
    technical = row.get("technical_task_eligible") is True and bool(row.get("technical_evidence"))
    if not technical:
        reasons.append("technical_task_not_eligible")
    report = row.get("source_license_token_report") or {}
    report_ok = (report.get("source_id") == source_id and report.get("source_version") == row.get("version")
                 and report.get("license_identifier") == license_id
                 and report.get("license_evidence_sha256") == license_info.get("evidence_sha256")
                 and report.get("raw_sha256") == row.get("raw_sha256")
                 and report.get("disclosed_to") == "Alon" and _text(report.get("evidence_id"))
                 and all(type(report.get(k)) is int and report[k] >= 0
                         for k in ("raw_tokens", "model_visible_tokens", "loss_bearing_tokens")))
    if report_ok:
        report_ok = report["loss_bearing_tokens"] <= report["model_visible_tokens"] <= report["raw_tokens"]
    if not report_ok:
        reasons.append("source_license_token_report_missing_or_stale")
    if not _approval(row, "training_inclusion"):
        reasons.append("training_inclusion_not_approved")
    parents = row.get("parent_source_ids")
    relationship = row.get("original_or_derivative")
    lineage_ok = isinstance(parents, list) and relationship in ("original", "derivative")
    if lineage_ok:
        lineage_ok = ((relationship == "original" and not parents)
                      or (relationship == "derivative" and bool(parents)))
    if source_id in _ancestors:
        lineage_ok = False
    if lineage_ok and parents:
        for parent_id in parents:
            parent = registry.get(parent_id) if isinstance(parent_id, str) else None
            if not isinstance(parent, dict) or parent.get("source_id") != parent_id:
                lineage_ok = False
                break
            if not evaluate_source(parent, registry, _ancestors=_ancestors | {source_id})["training_inclusion_allowed"]:
                lineage_ok = False
                break
    if not lineage_ok:
        reasons.append("parent_lineage_missing_restricted_or_cyclic")
    inclusion = not reasons
    return {"status": "ADMISSIBLE" if inclusion else "BLOCKED",
            "source_id": source_id, "collection_allowed": collection,
            "local_transform_allowed": transform, "rights_admissible": rights_ok,
            "technical_task_eligible": technical, "training_inclusion_allowed": inclusion,
            "training_run_authorized": _approval(row, "training_run"),
            "live_orders_authorized": _approval(row, "live_orders"),
            "candidate_loader_allowed": inclusion and row.get("candidate_export_status") == "APPROVED",
            "reasons": reasons}
