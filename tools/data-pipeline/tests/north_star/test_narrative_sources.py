"""Synthetic parser fixtures only; never exported as teaching records."""
import hashlib
import importlib.util
import json
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


def api():
    path = ROOT / "src/north_star/narrative_sources.py"
    assert path.exists(), "narrative source recovery adapter not implemented"
    spec = importlib.util.spec_from_file_location("narrative_sources_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sha(data):
    return hashlib.sha256(data).hexdigest()


def fixture(tmp_path, claims=None, contents=None):
    claim = {"claim_id": "q1", "content_id": "c1", "claim_text": "é\r\n\\n",
             "schema_version": "1.1.0", "publish_time_ms": 200,
             "first_seen_ms": 100, "temporal_class": "EX_ANTE",
             "admission_status": "GOLD"}
    content = {"content_id": "c1", "raw_text": "é\r\n\\n untouched  ",
               "schema_version": "1.1.0", "pipeline_version": "1.1.0",
               "run_uuid": "legacy-run", "source_hash": "short-legacy-hash",
               "publish_time_ms": 300, "first_seen_ms": 250}
    paths = [tmp_path / "claims.jsonl", tmp_path / "contents.jsonl"]
    for path, rows in zip(paths, [claims if claims is not None else [claim],
                                  contents if contents is not None else [content]]):
        path.write_bytes(b"".join(json.dumps(row, ensure_ascii=True).encode() + b"\r\n"
                                  for row in rows))
    return *paths, tmp_path / "recovered"


def read_rows(out):
    return [json.loads(line) for line in (out / "recovered_sources.jsonl").read_bytes().splitlines()]


def test_exact_prefix_roundtrip_keeps_bytes_versions_and_unverified_times(tmp_path):
    claims, contents, out = fixture(tmp_path)
    manifest = api().recover_sources(claims, contents, out, claim_limit=1)
    row, = read_rows(out)
    text = "é\r\n\\n untouched  ".encode("utf-8")
    assert row["raw_text"] == text.decode("utf-8")
    evidence = row["evidence"]
    assert (out / evidence["raw_id"]).read_bytes() == text
    assert evidence["sha256"] == sha(text)
    assert evidence["span_start"] == 0
    assert evidence["span_end"] == len("é\r\n\\n".encode("utf-8"))
    assert evidence["span_unit"] == "BYTE"
    assert evidence["content_version"]
    assert row["legacy_content_metadata"]["schema_version"] == "1.1.0"
    assert row["legacy_content_metadata"]["source_hash"] == "short-legacy-hash"
    assert row["legacy_timestamps_unverified"] == {
        "claim": {"publish_time_ms": 200, "first_seen_ms": 100},
        "content": {"publish_time_ms": 300, "first_seen_ms": 250}}
    assert row["availability_verified"] is False
    assert row["available_at_ms"] is None
    assert row["temporal_state"] == row["rights_state"] == row["source_state"] == "UNKNOWN"
    assert row["admitted"] is False
    assert row["split"] == "development"
    assert row["legacy_claim_metadata"]["temporal_class"] == "EX_ANTE"
    assert "ORIGINAL_PROVIDER_BLOB_MISSING" in row["reason_codes"]
    assert "TEXT_MISSING" not in row["reason_codes"]
    assert manifest["counts"]["recovered_text"] == 1
    assert manifest["counts"]["exact_prefix_spans"] == 1
    assert manifest["inputs"]["contents"]["sha256"] == sha(contents.read_bytes())
    assert manifest["inputs"]["claims"]["sha256"] == sha(claims.read_bytes())
    assert manifest == json.loads((out / "manifest.json").read_bytes())
    ptr = row["content_record_pointers"][0]
    with contents.open("rb") as handle:
        handle.seek(ptr["byte_offset"])
        assert sha(handle.read(ptr["byte_length"])) == ptr["sha256"]


@pytest.mark.parametrize("content,claim_text,reason,recovered", [
    ({"content_id": "c1 ", "raw_text": "prefix"}, "prefix", "CONTENT_ID_NOT_FOUND", False),
    ({"content_id": "c1", "raw_text": " prefix"}, "prefix", "CLAIM_PREFIX_NOT_EXACT", True),
    ({"content_id": "c1", "raw_text": "prefix"}, "PREFIX", "CLAIM_PREFIX_NOT_EXACT", True),
    ({"content_id": "c1", "raw_text": "é"}, "e\u0301", "CLAIM_PREFIX_NOT_EXACT", True),
    ({"content_id": "c1", "raw_text": "prefix"}, "", "CLAIM_PREFIX_NOT_EXACT", True),
    ({"content_id": "c1", "normalized_text": "prefix"}, "prefix", "TEXT_MISSING", False),
    ({"content_id": "c1", "raw_text": ""}, "prefix", "TEXT_MISSING", False),
    ({"content_id": "c1", "raw_text": 123}, "prefix", "TEXT_MISSING", False),
])
def test_only_exact_nonempty_prefix_gets_span(tmp_path, content, claim_text, reason, recovered):
    paths = fixture(tmp_path, claims=[{"claim_id": "q", "content_id": "c1", "claim_text": claim_text}],
                    contents=[content])
    manifest = api().recover_sources(*paths)
    row, = read_rows(paths[2])
    assert reason in row["reason_codes"]
    assert row["evidence"] is None
    assert manifest["counts"]["exact_prefix_spans"] == 0
    assert manifest["counts"]["recovered_text"] == int(recovered)
    assert (row["raw_text"] is not None) == recovered
    if recovered:
        assert (paths[2] / row["text_object"]["raw_id"]).read_bytes() == content["raw_text"].encode()


@pytest.mark.parametrize("change,conflict", [({}, False), ({"raw_text": "different"}, True),
                                            ({"publish_time_ms": 999}, True)])
def test_duplicate_content_is_flagged_and_conflicts_never_arbitrarily_selected(tmp_path, change, conflict):
    content = {"content_id": "c1", "raw_text": "prefix", "schema_version": "1.1.0"}
    paths = fixture(tmp_path, claims=[{"claim_id": "q", "content_id": "c1", "claim_text": "prefix"}],
                    contents=[content, {"content_id": "unrelated", "raw_text": "noise"}, dict(content, **change)])
    manifest = api().recover_sources(*paths)
    row, = read_rows(paths[2])
    assert "DUPLICATE_CONTENT_ID" in row["reason_codes"]
    assert ("CONTENT_ID_CONFLICT" in row["reason_codes"]) == conflict
    assert len(row["content_record_pointers"]) == 2
    assert (row["evidence"] is None) == conflict
    assert (row["raw_text"] is None) == conflict
    assert "TEXT_MISSING" not in row["reason_codes"]  # present but ambiguous != unavailable
    assert manifest["counts"]["conflicting_content_claims"] == int(conflict)
    for ptr in row["content_record_pointers"]:
        assert sha((paths[2] / ptr["artifact_path"]).read_bytes()) == ptr["sha256"]


@pytest.mark.parametrize("changed", [False, True])
def test_duplicate_claim_ids_are_not_counted_as_independent_examples(tmp_path, changed):
    claim = {"claim_id": "q", "content_id": "c1", "claim_text": "prefix"}
    paths = fixture(tmp_path, claims=[claim, dict(claim, claim_text="pre" if changed else "prefix")],
                    contents=[{"content_id": "c1", "raw_text": "prefix"}])
    manifest = api().recover_sources(*paths)
    rows = read_rows(paths[2])
    assert len(rows) == 2  # preserve original rows, flag rather than silently drop
    assert all("DUPLICATE_CLAIM_ID" in r["reason_codes"] for r in rows)
    assert all(("CLAIM_ID_CONFLICT" in r["reason_codes"]) == changed for r in rows)
    assert manifest["counts"]["unique_claim_ids"] == 1


def test_provider_pointer_is_unverified_not_a_verified_blob(tmp_path):
    content = {"content_id": "c1", "raw_text": "prefix", "original_provider_blob": {"path": "missing.json"}}
    paths = fixture(tmp_path, contents=[content])
    api().recover_sources(*paths)
    row, = read_rows(paths[2])
    assert row["original_provider_blob"]["state"] == "UNVERIFIED_REFERENCE"
    assert "ORIGINAL_PROVIDER_BLOB_UNVERIFIED" in row["reason_codes"]
    assert row["availability_verified"] is False


@pytest.mark.parametrize("option,value", [("claim_limit", 0), ("claim_limit", True),
    ("max_content_rows", -1), ("max_line_bytes", 0), ("max_input_bytes", False),
    ("max_selected_bytes", -1)])
def test_invalid_bounds_fail_before_writes(tmp_path, option, value):
    paths = fixture(tmp_path)
    with pytest.raises(ValueError, match="positive integer"):
        api().recover_sources(*paths, **{option: value})
    assert not paths[2].exists()


@pytest.mark.parametrize("bounds,reason", [({"max_content_rows": 1}, "row bound"),
    ({"max_line_bytes": 20}, "byte bound"), ({"max_input_bytes": 20}, "byte bound"),
    ({"max_selected_bytes": 20}, "selected byte bound")])
def test_bounds_never_publish_incomplete_duplicate_scan(tmp_path, bounds, reason):
    content = {"content_id": "c1", "raw_text": "prefix"}
    paths = fixture(tmp_path, contents=[content, dict(content, raw_text="conflict")])
    before = [p.read_bytes() for p in paths[:2]]
    with pytest.raises(ValueError, match=reason):
        api().recover_sources(*paths, **bounds)
    assert not paths[2].exists()
    assert before == [p.read_bytes() for p in paths[:2]]


def test_claim_prefix_is_bounded_and_full_content_hash_is_exact(tmp_path):
    paths = fixture(tmp_path)
    with paths[0].open("ab") as handle:
        handle.write(b"not read outside requested prefix\n")
    manifest = api().recover_sources(*paths, claim_limit=1, max_content_rows=1)
    assert manifest["inputs"]["claims"]["rows"] == 1
    assert manifest["inputs"]["claims"]["hash_scope"] == "selected_prefix"
    assert manifest["inputs"]["contents"]["rows"] == 1
    assert manifest["inputs"]["contents"]["scan_complete"] is True
    assert manifest["inputs"]["contents"]["sha256"] == sha(paths[1].read_bytes())
    with pytest.raises(FileExistsError):
        api().recover_sources(*paths, claim_limit=1)


def test_empty_inputs(tmp_path):
    paths = fixture(tmp_path, claims=[], contents=[])
    manifest = api().recover_sources(*paths)
    assert manifest["counts"]["claims"] == 0
    assert manifest["inputs"]["claims"]["rows"] == 0
    assert manifest["inputs"]["contents"]["rows"] == 0


@pytest.mark.parametrize("claim", [{"claim_id": "q", "content_id": None},
    {"claim_id": 12, "content_id": "c1"}, {"claim_id": "q", "content_id": ""}])
def test_ids_must_be_nonempty_strings_not_coerced(tmp_path, claim):
    paths = fixture(tmp_path, claims=[claim])
    with pytest.raises(ValueError, match="identifier"):
        api().recover_sources(*paths)
    assert not paths[2].exists()


@pytest.mark.parametrize("stream", [0, 1, 2])
@pytest.mark.parametrize("bad", [
    b'{"content_id":"other","content_id":"c1"}',
    b'{"raw_text":"other","raw_text":"prefix"}',
    b'{"nested":[{"x":1,"\\u0078":2}]}',
    b'{"nested":[NaN]}', b'{"nested":[Infinity]}',
    b'{"nested":[-Infinity]}', b'{"nested":[1e9999]}',
    b'{"nested":[-1e9999]}',
    b'{"raw_text":"\\ud800"}', b'{"nested":["\\udfff"]}',
    b'{"\\ud800":"key"}', b'{"raw_text":"\xff"}',
    b'{oops}', b'[]',
    b'{"nested":' + b'[' * 65 + b'0' + b']' * 65 + b'}',
    b'{"nested":' + b'[' * 2000 + b'0' + b']' * 2000 + b'}',
])
def test_strict_json_rejected_before_any_output(tmp_path, stream, bad):
    paths = fixture(tmp_path)
    if stream == 2:  # unselected contents must also be validated recursively
        with paths[1].open("ab") as handle:
            handle.write(bad + b"\n")
    else:
        paths[stream].write_bytes(bad + b"\n")
    before = [p.read_bytes() for p in paths[:2]]
    with pytest.raises(ValueError, match="invalid JSONL record") as error:
        api().recover_sources(*paths)
    assert type(error.value) is ValueError
    assert not paths[2].exists()
    assert before == [p.read_bytes() for p in paths[:2]]


@pytest.mark.parametrize("identifier", [" ", " c1 ", "../../outside", "C:/outside", "\\\\host\\share"])
def test_identifiers_are_literal_data_not_paths(tmp_path, identifier):
    paths = fixture(tmp_path, claims=[{"claim_id": identifier, "content_id": identifier,
                                     "claim_text": "😀"}],
                    contents=[{"content_id": identifier, "raw_text": "😀 tail",
                               "metadata": {"finite": 1e300, "brackets": "[[["}}])
    manifest = api().recover_sources(*paths)
    row, = read_rows(paths[2])
    assert row["claim_id"] == row["content_id"] == identifier
    assert row["evidence"]["span_end"] == len("😀".encode())
    assert all(Path(rel).parts[0] in {"records", "objects", "recovered_sources.jsonl"}
               for rel in manifest["files"])


@pytest.mark.parametrize("kind", ["claim", "content", "input_ancestor", "output", "output_ancestor"])
def test_symlink_paths_rejected_without_resolution(tmp_path, kind):
    paths = list(fixture(tmp_path))
    target = tmp_path / "redirected"
    if kind in {"claim", "content"}:
        index = 0 if kind == "claim" else 1
        link = tmp_path / "input-link"
        link.symlink_to(paths[index])
        paths[index] = link
    elif kind == "input_ancestor":
        link = tmp_path / "input-parent"
        link.symlink_to(tmp_path, target_is_directory=True)
        paths[0] = link / paths[0].name
    elif kind == "output":
        paths[2].symlink_to(target, target_is_directory=True)
    else:
        link = tmp_path / "output-parent"
        link.symlink_to(tmp_path, target_is_directory=True)
        paths[2] = link / "redirected"
    before = [p.read_bytes() for p in paths[:2]]
    with pytest.raises(ValueError, match="link/reparse"):
        api().recover_sources(*paths)
    assert not target.exists()
    assert not (tmp_path / "recovered" / "manifest.json").exists()
    assert before == [p.read_bytes() for p in paths[:2]]


@pytest.mark.parametrize("kind", ["input", "output_ancestor"])
def test_windows_reparse_attribute_rejected(tmp_path, monkeypatch, kind):
    from types import SimpleNamespace
    paths = fixture(tmp_path)
    flagged = paths[0] if kind == "input" else tmp_path
    original = Path.lstat

    def reparse(path, *args, **kwargs):
        info = original(path, *args, **kwargs)
        if path == flagged:
            return SimpleNamespace(st_mode=info.st_mode, st_file_attributes=0x400)
        return info

    monkeypatch.setattr(Path, "lstat", reparse)
    with pytest.raises(ValueError, match="link/reparse"):
        api().recover_sources(*paths)
    assert not paths[2].exists()


@pytest.mark.parametrize("changed_index,after_scan", [(0, 0), (0, 1), (1, 0), (1, 1)])
@pytest.mark.parametrize("replacement", [True, False])
def test_source_changes_across_both_scans_fail_before_output(tmp_path, monkeypatch,
                                                            changed_index, after_scan, replacement):
    import os
    paths = fixture(tmp_path)
    module = api()
    original = module._scan

    def changing_scan(path, *args, **kwargs):
        yield from original(path, *args, **kwargs)
        if path == paths[after_scan]:
            changed = paths[changed_index]
            data = changed.read_bytes().replace(b'"c1"', b'"c2"')
            if replacement:
                other = tmp_path / "replacement.jsonl"
                other.write_bytes(data)
                os.replace(other, changed)
            else:
                before = changed.stat()
                changed.write_bytes(data)
                # Windows can coalesce rapid same-size writes' timestamps.
                # Make the observed mutation deterministic (ABA is out of scope).
                os.utime(changed, ns=(before.st_atime_ns, before.st_mtime_ns + 1_000_000_000))

    monkeypatch.setattr(module, "_scan", changing_scan)
    with pytest.raises(ValueError, match="integrity changed"):
        module.recover_sources(*paths)
    assert not paths[2].exists()


@pytest.mark.parametrize("phase", ["manifest_error", "manifest_short", "object_error", "object_short",
                                    "source_change", "concurrent_dir", "concurrent_file"])
def test_publication_is_complete_or_absent_and_retryable(tmp_path, monkeypatch, phase):
    import os
    paths = fixture(tmp_path)
    out = paths[2]
    module = api()
    original_text, original_bytes = Path.write_text, Path.write_bytes
    observed = []

    def text_write(path, data, *args, **kwargs):
        if path.name == "manifest.json":
            observed.append(out.exists())
            if phase == "manifest_error":
                original_text(path, data[:25], *args, **kwargs)
                raise OSError("injected write failure")
            if phase == "manifest_short":
                return original_text(path, data[:25], *args, **kwargs)
            if phase == "source_change":
                other = tmp_path / "replacement.jsonl"
                original_bytes(other, paths[0].read_bytes().replace(b'"c1"', b'"c2"'))
                os.replace(other, paths[0])
            if phase == "concurrent_dir":
                out.mkdir(exist_ok=True)
                original_bytes(out / "sentinel", b"keep")
            if phase == "concurrent_file":
                if out.is_dir():  # old implementation already exposed output
                    return original_text(path, data, *args, **kwargs)
                original_bytes(out, b"keep")
        return original_text(path, data, *args, **kwargs)

    def bytes_write(path, data, *args, **kwargs):
        if path.suffix == ".utf8":
            observed.append(out.exists())
            if phase == "object_error":
                original_bytes(path, data[:1], *args, **kwargs)
                raise OSError("injected write failure")
            if phase == "object_short":
                return original_bytes(path, data[:1], *args, **kwargs)
        return original_bytes(path, data, *args, **kwargs)

    expected = FileExistsError if phase.startswith("concurrent") else (ValueError if phase == "source_change" else OSError)
    with monkeypatch.context() as patcher:
        patcher.setattr(Path, "write_text", text_write)
        patcher.setattr(Path, "write_bytes", bytes_write)
        with pytest.raises(expected):
            module.recover_sources(*paths)
    assert observed and not any(observed)
    assert not list(tmp_path.glob(".recovered-*.tmp"))
    if phase == "concurrent_dir":
        assert (out / "sentinel").read_bytes() == b"keep"
        assert list(out.iterdir()) == [out / "sentinel"]
    elif phase == "concurrent_file":
        assert out.read_bytes() == b"keep"
    else:
        assert not out.exists()
        manifest = module.recover_sources(*paths)
        assert manifest == json.loads((out / "manifest.json").read_bytes())


def test_artifact_pins_current_transform_and_is_fully_read_back(tmp_path):
    paths = fixture(tmp_path)
    module = api()
    manifest = module.recover_sources(*paths)
    assert manifest["transform"]["version"] == module.TRANSFORM_VERSION
    assert manifest["transform"]["sha256"] == sha(Path(module.__file__).read_bytes())
    for rel, info in manifest["files"].items():
        data = (paths[2] / rel).read_bytes()
        assert len(data) == info["bytes"] and sha(data) == info["sha256"]


@pytest.mark.parametrize("kind", ["empty_directory", "file"])
def test_os_publication_refuses_concurrent_destination_at_rename(tmp_path, monkeypatch, kind):
    paths = fixture(tmp_path)
    module = api()
    rename = module.os.rename
    out = paths[2]

    def race(source, destination):
        assert not out.exists()
        if kind == "empty_directory":
            out.mkdir()
        else:
            out.write_bytes(b"keep")
        return rename(source, destination)  # exercise real OS no-clobber behavior

    monkeypatch.setattr(module.os, "rename", race)
    with pytest.raises(FileExistsError):
        module.recover_sources(*paths)
    assert not list(tmp_path.glob(".recovered-*.tmp"))
    if kind == "empty_directory":
        assert list(out.iterdir()) == []
    else:
        assert out.read_bytes() == b"keep"


@pytest.mark.parametrize("kind", ["manifest", "object"])
def test_readback_detects_corruption_even_when_write_reports_success(tmp_path, monkeypatch, kind):
    paths = fixture(tmp_path)
    module = api()
    text_write, bytes_write = Path.write_text, Path.write_bytes

    def corrupt_text(path, data, *args, **kwargs):
        result = text_write(path, data, *args, **kwargs)
        if kind == "manifest" and path.name == "manifest.json":
            bytes_write(path, b"x" * len(path.read_bytes()))
        return result

    def corrupt_bytes(path, data, *args, **kwargs):
        result = bytes_write(path, data, *args, **kwargs)
        if kind == "object" and path.suffix == ".utf8":
            bytes_write(path, b"x" * len(data))
        return result

    monkeypatch.setattr(Path, "write_text", corrupt_text)
    monkeypatch.setattr(Path, "write_bytes", corrupt_bytes)
    with pytest.raises(OSError, match="readback mismatch"):
        module.recover_sources(*paths)
    assert not paths[2].exists()
    assert not list(tmp_path.glob(".recovered-*.tmp"))


def test_depth_boundary_and_escaped_quotes_are_not_overrejected(tmp_path):
    paths = fixture(tmp_path)
    # Root object plus 63 arrays = the documented 64-container limit.
    paths[1].write_bytes(b'{"nested":' + b'[' * 63 + b'0' + b']' * 63 +
                         b',"string":"\\\"[[[\\\\[[["}\n')
    api().recover_sources(*paths)
    assert (paths[2] / "manifest.json").exists()

