"""Development-only projection of Rust raw_recorder v1; not source admission.

raw_record_sha256 hashes supplied NDJSON bytes INCLUDING any line terminator;
source_sha256 identifies the compressed source object, not the decoded payload.
No transaction/account economics, participant attribution or finality inference.
"""
import base64
import binascii
from collections import Counter
import hashlib
import io
import json
import math
from pathlib import Path
import re


class RawProjectionError(ValueError):
    """Fail-closed structural rejection with a stable machine-readable reason."""

    def __init__(self, reason):
        self.reason = reason
        super().__init__(reason)


def _require(condition, reason):
    if not condition:
        raise RawProjectionError(reason)


def _uint(value, name):
    _require(type(value) is int and 0 <= value < 2**64, "invalid_" + name)
    return value


def _text(value, name):
    _require(isinstance(value, str) and bool(value.strip()), "invalid_" + name)
    _unicode_scalars(value)
    return value


def _texts(value, name):
    _require(isinstance(value, list), "invalid_" + name)
    for item in value:
        _text(item, name)
    return value


def _mapping(value, name):
    _require(isinstance(value, dict), "invalid_" + name)
    return value


def _unicode_scalars(value):
    """Validate JSON keys/values without repairing strings or recursive walking."""
    pending = [value]
    while pending:
        item = pending.pop()
        if isinstance(item, str):
            try:
                item.encode("utf-8")
            except UnicodeEncodeError as exc:
                raise RawProjectionError("invalid_unicode_scalar") from exc
        elif isinstance(item, dict):
            pending.extend(item.keys())
            pending.extend(item.values())
        elif isinstance(item, list):
            pending.extend(item)


def _object(pairs):
    result = {}
    for key, value in pairs:
        _require(key not in result, "duplicate_json_key")
        result[key] = value
    return result


def _nonfinite(_):
    raise RawProjectionError("nonfinite_json_number")


def _integer(token):
    # Bound conversion before int(): independent of Python's digit-limit setting.
    # Recorder integer domain is the union of signed i64 and unsigned u64.
    _require(len(token.lstrip("-")) <= 20, "json_integer_out_of_range")
    value = int(token)
    _require(-(2**63) <= value < 2**64, "json_integer_out_of_range")
    return value


def _float(token):
    value = float(token)
    _require(math.isfinite(value), "nonfinite_json_number")
    return value


def _context(source_path, source_sha256, revision, source_commitment):
    _text(source_path, "source_path")
    _require(isinstance(source_sha256, str) and
             re.fullmatch(r"[0-9a-fA-F]{64}", source_sha256) is not None,
             "invalid_source_sha256")
    _uint(revision, "revision")
    _require(source_commitment in (None, "processed", "confirmed", "finalized",
                                    "PROCESSED", "CONFIRMED", "FINALIZED"),
             "invalid_source_commitment")


def project_record(raw_record, *, source_path, source_sha256, revision=0,
                   source_commitment=None):
    """Project an exact byte record, not a trade or complete consumer event.

    source_sha256 and optional commitment are caller-supplied provenance assertions;
    project_bounded_file verifies the source hash itself. No raw row contains a
    subscription commitment. A slot status is preserved separately, never promoted
    into transaction finality. Missing transaction message/meta fail closed because
    the complete account-key index space cannot be projected. Literal source keys
    and signatures are retained, not cryptographically validated or repaired.
    Unknown non-envelope payload fields are neither interpreted nor certified.
    """
    _context(source_path, source_sha256, revision, source_commitment)
    _require(type(raw_record) is bytes, "raw_record_must_be_bytes")
    try:
        row = json.loads(raw_record.decode("utf-8"), object_pairs_hook=_object,
                         parse_constant=_nonfinite, parse_float=_float,
                         parse_int=_integer)
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as exc:
        raise RawProjectionError("invalid_json") from exc
    _unicode_scalars(row)
    _mapping(row, "envelope")
    kind = row.get("record_type")
    _require(kind in ("transaction", "account", "slot", "block_meta"),
             "unknown_record_type")
    for field in ("slot", "recv_unix_ms", "record_index"):
        _uint(row.get(field), field)
    payload = _mapping(row.get("payload"), "payload")
    raw_sha = hashlib.sha256(raw_record).hexdigest()
    identity = json.dumps([source_path, source_sha256, row["record_index"], raw_sha],
                          ensure_ascii=True, separators=(",", ":")).encode()
    event = {
        "schema_version": "raw_projection_v1",
        "event_id": "raw_projection:" + hashlib.sha256(identity).hexdigest(),
        "projection_scope": "envelope_only_development",
        "training_eligible": False,
        "source_admission": "unadmitted",
        "source_path": source_path,
        "source_sha256": source_sha256,
        "raw_record_sha256": raw_sha,
        "raw_record_hash_scope": "exact_ndjson_bytes_including_line_terminator",
        "record_index": row["record_index"],
        "record_type": kind,
        "revision": revision,
        "revision_basis": ("adapter_initial_projection" if revision == 0 else
                           "caller_supplied_projection_revision"),
        "slot": row["slot"],
        "observed_at_unix_ms": row["recv_unix_ms"],
        "observation_clock": "recorder_host_unix_ms",
        "block_time_unix_s": None,
        "source_commitment": source_commitment,
        "slot_status": None,
        "signature": None,
        "signatures": None,
        "source_payload_raw_hash": None,
        "static_account_keys": None,
        "loaded_writable_account_keys": None,
        "loaded_readonly_account_keys": None,
        "account_keys": None,
        "transaction_index": None,
        "account_pubkey": None,
        "account_write_version": None,
        "blockhash": None,
    }
    if kind == "transaction":
        signature = _text(payload.get("signature_b58"), "signature_b58")
        signatures = _texts(payload.get("signatures_b58"), "signatures_b58")
        _require(bool(signatures) and signatures[0] == signature, "signature_mismatch")
        message = _mapping(payload.get("message"), "message")
        meta = _mapping(payload.get("meta"), "meta")
        static = _texts(message.get("account_keys_b58"), "account_keys_b58")
        writable = _texts(meta.get("loaded_writable_addresses_b58"),
                          "loaded_writable_addresses_b58")
        readonly = _texts(meta.get("loaded_readonly_addresses_b58"),
                          "loaded_readonly_addresses_b58")
        event.update(signature=signature, signatures=signatures,
                     transaction_index=_uint(payload.get("tx_index"), "tx_index"),
                     static_account_keys=static, loaded_writable_account_keys=writable,
                     loaded_readonly_account_keys=readonly,
                     account_keys=static + writable + readonly,
                     source_payload_raw_hash=_text(payload.get("raw_hash"), "raw_hash"))
    elif kind == "account":
        signature = payload.get("txn_signature_b58")
        _require("txn_signature_b58" in payload, "missing_txn_signature_b58")
        if signature is not None:
            _text(signature, "txn_signature_b58")
        for field in ("lamports", "rent_epoch", "write_version", "data_len"):
            _uint(payload.get(field), field)
        _text(payload.get("owner_b58"), "owner_b58")
        _require(type(payload.get("executable")) is bool, "invalid_executable")
        try:
            _require(isinstance(payload.get("data_b64"), str), "invalid_data_b64")
            data = base64.b64decode(payload["data_b64"], validate=True)
        except (ValueError, binascii.Error) as exc:
            raise RawProjectionError("invalid_data_b64") from exc
        _require(len(data) == payload["data_len"], "data_length_mismatch")
        event.update(signature=signature,
                     account_pubkey=_text(payload.get("pubkey_b58"), "pubkey_b58"),
                     account_write_version=payload["write_version"],
                     source_payload_raw_hash=_text(payload.get("raw_hash"), "raw_hash"))
    else:
        _uint(payload.get("slot"), "slot")
        _require(payload["slot"] == row["slot"], "slot_mismatch")
        if kind == "slot":
            _require(payload.get("status") in ("Processed", "Confirmed", "Finalized"),
                     "unknown_slot_status")
            _require("parent" in payload, "missing_parent")
            if payload["parent"] is not None:
                _uint(payload["parent"], "parent")
            event["slot_status"] = payload["status"]
        else:
            _require("block_time" in payload, "missing_block_time")
            block_time = payload["block_time"]
            _require(block_time is None or (type(block_time) is int and
                     -(2**63) <= block_time < 2**63), "invalid_block_time")
            for field in ("parent_slot", "executed_transaction_count", "entries_count"):
                _uint(payload.get(field), field)
            _require("block_height" in payload, "missing_block_height")
            if payload["block_height"] is not None:
                _uint(payload["block_height"], "block_height")
            _text(payload.get("parent_blockhash"), "parent_blockhash")
            event.update(block_time_unix_s=block_time,
                         blockhash=_text(payload.get("blockhash"), "blockhash"))
    event["null_reasons"] = {
        key: ("source_commitment_not_supplied" if key == "source_commitment" else
              "source_value_missing" if key == "block_time_unix_s" and kind == "block_meta"
              or key == "signature" and kind == "account" else
              "not_applicable_or_not_recorded_for_record_type")
        for key, value in event.items() if value is None
    }
    return event


def _file_sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def project_bounded_file(source_path, *, output_dir, limit=100,
                         source_commitment=None):
    """Create a NEW development directory with <=100 envelope projections.

    Reads only a decompressed prefix (zstd may prefetch compressed blocks). Hashes
    the entire compressed object before/after; this is NOT a whole-file decode or
    coverage certification. The successful report is written last. An I/O failure
    leaves no success report. A rejected row is counted with hash/reason, never
    converted to a valid event. No rights or evaluation admission is performed.
    """
    import zstandard

    _require(type(limit) is int and 1 <= limit <= 100, "invalid_limit")
    _text(source_path, "source_path")
    source = Path(source_path).resolve()
    output = Path(output_dir).resolve()
    _require(not output.is_relative_to(source.parent) and
             not source.is_relative_to(output), "output_overlaps_source_directory")
    if output.exists():
        raise FileExistsError(output)
    _context(source_path, "0" * 64, 0, source_commitment)
    before = source.stat()
    source_sha = _file_sha(source)
    events, failures = [], []
    eof = False
    with source.open("rb") as compressed:
        with io.BufferedReader(zstandard.ZstdDecompressor().stream_reader(compressed)) as reader:
            for line_index in range(limit):
                raw = reader.readline(8 * 1024 * 1024 + 1)
                if not raw:
                    eof = True
                    break
                _require(len(raw) <= 8 * 1024 * 1024, "record_size_limit_exceeded")
                try:
                    event = project_record(raw, source_path=source_path,
                                           source_sha256=source_sha,
                                           source_commitment=source_commitment)
                except RawProjectionError as exc:
                    failures.append(dict(line_index=line_index, reason=exc.reason,
                                         raw_record_sha256=hashlib.sha256(raw).hexdigest()))
                else:
                    event["source_line_index"] = line_index
                    events.append(event)
    after_sha = _file_sha(source)
    after = source.stat()
    _require(source_sha == after_sha and before.st_size == after.st_size and
             before.st_mtime_ns == after.st_mtime_ns, "source_changed_during_projection")
    rows_read = len(events) + len(failures)
    report = dict(schema_version="raw_projection_report_v1", source_path=source_path,
                  source_sha256=source_sha, source_sha256_after=after_sha,
                  source_size_bytes=after.st_size, source_unchanged=True,
                  adapter_sha256=_file_sha(Path(__file__)),
                  source_commitment=source_commitment, training_eligible=False,
                  source_admission="unadmitted", projection_scope="envelope_only_development",
                  rows_read=rows_read, accepted=len(events), rejected=len(failures),
                  record_types=dict(Counter(e["record_type"] for e in events)),
                  limit=limit, limit_reached=rows_read == limit, eof_observed=eof,
                  failures=failures, full_source_decoded=False)
    output.mkdir(parents=True, exist_ok=False)
    encoded = b"".join((json.dumps(e, ensure_ascii=True, sort_keys=True) + "\n").encode()
                       for e in events)
    with (output / "events.jsonl").open("xb") as handle:
        handle.write(encoded)
    report["events_sha256"] = hashlib.sha256(encoded).hexdigest()
    report["events_path"] = str(output / "events.jsonl")
    with (output / "report.json").open("x", encoding="utf-8") as handle:
        json.dump(report, handle, indent=2, sort_keys=True)
        handle.write("\n")
    return report
