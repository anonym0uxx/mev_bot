"""Fail-closed context eligibility, NOT training admission or semantic truth."""
import hashlib
from collections.abc import Mapping


def _text(value):
    return isinstance(value, str) and bool(value.strip())


def _raw_valid(evidence, raw_objects):
    if not isinstance(evidence, Mapping):
        return False
    if not all(_text(evidence.get(k)) for k in
               ("raw_id", "content_id", "content_version", "sha256")):
        return False
    raw = raw_objects.get(evidence["raw_id"])
    start, end = evidence.get("span_start"), evidence.get("span_end")
    return (isinstance(raw, bytes) and evidence.get("span_unit") == "BYTE"
            and type(start) is int and type(end) is int
            and 0 <= start < end <= len(raw)
            and hashlib.sha256(raw).hexdigest() == evidence["sha256"])


def _mapping(value):
    return value if isinstance(value, Mapping) else {}


def _proof(value, state="VERIFIED"):
    value = _mapping(value)
    return (value.get("state") == state and _text(value.get("version"))
            and _text(value.get("evidence_ref")))


def _mint(value):
    # Decode exact base58 token to 32 bytes; never repair/casefold identifiers.
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    if not isinstance(value, str) or not 32 <= len(value) <= 44:
        return False
    n = 0
    for char in value:
        if char not in alphabet:
            return False
        n = n * 58 + alphabet.index(char)
    return len(value) - len(value.lstrip("1")) + (n.bit_length() + 7) // 8 == 32


def _ms(value):
    return type(value) is int and value >= 0


def _timing(record, cutoff, reasons):
    timing = _mapping(record.get("availability"))
    if not timing:
        reasons.append("AVAILABILITY_MISSING")
        return None
    initial = len(reasons)
    if timing.get("clock") != "UTC_KNOWN":
        reasons.append("CLOCK_UNRESOLVED")
    if timing.get("decision_cutoff_ms") != cutoff:
        reasons.append("CUTOFF_MISMATCH")
    evidence = _mapping(record.get("evidence"))
    if any(timing.get(k) != evidence.get(k) for k in ("content_id", "content_version")):
        reasons.append("CONTENT_VERSION_MISMATCH")
    values = [timing.get(k) for k in
              ("publish_ms", "retrieved_ms", "claim_available_ms", "media_available_ms")]
    modality = timing.get("modality")
    if modality not in ("TEXT", "ASR", "VIDEO", "AUDIO"):
        reasons.append("MODALITY_UNRESOLVED")
    if modality == "ASR" or "asr_completed_ms" in timing:
        values.append(timing.get("asr_completed_ms"))
    if modality in ("ASR", "VIDEO", "AUDIO"):
        epoch = _mapping(timing.get("media_epoch"))
        if not _proof(epoch, "RESOLVED"):
            reasons.append("MEDIA_EPOCH_UNRESOLVED")
        values.append(epoch.get("available_at_ms"))
    for key in ("source_confidence", "speaker_confidence", "entity_confidence",
                "rights", "mint_link", "numeric_snapshot", "origin"):
        values.append(_mapping(record.get(key)).get("available_at_ms"))
    dependencies = _mapping(timing.get("dependencies"))
    if not dependencies:
        reasons.append("DEPENDENCIES_MISSING")
    for key, dependency in dependencies.items():
        if not _text(key) or not _proof(dependency):
            reasons.append("DEPENDENCY_UNVERIFIED")
        values.append(_mapping(dependency).get("available_at_ms"))
    order = _mapping(timing.get("same_time_order"))
    if order:
        values.append(order.get("available_at_ms"))
    proof_values = [_mapping(record.get(k)) for k in
                    ("source_confidence", "speaker_confidence", "entity_confidence",
                     "rights", "mint_link", "numeric_snapshot", "origin")]
    proof_values.extend(_mapping(v) for v in dependencies.values())
    if modality in ("ASR", "VIDEO", "AUDIO"):
        proof_values.append(_mapping(timing.get("media_epoch")))
    if order:
        proof_values.append(order)
    if any(p.get("clock") != "UTC_KNOWN" for p in proof_values):
        reasons.append("CLOCK_UNRESOLVED")
    if any(value is None for value in values):
        reasons.append("AVAILABILITY_MISSING")
    delay = timing.get("compute_delay_ms")
    if not _ms(delay) or any(v is not None and not _ms(v) for v in values):
        reasons.append("AVAILABILITY_INVALID")
    if len(reasons) != initial or not _ms(cutoff):
        return None
    available = max(values) + delay
    if available > cutoff:
        reasons.append("AFTER_CUTOFF")
    elif available == cutoff and (not _proof(order)
                                  or order.get("decision_cutoff_ms") != cutoff):
        reasons.append("SAME_TIME_ORDER_UNPROVEN")
    return available


def _origin(record, reasons):
    origin = _mapping(record.get("origin"))
    if not origin:
        reasons.append("ORIGIN_RELATIONS_MISSING")
        return None, [], False
    group = origin.get("canonical_content_id")
    edges = origin.get("relations")
    content = _mapping(record.get("evidence")).get("content_id")
    valid = (_proof(origin) and _text(group) and isinstance(edges, list))
    if valid:
        valid = all(isinstance(edge, Mapping)
                    and edge.get("kind") in ("REPOST_OF", "DUPLICATE_OF", "AMPLIFIES")
                    and _text(edge.get("target_content_id")) for edge in edges)
    if valid and group != content:
        valid = any(e["target_content_id"] == group
                    and e["kind"] in ("REPOST_OF", "DUPLICATE_OF") for e in edges)
    if valid and group == content:
        valid = not any(e["kind"] in ("REPOST_OF", "DUPLICATE_OF") for e in edges)
    if not valid:
        reasons.append("ORIGIN_RELATIONS_INVALID")
    return group if valid else None, edges if isinstance(edges, list) else [], bool(
        valid and group == content and not edges)


def _same(left, right):
    """Exact typed JSON equality: bool/float timestamps never equal integers."""
    if isinstance(left, Mapping) and isinstance(right, Mapping):
        return left.keys() == right.keys() and all(_same(left[k], right[k]) for k in left)
    if isinstance(left, list) and isinstance(right, list):
        return len(left) == len(right) and all(_same(a, b) for a, b in zip(left, right))
    return type(left) is type(right) and left == right


def _metadata_valid(record, cutoff):
    """Closed declaration schema; subjects live in the registry, not ad hoc fields."""
    common = {"state", "version", "evidence_ref", "clock", "available_at_ms"}
    extras = {"source_confidence": set(), "speaker_confidence": set(),
              "entity_confidence": set(), "rights": {"scope"},
              "mint_link": {"mint", "chain", "content_id", "content_version"},
              "numeric_snapshot": {"snapshot_id", "snapshot_version", "mint", "chain",
                                   "decision_cutoff_ms", "observed_at_ms"},
              "origin": {"canonical_content_id", "relations"}}
    for key, fields in extras.items():
        if set(_mapping(record.get(key))) != common | fields:
            return False
    snapshot = _mapping(record.get("numeric_snapshot"))
    observed, available = snapshot.get("observed_at_ms"), snapshot.get("available_at_ms")
    if (snapshot.get("chain") != "solana" or not _text(snapshot.get("snapshot_version"))
            or not _ms(observed) or not _ms(available) or not _ms(cutoff)
            or observed > available or observed > cutoff):
        return False
    timing = _mapping(record.get("availability"))
    required_timing = {"clock", "decision_cutoff_ms", "content_id", "content_version",
                       "modality", "publish_ms", "retrieved_ms", "claim_available_ms",
                       "media_available_ms", "compute_delay_ms", "dependencies"}
    if (not required_timing <= set(timing)
            or set(timing) - required_timing - {"asr_completed_ms", "media_epoch", "same_time_order"}
            or not _ms(timing.get("decision_cutoff_ms"))
            or not _ms(snapshot.get("decision_cutoff_ms"))):
        return False
    for value in _mapping(timing.get("dependencies")).values():
        if set(_mapping(value)) != common:
            return False
    for key, fields in (("media_epoch", set()), ("same_time_order", {"decision_cutoff_ms"})):
        if key in timing and set(_mapping(timing[key])) != common | fields:
            return False
    if "same_time_order" in timing and not _ms(
            _mapping(timing["same_time_order"]).get("decision_cutoff_ms")):
        return False
    return True


def _registry_valid(record, cutoff, registry):
    if not isinstance(registry, Mapping):
        return False
    evidence = _mapping(record.get("evidence"))
    timing = _mapping(record.get("availability"))
    link = _mapping(record.get("mint_link"))
    snapshot = _mapping(record.get("numeric_snapshot"))
    subject = {k: record.get(k) for k in ("source_id", "speaker_id", "entity_id")}
    if not all(_text(v) for v in subject.values()):
        return False
    subject.update(content_id=evidence.get("content_id"),
                   content_version=evidence.get("content_version"),
                   chain=link.get("chain"), mint=link.get("mint"),
                   snapshot_id=snapshot.get("snapshot_id"), decision_cutoff_ms=cutoff)
    proofs = {k: _mapping(record.get(k)) for k in ("source_confidence", "speaker_confidence",
              "entity_confidence", "rights", "mint_link", "numeric_snapshot", "origin")}
    for key, value in _mapping(timing.get("dependencies")).items():
        if not _text(key):
            return False
        proofs["dependency:" + key] = _mapping(value)
    for key in ("media_epoch", "same_time_order"):
        if key in timing:
            proofs[key] = _mapping(timing[key])
    for kind, declaration in proofs.items():
        ref = declaration.get("evidence_ref")
        if not _text(ref):
            return False
        entry = _mapping(registry.get(ref))
        if (entry.get("kind") != kind or not _same(entry.get("subject"), subject)
                or not _same(entry.get("evidence"), evidence)
                or not _same(entry.get("declaration"), declaration)
                or not _same(entry.get("availability"), timing)):
            return False
    return True


def evaluate_context(record, *, cutoff_ms, raw_objects, trusted_proofs=None):
    """Return missing proof reasons; legacy labels have no authority.

    `trusted_proofs` is an independently authenticated upstream registry, never
    extracted from `record`. Each evidence_ref resolves to a typed attestation
    with exact subject, evidence (raw hash/BYTE interval), declaration and full
    availability metadata. All must match. No registry means STRUCTURAL_ONLY
    at best, never eligibility; available_at_ms then describes declarations only.

    The caller owns registry authentication and immutable snapshot loading.
    Passing a registry synthesized from the candidate violates this trust
    boundary. This pure gate performs no network, signature or semantic/legal
    verification; REGISTRY_VERIFIED means matched trusted attestations only.
    """
    reasons = []
    if not _ms(cutoff_ms):
        reasons.append("DECISION_CUTOFF_UNRESOLVED")
    evidence = _mapping(record.get("evidence"))
    if record.get("evidence") is None:
        reasons.append("RAW_EVIDENCE_MISSING")
    elif not _raw_valid(evidence, raw_objects):
        reasons.append("RAW_EVIDENCE_INVALID")
    source_id = record.get("source_id") if (
        _text(record.get("source_id")) and _proof(record.get("source_confidence"))) else None

    if source_id is None:
        reasons.append("SOURCE_UNKNOWN")
    for name in ("source", "speaker", "entity"):
        value = record.get(name + "_confidence")
        if value is None:
            reasons.append(name.upper() + "_CONFIDENCE_MISSING")
        elif not _proof(value):
            reasons.append(name.upper() + "_CONFIDENCE_UNVERIFIED")
    rights = _mapping(record.get("rights"))
    if not _proof(rights, "CLEARED") or rights.get("scope") != "CONTEXT_ASSEMBLY":
        reasons.append("RIGHTS_UNRESOLVED")
    link = _mapping(record.get("mint_link"))
    if (not _proof(link) or not _mint(link.get("mint"))
            or link.get("chain") != "solana"
            or not evidence or any(link.get(k) != evidence.get(k)
                                   for k in ("content_id", "content_version"))):
        reasons.append("MINT_LINK_UNVERIFIED")
    snapshot = _mapping(record.get("numeric_snapshot"))
    if not snapshot:
        reasons.append("NUMERIC_SNAPSHOT_MISSING")
    else:
        if not _proof(snapshot) or not _text(snapshot.get("snapshot_id")):
            reasons.append("NUMERIC_SNAPSHOT_UNVERIFIED")
        if snapshot.get("mint") != link.get("mint"):
            reasons.append("MINT_MISMATCH")
        if snapshot.get("decision_cutoff_ms") != cutoff_ms:
            reasons.append("CUTOFF_MISMATCH")
    available = _timing(record, cutoff_ms, reasons)
    group, edges, independent = _origin(record, reasons)
    if trusted_proofs is not None and not _metadata_valid(record, cutoff_ms):
        reasons.append("PROOF_METADATA_INVALID")
    structural_valid = not reasons
    registry_valid = _registry_valid(record, cutoff_ms, trusted_proofs)
    if trusted_proofs is not None and not registry_valid:
        reasons.append("PROOF_REGISTRY_MISMATCH")
    elif structural_valid and not registry_valid:
        reasons.append("TRUSTED_PROOFS_MISSING")
    if not registry_valid or not structural_valid:
        source_id = None
    return {
        "context_eligible": not reasons, "training_admitted": False,
        "structural_valid": structural_valid,
        "verification_status": ("REGISTRY_VERIFIED" if not reasons else
                                "STRUCTURAL_ONLY" if structural_valid else "INVALID"),
        "available_at_ms": available, "source_id": source_id,
        "source_null_reason": "SOURCE_UNKNOWN" if source_id is None else None,
        "reason_codes": list(dict.fromkeys(reasons)),
        "testimony_group": group, "graph_relations": edges,
        "independent_testimony": independent and not reasons,
    }


def audit_creator_claims(path, *, limit=50):
    """Read first N physical JSONL rows, hash whole source, emit metadata only.

    No legacy field is renamed into a verified proof. No cutoff is invented.
    Hashing reads the remainder as opaque bytes, without parsing further rows.
    """
    import json
    from collections import Counter
    from pathlib import Path

    if type(limit) is not int or not 1 <= limit <= 50:
        raise ValueError("development audit limit must be 1..50")
    path = Path(path)
    source_hash, prefix_hash = hashlib.sha256(), hashlib.sha256()
    rows, counts = [], Counter()
    metadata_keys = ("content_id", "schema_version", "pipeline_version", "run_uuid",
                     "account_handle", "platform", "entity_type", "primary_mint",
                     "resolution_method", "resolution_confidence", "admission_status",
                     "temporal_class", "publish_time_ms", "first_seen_ms",
                     "amplification_cluster_id", "originality_confidence")
    with path.open("rb") as handle:
        for index in range(limit):
            line = handle.readline()
            if not line:
                break
            source_hash.update(line)
            prefix_hash.update(line)
            record = json.loads(line)
            if not isinstance(record, dict):
                raise ValueError(f"row {index + 1} is not an object")
            # Strip prose, retain explicit contract fields only if actually present.
            gate_keys = ("evidence", "source_id", "source_confidence", "speaker_confidence",
                         "entity_confidence", "rights", "mint_link", "numeric_snapshot",
                         "availability", "origin")
            candidate = {k: record[k] for k in gate_keys if k in record}
            result = evaluate_context(candidate, cutoff_ms=record.get("decision_cutoff_ms"),
                                      raw_objects={})
            counts.update(result["reason_codes"])
            rows.append({"line": index + 1, "claim_id": record.get("claim_id"),
                         "row_sha256": hashlib.sha256(line).hexdigest(),
                         "legacy_metadata": {k: record[k] for k in metadata_keys if k in record},
                         "context_eligible": result["context_eligible"],
                         "reason_codes": result["reason_codes"]})
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            source_hash.update(chunk)
    return {"scope": "DEVELOPMENT_EXPOSED_METADATA_ONLY", "source_path": str(path),
            "source_sha256": source_hash.hexdigest(), "prefix_sha256": prefix_hash.hexdigest(),
            "rows_audited": len(rows), "requested_limit": limit,
            "context_eligible_count": sum(r["context_eligible"] for r in rows),
            "training_admitted_count": 0, "reason_counts": dict(sorted(counts.items())),
            "rows": rows}


if __name__ == "__main__":
    import argparse
    import json

    parser = argparse.ArgumentParser(description="Read-only, development-only creator-claim audit")
    parser.add_argument("source")
    parser.add_argument("--limit", type=int, default=50)
    args = parser.parse_args()
    print(json.dumps(audit_creator_claims(args.source, limit=args.limit), indent=2))

