"""Bounded development envelope Parquet, NOT semantic events or admission.

All columns are raw_adapter projections; nulls and literal strings survive.
Caller-controlled trusted trees only (fs_integrity's race limits apply).
"""
import hashlib
import json
import os
import re
from collections import Counter
from pathlib import Path
import tempfile

import pyarrow as pa
import pyarrow.parquet as pq

from north_star.fs_integrity import checked_path, stable_reader
from north_star.io import _publish_new

_NULLABLE_TEXT = ("source_commitment", "slot_status", "signature",
                  "source_payload_raw_hash", "account_pubkey", "blockhash")
_LISTS = ("signatures", "static_account_keys", "loaded_writable_account_keys",
          "loaded_readonly_account_keys", "account_keys")
_NULLABLE_UINT = ("transaction_index", "account_write_version")
ENVELOPE_SCHEMA = pa.schema(
    [pa.field(name, pa.string(), nullable=False) for name in (
        "schema_version", "event_id", "projection_scope", "source_admission",
        "source_path", "source_sha256", "raw_record_sha256", "raw_record_hash_scope",
        "record_type", "revision_basis", "observation_clock")]
    + [pa.field("training_eligible", pa.bool_(), nullable=False)]
    + [pa.field(name, pa.uint64(), nullable=False) for name in (
        "record_index", "revision", "slot", "observed_at_unix_ms", "source_line_index")]
    + [pa.field("block_time_unix_s", pa.int64())]
    + [pa.field(name, pa.string()) for name in _NULLABLE_TEXT]
    + [pa.field(name, pa.list_(pa.field("element", pa.string(), nullable=False)))
       for name in _LISTS]
    + [pa.field(name, pa.uint64()) for name in _NULLABLE_UINT]
    + [pa.field("null_reasons", pa.map_(pa.string(), pa.string()), nullable=False)],
    metadata={b"north_star_schema": b"development_envelope_partition_v1",
              b"projection_scope": b"envelope_only_development",
              b"source_admission": b"unadmitted", b"training_eligible": b"false"})


def _sha(path):
    with stable_reader(path) as (source, _):
        return hashlib.file_digest(source, "sha256").hexdigest()


def _encoded(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=True, allow_nan=False,
                      separators=(",", ":")).encode("utf-8")


def _object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _json(raw):
    def nonfinite(value):
        raise ValueError("nonfinite JSON value: " + value)
    return json.loads(raw, object_pairs_hook=_object, parse_constant=nonfinite)


def _digest(value):
    if type(value) is not str or re.fullmatch(r"[0-9a-fA-F]{64}", value) is None:
        raise ValueError("invalid literal digest")


def _event(event, report):
    if type(event) is not dict or set(event) != set(ENVELOPE_SCHEMA.names):
        raise ValueError("envelope columns mismatch")
    for field in ENVELOPE_SCHEMA:
        value = event[field.name]
        if value is None:
            if not field.nullable:
                raise ValueError("null required column: " + field.name)
            continue
        kind = field.type
        valid = True
        if pa.types.is_integer(kind):
            low, high = (0, 2**64) if pa.types.is_unsigned_integer(kind) else (-2**63, 2**63)
            valid = type(value) is int and low <= value < high
        elif pa.types.is_string(kind):
            valid = type(value) is str and bool(value.strip())
        elif pa.types.is_boolean(kind):
            valid = type(value) is bool
        elif pa.types.is_list(kind):
            valid = type(value) is list and all(type(v) is str and bool(v.strip()) for v in value)
        elif pa.types.is_map(kind):
            valid = type(value) is dict and all(type(k) is str and type(v) is str and v
                                                for k, v in value.items())
        if not valid:
            raise ValueError("invalid envelope type: " + field.name)
    constants = {"schema_version": "raw_projection_v1", "projection_scope": "envelope_only_development",
                 "source_admission": "unadmitted", "training_eligible": False,
                 "observation_clock": "recorder_host_unix_ms",
                 "raw_record_hash_scope": "exact_ndjson_bytes_including_line_terminator"}
    constants.update({key: report[key] for key in ("source_path", "source_sha256", "source_commitment")})
    if any(event[key] != value for key, value in constants.items()):
        raise ValueError("envelope development/provenance mismatch")
    for name in ("source_sha256", "raw_record_sha256"):
        _digest(event[name])
    identity = json.dumps([event["source_path"], event["source_sha256"], event["record_index"],
                           event["raw_record_sha256"]], ensure_ascii=True, separators=(",", ":")).encode()
    if event["event_id"] != "raw_projection:" + hashlib.sha256(identity).hexdigest():
        raise ValueError("envelope identity mismatch")
    if event["record_type"] not in ("transaction", "account", "slot", "block_meta"):
        raise ValueError("unknown envelope record type")
    if set(event["null_reasons"]) != {key for key, value in event.items() if value is None}:
        raise ValueError("null reasons mismatch")
    if event["source_line_index"] >= report["rows_read"]:
        raise ValueError("source line index out of range")
    return event


def _events(path, report, config):
    rows, lines, counts = 0, set(), Counter()
    with stable_reader(path) as (source, _):
        for line in iter(lambda: source.readline(config["max_line_bytes"] + 1), b""):
            if len(line) > config["max_line_bytes"] or rows >= config["max_rows"]:
                raise ValueError("input limit exceeded")
            if not line.endswith(b"\n"):
                raise ValueError("unfinished input line")
            event = _event(_json(line), report)
            index = event["source_line_index"]
            if index in lines or lines and index <= max(lines):
                raise ValueError("duplicate/unordered source line")
            lines.add(index)
            rows += 1
            counts[event["record_type"]] += 1
            yield event
    rejected = [failure["line_index"] for failure in report["failures"]]
    if (rows != report["accepted"] or dict(counts) != report["record_types"] or
            len(set(rejected)) != len(rejected) or lines.intersection(rejected) or
            lines.union(rejected) != set(range(report["rows_read"]))):
        raise ValueError("input row accounting mismatch")


def _inactive(path):
    if path.name != "events.jsonl":
        raise ValueError("only completed events.jsonl input is supported")
    for name in ("events.jsonl.active", "events.jsonl.pending", "report.json.pending",
                 "ACTIVE", ".active", ".lock"):
        if os.path.lexists(path.with_name(name)):
            raise ValueError("active/unfinished input marker")


def _report(path, max_rows, max_line_bytes):
    _inactive(path)
    if checked_path(path).st_size > max_rows * max_line_bytes:
        raise ValueError("input byte limit exceeded")
    with stable_reader(path.with_name("report.json")) as (source, info):
        if info.st_size > 1024 * 1024:
            raise ValueError("report size limit exceeded")
        raw = source.read(1024 * 1024 + 1)
        report = _json(raw)
    if (report.get("schema_version") != "raw_projection_report_v1" or
            report.get("source_unchanged") is not True or
            report.get("source_sha256") != report.get("source_sha256_after") or
            report.get("training_eligible") is not False or
            report.get("source_admission") != "unadmitted" or
            report.get("projection_scope") != "envelope_only_development" or
            report.get("full_source_decoded") is not False):
        raise ValueError("unfinished or non-development input report")
    for field in ("accepted", "rejected", "rows_read", "limit"):
        if type(report.get(field)) is not int or not 0 <= report[field] <= 100:
            raise ValueError("invalid report count")
    failures = report.get("failures")
    if type(failures) is not list or any(
            type(failure) is not dict or
            type(failure.get("line_index")) is not int or
            not 0 <= failure["line_index"] < report["rows_read"]
            for failure in failures):
        raise ValueError("invalid report rejection index")
    record_types = report.get("record_types")
    if type(record_types) is not dict or any(
            type(count) is not int or not 0 <= count <= report["accepted"]
            for count in record_types.values()):
        raise ValueError("invalid report record type count")
    if (report["accepted"] > max_rows or report["limit"] < 1 or
            report["rows_read"] != report["accepted"] + report["rejected"] or
            report["rows_read"] > report["limit"] or
            len(report.get("failures", [])) != report["rejected"]):
        raise ValueError("report count/limit mismatch")
    if (type(report.get("eof_observed")) is not bool or
            type(report.get("limit_reached")) is not bool or
            report["limit_reached"] != (report["rows_read"] == report["limit"]) or
            report["eof_observed"] == report["limit_reached"]):
        raise ValueError("unfinished report stop flags")
    for name in ("source_sha256", "source_sha256_after", "events_sha256", "adapter_sha256"):
        _digest(report[name])
    if Path(report["events_path"]).absolute() != path.absolute():
        raise ValueError("report events path mismatch")
    digest = _sha(path)
    if digest != report.get("events_sha256"):
        raise ValueError("input digest mismatch")
    return report, digest, hashlib.sha256(raw).hexdigest()


def _unchanged(events_path, provenance):
    _inactive(events_path)
    if (_sha(events_path) != provenance["events_sha256"] or
            _sha(events_path.with_name("report.json")) != provenance["report_sha256"]):
        raise ValueError("input changed during partition operation")


# Independent development ceiling, including Parquet overhead; never trust a receipt's size.
_MAX_OUTPUT_BYTES = 128 * 1024 * 1024


def _inspect(path, expected_rows, batch_rows):
    with stable_reader(path) as (source, info):
        if info.st_size > _MAX_OUTPUT_BYTES:
            raise ValueError("output byte limit exceeded")
        digest = hashlib.file_digest(source, "sha256").hexdigest()
        source.seek(0)
        parquet = pq.ParquetFile(source)
        if (not parquet.schema_arrow.equals(ENVELOPE_SCHEMA) or
                parquet.schema_arrow.metadata != ENVELOPE_SCHEMA.metadata):
            raise ValueError("partition schema mismatch")
        if parquet.metadata.num_rows != expected_rows:
            raise ValueError("partition row count mismatch")
        if parquet.num_row_groups > expected_rows:
            raise ValueError("partition batch bound mismatch")
        group_rows = [parquet.metadata.row_group(i).num_rows
                      for i in range(parquet.num_row_groups)]
        if any(not 1 <= count <= batch_rows for count in group_rows):
            raise ValueError("partition batch bound mismatch")
        if sum(group_rows) != expected_rows:
            raise ValueError("partition row count mismatch")
        rows, content = 0, hashlib.sha256()
        for batch in parquet.iter_batches(batch_size=batch_rows):
            if batch.num_rows > batch_rows or rows + batch.num_rows > expected_rows:
                raise ValueError("partition readback row limit exceeded")
            batch.validate(full=True)
            rows += batch.num_rows
            for row in batch.to_pylist():
                row["null_reasons"] = dict(row["null_reasons"])
                content.update(_encoded(row) + b"\n")
        if rows != expected_rows:
            raise ValueError("partition row count mismatch")
    return {"sha256": digest, "bytes": info.st_size, "rows": rows,
            "content_sha256": content.hexdigest()}


def _verify(path, provenance, rows, batch_rows):
    with stable_reader(path.with_name(path.name + ".receipt.json")) as (source, _):
        if os.fstat(source.fileno()).st_size > 1024 * 1024:
            raise ValueError("receipt size limit exceeded")
        receipt = _json(source.read(1024 * 1024 + 1))
    if receipt.get("provenance") != provenance:
        raise ValueError("partition provenance mismatch")
    expected = dict(_inspect(path, rows, batch_rows), provenance=provenance,
                    source_admission="unadmitted", training_eligible=False)
    if expected["content_sha256"] != provenance["content_sha256"]:
        raise ValueError("partition content mismatch")
    if receipt != expected:
        raise ValueError("partition receipt/integrity mismatch")
    return receipt


def write_partition(events_path, output_path, *, batch_rows=32, max_rows=100,
                    max_line_bytes=1024 * 1024, compression="zstd"):
    """Write one bounded input into a new Parquet file and commit receipt."""
    for name, value, ceiling in (("batch_rows", batch_rows, 100),
                                  ("max_rows", max_rows, 100),
                                  ("max_line_bytes", max_line_bytes, 8 * 1024 * 1024)):
        if type(value) is not int or not 1 <= value <= ceiling:
            raise ValueError("invalid config: " + name)
    if compression not in ("zstd", "snappy", "NONE"):
        raise ValueError("invalid config: compression")
    events_path, output_path = Path(events_path), Path(output_path)
    report, events_sha, report_sha = _report(events_path, max_rows, max_line_bytes)
    config = {"batch_rows": batch_rows, "max_rows": max_rows,
              "max_line_bytes": max_line_bytes, "compression": compression}
    content = hashlib.sha256()
    for event in _events(events_path, report, config):
        content.update(_encoded(event) + b"\n")
    provenance = {"events_sha256": events_sha,
                  "report_sha256": report_sha, "config": config,
                  "config_sha256": hashlib.sha256(_encoded(config)).hexdigest(),
                  "source_path": report["source_path"],
                  "source_sha256": report["source_sha256"],
                  "content_sha256": content.hexdigest(),
                  "schema_sha256": hashlib.sha256(ENVELOPE_SCHEMA.serialize().to_pybytes()).hexdigest(),
                  "schema_version": "development_envelope_partition_v1",
                  "pyarrow_version": pa.__version__,
                  "writer_sha256": _sha(Path(__file__))}
    exists = checked_path(output_path, allow_missing=True, create_parents=True) is not None
    receipt_path = output_path.with_name(output_path.name + ".receipt.json")
    receipt_exists = checked_path(receipt_path, allow_missing=True) is not None
    if exists != receipt_exists:
        raise ValueError("uncommitted output/receipt orphan; manual recovery required")
    if exists:
        receipt = _verify(output_path, provenance, report["accepted"], batch_rows)
        _unchanged(events_path, provenance)
        return receipt
    fd, name = tempfile.mkstemp(prefix=output_path.name + ".", suffix=".pending",
                                dir=output_path.parent)
    os.close(fd)
    pending = Path(name)
    rows = 0
    try:
        with pq.ParquetWriter(pending, ENVELOPE_SCHEMA, compression=compression) as writer:
            batch = []
            for event in _events(events_path, report, config):
                batch.append(event)
                if len(batch) == batch_rows:
                    writer.write_table(pa.Table.from_pylist(batch, schema=ENVELOPE_SCHEMA))
                    rows += len(batch)
                    batch.clear()
            if batch:
                writer.write_table(pa.Table.from_pylist(batch, schema=ENVELOPE_SCHEMA))
                rows += len(batch)
        with pending.open("rb+") as handle:
            os.fsync(handle.fileno())
        receipt = dict(_inspect(pending, report["accepted"], batch_rows),
                       provenance=provenance, source_admission="unadmitted",
                       training_eligible=False)
        if receipt["content_sha256"] != provenance["content_sha256"]:
            raise ValueError("partition content mismatch")
        _unchanged(events_path, provenance)
        checked_path(output_path, allow_missing=True)
        os.link(pending, output_path)
        _publish_new(receipt_path, _encoded(receipt))
        return _verify(output_path, provenance, report["accepted"], batch_rows)
    finally:
        pending.unlink()
