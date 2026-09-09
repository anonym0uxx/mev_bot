"""Synthetic engineering fixtures, never admission or semantic-event evidence."""
import hashlib
import importlib
import json
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

from north_star.raw_adapter import project_record


def module():
    path = Path(__file__).resolve().parents[2] / "src/north_star/partitions.py"
    assert path.exists(), "bounded partition writer not implemented"
    return importlib.import_module("north_star.partitions")


def fixture(tmp_path, count=3):
    source = tmp_path / "input" / "events.jsonl"
    source.parent.mkdir()
    events = []
    for index in range(count):
        raw = (json.dumps(dict(record_type="slot", slot=2**64 - 1,
               recv_unix_ms=2**53 + 1, record_index=index,
               payload=dict(slot=2**64 - 1, parent=None, status="Finalized"))) + "\n").encode()
        event = project_record(raw, source_path="Literal/Source.ndjson.zst",
                               source_sha256="Ab" * 32)
        event["source_line_index"] = index
        events.append(event)
    encoded = b"".join((json.dumps(e) + "\n").encode() for e in events)
    source.write_bytes(encoded)
    report = dict(schema_version="raw_projection_report_v1", source_path=events[0]["source_path"],
                  source_sha256="Ab" * 32, source_sha256_after="Ab" * 32,
                  source_unchanged=True, source_size_bytes=123, adapter_sha256="b" * 64,
                  source_commitment=None, training_eligible=False, source_admission="unadmitted",
                  projection_scope="envelope_only_development", rows_read=count, accepted=count,
                  rejected=0, record_types={"slot": count}, limit=100, limit_reached=False,
                  eof_observed=True, failures=[], full_source_decoded=False,
                  events_path=str(source), events_sha256=hashlib.sha256(encoded).hexdigest())
    (source.parent / "report.json").write_text(json.dumps(report), encoding="utf-8")
    return source, events


@pytest.mark.parametrize("config", [
    {"batch_rows": 0}, {"batch_rows": True}, {"batch_rows": 1.5},
    {"batch_rows": 101}, {"max_rows": 0}, {"max_rows": 101},
    {"max_line_bytes": 0}, {"max_line_bytes": 8 * 1024 * 1024 + 1},
    {"compression": "bogus"},
])
def test_bad_config_fails_before_output(tmp_path, config):
    source, _ = fixture(tmp_path)
    out = tmp_path / "output" / "part.parquet"
    with pytest.raises(ValueError, match="config"):
        module().write_partition(source, out, **config)
    assert not out.parent.exists()


def test_row_and_line_budgets_fail_closed(tmp_path):
    source, _ = fixture(tmp_path)
    out = tmp_path / "output" / "part.parquet"
    for config in ({"max_rows": 2, "batch_rows": 2}, {"max_line_bytes": 32}):
        with pytest.raises(ValueError, match="limit"):
            module().write_partition(source, out, **config)
        assert not out.exists()
        assert not out.with_name(out.name + ".receipt.json").exists()


def test_resume_validates_receipt_and_output_without_rewriting(tmp_path):
    source, _ = fixture(tmp_path)
    out = tmp_path / "output" / "part.parquet"
    writer = module()
    first = writer.write_partition(source, out, batch_rows=2)
    before = out.stat().st_mtime_ns
    assert writer.write_partition(source, out, batch_rows=2) == first
    assert out.stat().st_mtime_ns == before
    with pytest.raises(ValueError, match="provenance"):
        writer.write_partition(source, out, batch_rows=3)
    assert out.stat().st_mtime_ns == before


@pytest.mark.parametrize("damage", ["bytes", "rows", "schema", "missing_output", "missing_receipt"])
def test_corruption_and_orphans_refuse_resume(tmp_path, damage):
    source, _ = fixture(tmp_path)
    out = tmp_path / "output" / "part.parquet"
    writer = module()
    writer.write_partition(source, out)
    receipt_path = out.with_name(out.name + ".receipt.json")
    receipt = json.loads(receipt_path.read_text())
    if damage == "bytes":
        data = out.read_bytes()
        out.write_bytes(data[:100] + bytes([data[100] ^ 1]) + data[101:])
    elif damage == "rows":
        pq.write_table(pq.ParquetFile(out).read().slice(0, 1), out)
    elif damage == "schema":
        pq.write_table(pa.table({"wrong": [1, 2, 3]}), out)
    elif damage == "missing_output":
        out.unlink()
    else:
        receipt_path.unlink()
    # Even a recomputed byte digest cannot hide wrong schema or row count.
    if damage in ("rows", "schema"):
        receipt.update(sha256=hashlib.sha256(out.read_bytes()).hexdigest(), bytes=out.stat().st_size)
        receipt_path.write_text(json.dumps(receipt))
    before = out.read_bytes() if out.exists() else None
    with pytest.raises((ValueError, FileNotFoundError, FileExistsError)):
        writer.write_partition(source, out)
    assert (out.read_bytes() if out.exists() else None) == before


def test_interruption_after_data_publication_leaves_uncommitted_orphan(tmp_path, monkeypatch):
    source, _ = fixture(tmp_path)
    out = tmp_path / "output" / "part.parquet"
    writer = module()
    def interrupt(*args, **kwargs):
        raise OSError("simulated receipt publication interruption")
    with monkeypatch.context() as patcher:
        patcher.setattr(writer, "_publish_new", interrupt)
        with pytest.raises(OSError, match="interruption"):
            writer.write_partition(source, out)
    assert out.exists()
    assert not out.with_name(out.name + ".receipt.json").exists()
    assert not list(out.parent.glob("*.pending"))
    with pytest.raises((ValueError, FileNotFoundError, FileExistsError)):
        writer.write_partition(source, out)


@pytest.mark.parametrize("damage", ["missing_report", "unfinished", "active_marker", "digest",
    "admitted", "eligible", "counts", "wrong_source", "oversized_report", "no_newline"])
def test_unfinished_active_or_inconsistent_input_fails_closed(tmp_path, damage):
    source, _ = fixture(tmp_path)
    report_path = source.with_name("report.json")
    report = json.loads(report_path.read_text())
    if damage == "missing_report":
        report_path.unlink()
    elif damage == "active_marker":
        source.with_name("events.jsonl.active").write_text("active")
    elif damage == "oversized_report":
        report_path.write_bytes(b" " * (1024 * 1024 + 1))
    else:
        if damage == "unfinished":
            report["source_unchanged"] = False
        elif damage == "digest":
            report["events_sha256"] = "0" * 64
        elif damage == "admitted":
            report["source_admission"] = "admitted"
        elif damage == "eligible":
            report["training_eligible"] = True
        elif damage == "counts":
            report["rows_read"] += 1
        elif damage == "wrong_source":
            report["source_sha256_after"] = "f" * 64
        else:
            source.write_bytes(source.read_bytes().rstrip(b"\n"))
            report["events_sha256"] = hashlib.sha256(source.read_bytes()).hexdigest()
        report_path.write_text(json.dumps(report))
    out = tmp_path / "output" / "part.parquet"
    with pytest.raises((ValueError, FileNotFoundError)):
        module().write_partition(source, out)
    assert not out.exists()
    assert not out.with_name(out.name + ".receipt.json").exists()


def test_source_mutation_during_build_has_no_commit(tmp_path, monkeypatch):
    source, _ = fixture(tmp_path)
    out = tmp_path / "output" / "part.parquet"
    writer = module()
    inspect = writer._inspect
    def mutate(*args):
        result = inspect(*args)
        with source.open("ab") as handle:
            handle.write(b"\n")
        return result
    monkeypatch.setattr(writer, "_inspect", mutate)
    with pytest.raises(ValueError, match="changed"):
        writer.write_partition(source, out)
    assert not out.exists()


def rewrite_events(source, events):
    source.write_bytes(b"".join((json.dumps(e) + "\n").encode() for e in events))
    report_path = source.with_name("report.json")
    report = json.loads(report_path.read_text())
    report["events_sha256"] = hashlib.sha256(source.read_bytes()).hexdigest()
    report_path.write_text(json.dumps(report))


@pytest.mark.parametrize("field,value", [
    ("slot", 1.5), ("slot", True), ("slot", -1), ("slot", 2**64),
    ("observed_at_unix_ms", "123"), ("observed_at_unix_ms", None),
    ("training_eligible", True), ("source_admission", "admitted"),
    ("source_sha256", "ab" * 32), ("source_sha256", "bad"),
    ("raw_record_sha256", "bad"), ("event_id", "wrong"),
    ("source_path", "different"), ("source_commitment", "CONFIRMED"),
    ("null_reasons", {}), ("unknown_column", 1), ("record_type", "trade"),
    ("account_keys", [None]), ("block_time_unix_s", 1.5),
    ("projection_scope", "economic_events"), ("source_line_index", 100),
])
def test_bad_envelope_not_coerced_or_silently_dropped(tmp_path, field, value):
    source, events = fixture(tmp_path)
    events[0][field] = value
    rewrite_events(source, events)
    out = tmp_path / "output" / "part.parquet"
    with pytest.raises(ValueError):
        module().write_partition(source, out)
    assert not out.exists()


def test_missing_column_and_duplicate_json_keys_refused(tmp_path):
    source, events = fixture(tmp_path)
    del events[0]["signature"]
    rewrite_events(source, events)
    out = tmp_path / "output" / "part.parquet"
    with pytest.raises(ValueError):
        module().write_partition(source, out)
    source.write_bytes(source.read_bytes().replace(b'"slot": ', b'"slot": 1, "slot": ', 1))
    report_path = source.with_name("report.json")
    report = json.loads(report_path.read_text())
    report["events_sha256"] = hashlib.sha256(source.read_bytes()).hexdigest()
    report_path.write_text(json.dumps(report))
    with pytest.raises(ValueError, match="duplicate"):
        module().write_partition(source, out)


def test_same_schema_same_count_content_corruption_refused(tmp_path):
    source, _ = fixture(tmp_path)
    out = tmp_path / "output" / "part.parquet"
    writer = module()
    writer.write_partition(source, out)
    rows = read_rows(out)
    rows[0]["slot"] = 1
    pq.write_table(pa.Table.from_pylist(rows, schema=writer.ENVELOPE_SCHEMA), out)
    receipt_path = out.with_name(out.name + ".receipt.json")
    receipt = json.loads(receipt_path.read_text())
    receipt.update(sha256=hashlib.sha256(out.read_bytes()).hexdigest(), bytes=out.stat().st_size)
    receipt_path.write_text(json.dumps(receipt))
    with pytest.raises(ValueError, match="content"):
        writer.write_partition(source, out)


def test_incomplete_report_stop_flags_refused(tmp_path):
    source, _ = fixture(tmp_path)
    report_path = source.with_name("report.json")
    report = json.loads(report_path.read_text())
    report.update(eof_observed=False, limit_reached=False)
    report_path.write_text(json.dumps(report))
    with pytest.raises(ValueError):
        module().write_partition(source, tmp_path / "part.parquet")


def test_atomic_no_clobber_race(tmp_path, monkeypatch):
    source, _ = fixture(tmp_path)
    out = tmp_path / "output" / "part.parquet"
    writer = module()
    link = writer.os.link
    def race(src, dst):
        if Path(dst) == out:
            out.write_bytes(b"other publisher")
        return link(src, dst)
    monkeypatch.setattr(writer.os, "link", race)
    with pytest.raises(FileExistsError):
        writer.write_partition(source, out)
    assert out.read_bytes() == b"other publisher"
    assert not out.with_name(out.name + ".receipt.json").exists()


def test_literal_key_lists_empty_lists_and_signed_time_roundtrip(tmp_path):
    source, events = fixture(tmp_path)
    payloads = [
        ("transaction", dict(signature_b58=" LiteralSignature ", signatures_b58=[" LiteralSignature "],
             tx_index=2**64 - 1, raw_hash=" LiteralPayloadHash ",
             message={"account_keys_b58": ["Duplicate", "Duplicate"]},
             meta={"loaded_writable_addresses_b58": [], "loaded_readonly_addresses_b58": [" Key "]})),
        ("block_meta", dict(slot=42, block_time=-(2**63), blockhash=" Block ",
             parent_blockhash="Parent", parent_slot=41, block_height=None,
             executed_transaction_count=0, entries_count=0)),
    ]
    for index, (kind, payload) in enumerate(payloads):
        raw = (json.dumps(dict(record_type=kind, slot=42, recv_unix_ms=2**64 - 1,
                    record_index=index, payload=payload)) + "\n").encode()
        events[index] = project_record(raw, source_path=events[index]["source_path"],
                                       source_sha256="Ab" * 32)
        events[index]["source_line_index"] = index
    rewrite_events(source, events)
    report_path = source.with_name("report.json")
    report = json.loads(report_path.read_text())
    report["record_types"] = {"transaction": 1, "block_meta": 1, "slot": 1}
    report_path.write_text(json.dumps(report))
    out = tmp_path / "part.parquet"
    module().write_partition(source, out, batch_rows=1)
    assert read_rows(out) == events


def test_stale_pending_not_accepted_or_removed(tmp_path):
    source, events = fixture(tmp_path)
    out = tmp_path / "part.parquet"
    pending = tmp_path / "part.parquet.old.pending"
    pending.write_bytes(b"interrupted unpublished data")
    module().write_partition(source, out)
    assert read_rows(out) == events
    assert pending.read_bytes() == b"interrupted unpublished data"


def test_budget_preflight_before_hashing_large_input(tmp_path, monkeypatch):
    source, _ = fixture(tmp_path)
    with source.open("wb") as handle:
        handle.truncate(101 * 1024 * 1024)
    writer = module()
    def no_hash(path):
        pytest.fail("oversized input must not be scanned")
    monkeypatch.setattr(writer, "_sha", no_hash)
    with pytest.raises(ValueError, match="limit"):
        writer.write_partition(source, tmp_path / "part.parquet")


@pytest.mark.parametrize("resume", [False, True])
def test_oversized_output_refused_before_hash_or_parquet_open(tmp_path, monkeypatch, resume):
    source, _ = fixture(tmp_path)
    out = tmp_path / "part.parquet"
    writer = module()
    writer.write_partition(source, out)
    with out.open("wb") as handle:
        handle.truncate(128 * 1024 * 1024 + 1)
    file_digest = writer.hashlib.file_digest

    def guarded_hash(handle, *args, **kwargs):
        assert Path(handle.name) != out, "oversized output must not be hashed"
        return file_digest(handle, *args, **kwargs)

    def no_parquet(*args, **kwargs):
        pytest.fail("oversized output must not be opened or decoded as Parquet")

    monkeypatch.setattr(writer.hashlib, "file_digest", guarded_hash)
    monkeypatch.setattr(writer.pq, "ParquetFile", no_parquet)
    with pytest.raises(ValueError, match="output byte limit"):
        if resume:
            writer.write_partition(source, out)
        else:
            writer._inspect(out, 3, 32)


@pytest.mark.parametrize("damage", ["more_rows", "fewer_rows", "large_group", "empty_group"])
def test_bad_footer_refused_before_decode(tmp_path, monkeypatch, damage):
    source, events = fixture(tmp_path)
    out = tmp_path / "part.parquet"
    writer = module()
    writer.write_partition(source, out, batch_rows=2)
    table = pa.Table.from_pylist(events, schema=writer.ENVELOPE_SCHEMA)
    if damage == "more_rows":
        pq.write_table(pa.concat_tables([table, table]), out, row_group_size=2)
    elif damage == "fewer_rows":
        pq.write_table(table.slice(0, 1), out, row_group_size=2)
    elif damage == "large_group":
        pq.write_table(table, out, row_group_size=3)
    else:
        with pq.ParquetWriter(out, writer.ENVELOPE_SCHEMA) as handle:
            handle.write_table(table.slice(0, 0))
            handle.write_table(table, row_group_size=2)

    def no_decode(*args, **kwargs):
        pytest.fail("bad footer must be rejected before decoding batches")

    monkeypatch.setattr(writer.pq.ParquetFile, "iter_batches", no_decode)
    with pytest.raises(ValueError, match="partition (row count|batch bound) mismatch"):
        writer.write_partition(source, out, batch_rows=2)


@pytest.mark.parametrize("overflow", ["total", "batch"])
def test_readback_running_row_cap_before_validation_or_materialization(tmp_path, monkeypatch, overflow):
    source, events = fixture(tmp_path)
    out = tmp_path / "part.parquet"
    writer = module()
    writer.write_partition(source, out, batch_rows=2)
    first = pa.RecordBatch.from_pylist(events[:2], schema=writer.ENVELOPE_SCHEMA)

    class ExcessBatch:
        num_rows = 2 if overflow == "total" else 3

        def validate(self, **kwargs):
            pytest.fail("over-budget batch must not be validated")

        def to_pylist(self):
            pytest.fail("over-budget batch must not be materialized")

    def batches(*args, **kwargs):
        if overflow == "total":
            yield first
        yield ExcessBatch()
        pytest.fail("readback must stop at the first over-budget batch")

    monkeypatch.setattr(writer.pq.ParquetFile, "iter_batches", batches)
    with pytest.raises(ValueError, match="partition readback row limit"):
        writer._inspect(out, 3, 2)


@pytest.mark.parametrize("index", [False, True, 0.0, 1.0])
def test_report_rejection_index_requires_exact_integer(tmp_path, index):
    source, events = fixture(tmp_path)
    events.pop(int(index))
    source.write_text("".join(json.dumps(event) + "\n" for event in events), encoding="utf-8")
    report_path = source.with_name("report.json")
    report = json.loads(report_path.read_text())
    report.update(accepted=2, rejected=1, record_types={"slot": 2},
                  failures=[{"line_index": index, "reason": "synthetic rejection"}],
                  events_sha256=hashlib.sha256(source.read_bytes()).hexdigest())
    report_path.write_text(json.dumps(report), encoding="utf-8")
    out = tmp_path / "output" / "part.parquet"
    with pytest.raises(ValueError, match="report rejection index"):
        module().write_partition(source, out)
    assert not out.parent.exists()


@pytest.mark.parametrize("count", [True, 1.0])
def test_report_record_type_count_requires_exact_integer(tmp_path, count):
    source, _ = fixture(tmp_path, count=1)
    report_path = source.with_name("report.json")
    report = json.loads(report_path.read_text())
    report["record_types"]["slot"] = count
    report_path.write_text(json.dumps(report), encoding="utf-8")
    out = tmp_path / "output" / "part.parquet"
    with pytest.raises(ValueError, match="report record type count"):
        module().write_partition(source, out)
    assert not out.parent.exists()


def read_rows(path):
    rows = pq.ParquetFile(path).read().to_pylist()
    for row in rows:
        row["null_reasons"] = dict(row["null_reasons"])
    return rows


def test_typed_envelope_roundtrip_exact_integers_and_nulls(tmp_path):
    source, events = fixture(tmp_path)
    out = tmp_path / "output" / "part.parquet"
    writer = module()
    receipt = writer.write_partition(source, out, batch_rows=2)
    assert read_rows(out) == events
    parquet = pq.ParquetFile(out)
    # Parquet renames the map's internal entries node to its column name.
    assert parquet.schema_arrow.equals(writer.ENVELOPE_SCHEMA)
    assert parquet.schema_arrow.metadata == writer.ENVELOPE_SCHEMA.metadata
    assert parquet.schema_arrow.field("slot").type == pa.uint64()
    assert parquet.schema_arrow.field("observed_at_unix_ms").type == pa.uint64()
    assert [parquet.metadata.row_group(i).num_rows for i in range(parquet.num_row_groups)] == [2, 1]
    assert receipt["rows"] == 3
    assert receipt["sha256"] == hashlib.sha256(out.read_bytes()).hexdigest()
    assert receipt["provenance"]["events_sha256"] == hashlib.sha256(source.read_bytes()).hexdigest()
    assert receipt["source_admission"] == "unadmitted"
    assert receipt["training_eligible"] is False
    assert json.loads(out.with_name(out.name + ".receipt.json").read_text()) == receipt
