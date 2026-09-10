"""Synthetic fixtures only: conservation and publication, not training evidence."""
import gzip
import hashlib
import importlib.util
import json
from pathlib import Path
import sys

import pytest
import zstandard

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "src"))


def driver():
    path = ROOT / "scripts/aggregate_corrected_capture.py"
    assert path.exists(), "streaming aggregation driver not implemented"
    spec = importlib.util.spec_from_file_location("aggregate_capture_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fixture(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    raw_files, originals = [], []
    for part in range(3):
        valid = (json.dumps(dict(record_type="slot", slot=42, recv_unix_ms=123456,
                                record_index=part, payload=dict(slot=42, parent=None,
                                                               status="Confirmed"))) + "\n").encode()
        raw = valid + (b"bad json\r\n" if part == 1 else b"")
        originals.append(raw)
        path = source / f"part{part}.ndjson.zst"
        path.write_bytes(zstandard.ZstdCompressor().compress(raw))
        raw_files.append(dict(filename=path.name, bytes=path.stat().st_size,
                              sha256=hashlib.sha256(path.read_bytes()).hexdigest()))
    events = source / "events.ndjson.zst"
    events.write_bytes(zstandard.ZstdCompressor().compress(b""))
    manifest = dict(session_id="synthetic", commitment="CONFIRMED", total_raw_records=4,
                    raw_files=raw_files, events_file=dict(filename=events.name,
                    bytes=events.stat().st_size, sha256=hashlib.sha256(events.read_bytes()).hexdigest()))
    path = source / "manifest.json"
    path.write_text(json.dumps(manifest), encoding="utf-8")
    args = dict(manifest_path=path, manifest_sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
                expected_raw_files=raw_files, expected_records=4, output_dir=tmp_path / "out",
                chunk_rows=2)
    return args, originals


def test_all_three_parts_conserved_and_exact_schema_nulls(tmp_path):
    m = driver()
    args, originals = fixture(tmp_path)
    report = m.aggregate(**args)
    assert report["complete"] is True
    assert report["rows_read"] == 4
    assert report["accepted"] == 3 and report["rejected"] == 1
    assert report["producer_count_matches"] is True
    assert report["training_eligible"] is False
    assert report["source_admission"] == "unadmitted"
    assert len(report["raw_parts"]) == 3
    assert m.verify(args["output_dir"])["rows_read"] == 4
    accepted, rejected = [], []
    for item in report["outputs"]:
        if item["kind"] not in ("accepted", "rejected"):
            continue
        with gzip.open(args["output_dir"] / item["path"], "rt", encoding="utf-8") as f:
            rows = [json.loads(line) for line in f]
        (accepted if item["kind"] == "accepted" else rejected).extend(rows)
    from north_star.partitions import ENVELOPE_SCHEMA
    from north_star.raw_adapter import project_record
    for row in accepted:
        ptr = row["raw_pointer"]
        part = ptr["source_part_index"]
        event = project_record(originals[part].splitlines(keepends=True)[0],
                               source_path=str(args["manifest_path"].parent / args["expected_raw_files"][part]["filename"]),
                               source_sha256=args["expected_raw_files"][part]["sha256"],
                               source_commitment="CONFIRMED")
        event["source_line_index"] = 0
        assert set(row["envelope"]) == set(ENVELOPE_SCHEMA.names)
        assert row["envelope"] == event
        assert ptr["decompressed_byte_offset"] == 0
        assert ptr["raw_record_sha256"] == event["raw_record_sha256"]
    assert rejected[0]["reason"] == "invalid_json"
    assert rejected[0]["raw_pointer"]["raw_record_sha256"] == hashlib.sha256(b"bad json\r\n").hexdigest()
    assert rejected[0]["raw_pointer"]["raw_byte_length"] == len(b"bad json\r\n")
    assert (args["output_dir"] / "COMPLETE.json").is_file()


@pytest.mark.parametrize("error", [KeyboardInterrupt, OSError])
def test_interruption_has_no_false_complete_and_can_retry(tmp_path, monkeypatch, error):
    m = driver()
    args, _ = fixture(tmp_path)
    original = m.Chunks.add
    calls = 0

    def fail(self, row):
        nonlocal calls
        calls += 1
        if calls == 3:
            raise error("injected interruption")
        return original(self, row)

    monkeypatch.setattr(m.Chunks, "add", fail)
    with pytest.raises(error):
        m.aggregate(**args)
    assert not args["output_dir"].exists()
    assert not list(tmp_path.rglob("COMPLETE.json"))
    failures = list(tmp_path.rglob("FAILED.json"))
    assert len(failures) == 1
    assert json.loads(failures[0].read_text())["complete"] is False
    monkeypatch.setattr(m.Chunks, "add", original)
    assert m.aggregate(**args)["rows_read"] == 4


def test_whole_manifest_hash_verification_precedes_project(tmp_path, monkeypatch):
    m = driver()
    args, _ = fixture(tmp_path)
    third = args["manifest_path"].parent / args["expected_raw_files"][2]["filename"]
    third.write_bytes(b"corrupt last part")
    monkeypatch.setattr(m.raw_adapter, "project_record", lambda *a, **k: pytest.fail("semantic read before all pins"))
    with pytest.raises(ValueError, match="source size/hash"):
        m.aggregate(**args)
    assert not args["output_dir"].exists()
    assert not list(tmp_path.glob("out.pending-*"))


def test_producer_mismatch_not_adjusted_or_published(tmp_path):
    m = driver()
    args, _ = fixture(tmp_path)
    manifest = json.loads(args["manifest_path"].read_text())
    manifest["total_raw_records"] = 5
    args["manifest_path"].write_text(json.dumps(manifest))
    args.update(expected_records=5, manifest_sha256=hashlib.sha256(args["manifest_path"].read_bytes()).hexdigest())
    with pytest.raises(ValueError, match="producer_count_mismatch"):
        m.aggregate(**args)
    failure = json.loads(next(tmp_path.rglob("FAILED.json")).read_text())
    assert failure["rows_read"] == 4 and failure["producer_records"] == 5
    assert failure["producer_count_matches"] is False
    assert not args["output_dir"].exists()
    assert not list(tmp_path.rglob("COMPLETE.json"))


def test_oversized_physical_line_drained_as_one_and_final_no_newline(tmp_path):
    m = driver()
    path = tmp_path / "oversized.zst"
    huge = b"x" * (m.MAX_RECORD_BYTES + 125) + b"\n"
    final = b"bad final"
    path.write_bytes(zstandard.ZstdCompressor().compress(huge + final))
    lines = list(m.raw_lines(path))
    assert len(lines) == 2
    assert lines[0][2:5] == (len(huge), hashlib.sha256(huge).hexdigest(), None)
    assert lines[1][0:3] == (1, len(huge), len(final))
    assert lines[1][4] == final
    assert m.project(lines[0], {}, "CONFIRMED")[1]["reason"] == "record_size_limit_exceeded"


def test_missing_output_resume_never_heals(tmp_path, monkeypatch):
    m = driver()
    args, _ = fixture(tmp_path)
    report = m.aggregate(**args)
    missing = args["output_dir"] / report["outputs"][0]["path"]
    missing.unlink()
    monkeypatch.setattr(m, "write_json", lambda *a: pytest.fail("verification wrote a file"))
    with pytest.raises((ValueError, FileNotFoundError)):
        m.verify(args["output_dir"])
    assert not missing.exists()


def test_destination_race_preserves_existing_target(tmp_path, monkeypatch):
    m = driver()
    args, _ = fixture(tmp_path)
    original = m.os.rename

    def race(source, target):
        target.mkdir()
        (target / "sentinel").write_bytes(b"existing unrelated data")
        return original(source, target)

    monkeypatch.setattr(m.os, "rename", race)
    with pytest.raises(OSError):
        m.aggregate(**args)
    assert (args["output_dir"] / "sentinel").read_bytes() == b"existing unrelated data"
    assert not list(tmp_path.rglob("COMPLETE.json"))


def test_duplicate_keys_nonfinite_and_empty_lines_are_rejections(tmp_path):
    m = driver()
    path = tmp_path / "malformed.zst"
    values = [b'{"x":1,"x":2}\n', b'{"x":1e9999}\n', b'\n', b'\xff\n']
    path.write_bytes(zstandard.ZstdCompressor().compress(b"".join(values)))
    results = []
    for line in m.raw_lines(path):
        ptr = m.pointer(0, dict(sha256="a" * 64), path, line)
        results.append(m.project(line, ptr, "CONFIRMED"))
    assert [x[0] for x in results] == ["rejected"] * 4
    assert [x[1]["reason"] for x in results] == ["duplicate_json_key", "nonfinite_json_number", "invalid_json", "invalid_json"]


def test_output_rotation_at_configured_row_bound(tmp_path):
    m = driver()
    root = tmp_path / "chunks"
    root.mkdir()
    inventory = []
    writer = m.Chunks(root, 0, "ledger", 2, inventory)
    for i in range(5):
        writer.add(dict(index=i))
    writer.close()
    assert [x["rows"] for x in inventory] == [2, 2, 1]
    assert list(m.rows(root, dict(outputs=inventory), 0, "ledger")) == [dict(index=i) for i in range(5)]
