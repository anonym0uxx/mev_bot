"""Development-only exact legacy content joins; not source admission.

``raw_text`` bytes mean UTF-8 encoding of the decoded JSON string, with no
normalization/unescaping beyond JSON decoding. These are legacy text bytes,
NOT the original provider response. Original JSONL bytes have separate hashes.
"""
from __future__ import annotations

import hashlib
import json
import math
import os
import stat
import tempfile
from pathlib import Path

from north_star.fs_integrity import checked_path, stable_reader, _reject_link, _version

VERSION = "narrative_sources_v1"
TRANSFORM_VERSION = "narrative_sources_strict_v2"
MAX_JSON_DEPTH = 64


def _strict_record(raw):
    """Linear bounded validation, including ignored metadata and object keys."""
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError("duplicate object key")
            result[key] = value
        return result

    def reject_constant(_):
        raise ValueError("nonfinite number")

    def validate(value):
        if isinstance(value, str):
            value.encode("utf-8", errors="strict")
        elif isinstance(value, float) and not math.isfinite(value):
            raise ValueError("nonfinite number")
        elif isinstance(value, dict):
            for key, item in value.items():
                validate(key)
                validate(item)
        elif isinstance(value, list):
            for item in value:
                validate(item)

    try:
        text = raw.decode("utf-8", errors="strict")
        # Bound nesting BEFORE calling the recursive stdlib decoder. Brackets
        # in strings (including escaped quotes/backslashes) do not count.
        depth = 0
        quoted = escaped = False
        for char in text:
            if quoted:
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == '"':
                    quoted = False
            elif char == '"':
                quoted = True
            elif char in "[{":
                depth += 1
                if depth > MAX_JSON_DEPTH:
                    raise ValueError("nesting bound exceeded")
            elif char in "]}":
                depth -= 1
        obj = json.loads(text, object_pairs_hook=pairs, parse_constant=reject_constant)
        if not isinstance(obj, dict):
            raise ValueError("record must be an object")
        validate(obj)
        return obj
    except (ValueError, RecursionError) as error:
        raise ValueError("invalid JSONL record") from error


def _sha(data):
    return hashlib.sha256(data).hexdigest()


def _fingerprint(record):
    return _sha(json.dumps(record, sort_keys=True, ensure_ascii=True).encode("utf-8"))


def _scan(path, limit, stats, *, prefix=False, max_line_bytes=1_048_576,
          max_input_bytes=268_435_456):
    digest = hashlib.sha256()
    offset = 0
    with stable_reader(path) as (handle, _):
        for index in range(limit):
            raw = handle.readline(max_line_bytes + 1)
            if not raw:
                break
            if len(raw) > max_line_bytes or offset + len(raw) > max_input_bytes:
                raise ValueError("input byte bound exceeded")
            obj = _strict_record(raw)
            pointer = {"path": str(path), "line_number": index + 1,
                       "byte_offset": offset, "byte_length": len(raw),
                       "sha256": _sha(raw)}
            digest.update(raw)
            offset += len(raw)
            yield obj, pointer, raw
        else:
            if not prefix and handle.read(1):
                raise ValueError("content row bound exceeded; duplicates not fully checked")
    stats.update(path=str(path), rows=index + 1 if raw else index,
                 bytes_read=offset, sha256=digest.hexdigest(),
                 hash_scope="selected_prefix" if prefix else "complete_file",
                 scan_complete=not prefix)


def _timestamps(record):
    return {key: value for key, value in record.items()
            if key.endswith(("_ms", "_at", "_time", "_timestamp"))}


def _new_output(path):
    """Inspect lexical ancestors without resolving away unsafe links."""
    for component in [*reversed(path.parents), path]:
        try:
            info = component.lstat()
        except FileNotFoundError:
            continue
        _reject_link(component, info)
        if component == path:
            raise FileExistsError(str(path))
        if not stat.S_ISDIR(info.st_mode):
            raise ValueError(f"Non-directory ancestor: {component}")


def recover_sources(claims_path, contents_path, output_dir, *, claim_limit=50,
                    max_content_rows=100_000, max_line_bytes=1_048_576,
                    max_input_bytes=268_435_456, max_selected_bytes=33_554_432):
    """Scan a bounded claim prefix and the bounded complete content stream.

    Fail before publishing on scan overflow. Only selected exact IDs are kept
    in memory. The output directory must be new; original inputs are read-only.
    Validation precedes staging; a verified complete directory is atomically
    published without clobber on Windows. Other platforms fail closed.
    Caller-controlled trusted directory trees only: identity/timestamp checks
    detect observed changes, not hostile ABA/hardlink/ancestor races or changes
    after the final check. This is not a hostile-filesystem sandbox.
    IDs are nonempty Unicode strings compared literally, including whitespace
    and pathlike strings; they are never normalized or used as file paths.
    All scanned JSON must be UTF-8, unique-key, finite, surrogate-free, and
    at most MAX_JSON_DEPTH containers deep, even in unselected content rows.
    """
    limits = dict(claim_limit=claim_limit, max_content_rows=max_content_rows,
                  max_line_bytes=max_line_bytes, max_input_bytes=max_input_bytes,
                  max_selected_bytes=max_selected_bytes)
    for name, value in limits.items():
        if type(value) is not int or value <= 0:
            raise ValueError(f"{name} must be a positive integer")
    claims_path, contents_path = Path(claims_path).absolute(), Path(contents_path).absolute()
    out = Path(output_dir).absolute()
    originals = {path: _version(checked_path(path)) for path in (claims_path, contents_path)}

    def check_inputs():
        for path, version in originals.items():
            if _version(checked_path(path)) != version:
                raise ValueError(f"File integrity changed: {path}")

    _new_output(out)
    stats = {"claims": {}, "contents": {}}
    bounds = dict(max_line_bytes=max_line_bytes, max_input_bytes=max_input_bytes)
    claims = []
    selected_bytes = 0
    for item in _scan(claims_path, claim_limit, stats["claims"], prefix=True, **bounds):
        selected_bytes += len(item[2])
        if selected_bytes > max_selected_bytes:
            raise ValueError("selected byte bound exceeded")
        for name in ("claim_id", "content_id"):
            if not isinstance(item[0].get(name), str) or not item[0][name]:
                raise ValueError(f"{name} identifier must be a nonempty string")
        claims.append(item)
    check_inputs()
    claim_groups = {}
    for claim, _, _ in claims:
        claim_groups.setdefault(claim["claim_id"], []).append(_fingerprint(claim))
    ids = {claim["content_id"] for claim, _, _ in claims}
    selected = {}
    for content, pointer, raw in _scan(contents_path, max_content_rows, stats["contents"], **bounds):
        if isinstance(content.get("content_id"), str) and content["content_id"] in ids:
            selected_bytes += len(raw)
            if selected_bytes > max_selected_bytes:
                raise ValueError("selected byte bound exceeded")
            selected.setdefault(content["content_id"], []).append((content, pointer, raw))
    check_inputs()
    files = {}
    blobs = {}

    def save(relative, data):
        if relative not in files:
            blobs[relative] = data
            files[relative] = {"sha256": _sha(data), "bytes": len(data)}

    rows = []
    for claim, claim_pointer, claim_raw in claims:
        matches = selected.get(claim["content_id"], [])
        conflict = len({_fingerprint(c) for c, _, _ in matches}) > 1
        content, pointer, raw = matches[0] if matches and not conflict else ({}, None, None)
        reasons = []
        if len(matches) > 1:
            reasons.append("DUPLICATE_CONTENT_ID")
        if conflict:
            reasons.append("CONTENT_ID_CONFLICT")
        group = claim_groups[claim["claim_id"]]
        if len(group) > 1:
            reasons.append("DUPLICATE_CLAIM_ID")
        if len(set(group)) > 1:
            reasons.append("CLAIM_ID_CONFLICT")
        provider_refs = [{k: v for k, v in c.items()
                          if k in ("original_provider_blob", "provider_blob_path", "raw_blob_path") and v}
                         for c in [claim] + [c for c, _, _ in matches]]
        provider_refs = [ref for ref in provider_refs if ref]
        reasons.append("ORIGINAL_PROVIDER_BLOB_UNVERIFIED" if provider_refs else "ORIGINAL_PROVIDER_BLOB_MISSING")
        if not matches:
            reasons.append("CONTENT_ID_NOT_FOUND")
        text = content.get("raw_text")
        text_object = evidence = None
        if not isinstance(text, str) or not text:
            text = None
            if not conflict:
                reasons.append("TEXT_MISSING")
        else:
            encoded = text.encode("utf-8")
            digest = _sha(encoded)
            raw_id = f"objects/{digest}.utf8"
            save(raw_id, encoded)
            text_object = {"raw_id": raw_id, "sha256": digest, "bytes": len(encoded),
                           "byte_semantics": "legacy_raw_text_decoded_json_utf8"}
            prefix = claim.get("claim_text")
            if isinstance(prefix, str) and prefix and text.startswith(prefix):
                evidence = {"raw_id": raw_id, "content_id": claim["content_id"],
                            "content_version": f"legacy-record-sha256:{pointer['sha256']}",
                            "sha256": digest, "span_start": 0,
                            "span_end": len(prefix.encode("utf-8")), "span_unit": "BYTE"}
            else:
                reasons.append("CLAIM_PREFIX_NOT_EXACT")
        records = [(claim_pointer, claim_raw)] + [(p, r) for _, p, r in matches]
        for ptr, record_bytes in records:
            ptr["artifact_path"] = f"records/{ptr['sha256']}.jsonl"
            save(ptr["artifact_path"], record_bytes)
        rows.append({
            "schema_version": VERSION, "claim_id": claim["claim_id"],
            "content_id": claim["content_id"], "raw_text": text,
            "evidence": evidence, "text_object": text_object,
            "claim_record_pointer": claim_pointer, "content_record_pointers": [p for _, p, _ in matches],
            "legacy_claim_metadata": {k: v for k, v in claim.items() if k != "claim_text"},
            "legacy_content_metadata": {k: v for k, v in content.items()
                                        if k not in ("raw_text", "normalized_text")},
            "legacy_timestamps_unverified": {"claim": _timestamps(claim), "content": _timestamps(content)},
            "availability_verified": False, "available_at_ms": None,
            "source_state": "UNKNOWN", "rights_state": "UNKNOWN", "temporal_state": "UNKNOWN",
            "admitted": False, "split": "development",
            "original_provider_blob": {"state": "UNVERIFIED_REFERENCE" if provider_refs else "MISSING",
                                       "references_unverified": provider_refs,
                                       "scope": "supplied_legacy_rows_only"},
            "reason_codes": reasons,
        })
    payload = b"".join(json.dumps(row, ensure_ascii=True, sort_keys=True).encode("utf-8") + b"\n"
                       for row in rows)
    save("recovered_sources.jsonl", payload)
    with stable_reader(Path(__file__)) as (source, _):
        transform_sha = _sha(source.read())
    manifest = {"schema_version": VERSION, "split": "development", "admitted": False,
                "transform": {"version": TRANSFORM_VERSION, "sha256": transform_sha},
                "inputs": stats, "bounds": limits, "selected_record_bytes": selected_bytes,
                "counts": {"claims": len(rows),
                                             "unique_claim_ids": len(claim_groups),
                                             "conflicting_content_claims": sum("CONTENT_ID_CONFLICT" in r["reason_codes"] for r in rows),
                                             "recovered_text": sum(r["raw_text"] is not None for r in rows),
                                             "exact_prefix_spans": sum(r["evidence"] is not None for r in rows)},
                "files": files}
    manifest_text = json.dumps(manifest, indent=2, sort_keys=True, allow_nan=False)
    check_inputs()
    _new_output(out)
    # Python's Windows rename is atomic and refuses ANY existing destination.
    # POSIX rename can replace an empty directory: never use it as a fallback.
    if os.name != "nt":
        raise OSError("atomic no-clobber directory publication requires Windows")
    out.parent.mkdir(parents=True, exist_ok=True)
    _new_output(out)
    with tempfile.TemporaryDirectory(prefix=f".{out.name}-", suffix=".tmp", dir=out.parent) as temporary:
        stage = Path(temporary)
        (stage / "objects").mkdir()
        (stage / "records").mkdir()
        for relative, data in blobs.items():
            if (stage / relative).write_bytes(data) != len(data):
                raise OSError("short artifact write")
        if (stage / "manifest.json").write_text(manifest_text, encoding="utf-8", newline="\n") != len(manifest_text):
            raise OSError("short manifest write")
        for relative, data in {**blobs, "manifest.json": manifest_text.encode("utf-8")}.items():
            with stable_reader(stage / relative) as (source, _):
                actual = source.read(len(data) + 1)
            if actual != data:
                raise OSError("artifact readback mismatch")
        check_inputs()
        _new_output(out)
        os.rename(stage, out)  # no replace flag; complete directory only
    return manifest
