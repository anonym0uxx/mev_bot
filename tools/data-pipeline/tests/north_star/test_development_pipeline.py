"""Synthetic development-only integration tests; no new real source exposure."""
import importlib
import hashlib
import json
import builtins
import io
import os
from contextlib import contextmanager
from copy import deepcopy
from pathlib import Path

import zstandard

import pytest


def api():
    assert importlib.util.find_spec("north_star.development_pipeline") is not None, "integration driver missing"
    return importlib.import_module("north_star.development_pipeline")


def tree_snapshot(root):
    return {p.relative_to(root).as_posix(): (
        p.is_dir(), p.stat().st_ino, p.stat().st_mode, p.stat().st_mtime_ns,
        None if p.is_dir() else p.read_bytes())
        for p in [root, *root.rglob("*")]}


@contextmanager
def write_guard(monkeypatch, *, block=True):
    """Observe Python file mutation attempts, optionally refusing before IO."""
    writes = []
    def wrap(original, mutates):
        def guarded(*args, **kwargs):
            if mutates(*args, **kwargs):
                writes.append((original.__name__, str(args[0])))
                if block:
                    raise AssertionError("write attempted: " + str(writes[-1]))
            return original(*args, **kwargs)
        return guarded
    def write_mode(file, mode="r", *args, **kwargs):
        return any(flag in mode for flag in "wax+")
    def write_flags(path, flags, *args, **kwargs):
        return bool(flags & (os.O_WRONLY | os.O_RDWR | os.O_CREAT | os.O_TRUNC | os.O_APPEND))
    with monkeypatch.context() as guard:
        for owner in (builtins, io):
            guard.setattr(owner, "open", wrap(owner.open, write_mode))
        guard.setattr(os, "open", wrap(os.open, write_flags))
        for name in ("mkdir", "link", "unlink", "remove", "rename", "replace", "rmdir", "utime", "chmod"):
            guard.setattr(os, name, wrap(getattr(os, name), lambda *a, **k: True))
        yield writes


def repin_manifest(module, out, manifest):
    """Synthetic corruption only: preserve identity, refresh completion hash."""
    (out / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
    complete = module._json(out / "COMPLETE.json")
    complete["manifest"] = module._file(out / "manifest.json")
    (out / "COMPLETE.json").write_text(json.dumps(complete), encoding="utf-8")


@pytest.mark.parametrize("block", [False, True])
def test_omitted_partition_inventory_refuses_without_any_write(case, monkeypatch, block):
    module, config, out = case
    manifest = module.run_development(out, config=config)
    for name in ("events.parquet", "events.parquet.receipt.json"):
        (out / name).unlink()
        del manifest["files"][name]
    repin_manifest(module, out, manifest)
    before = tree_snapshot(out.parent)
    with write_guard(monkeypatch, block=block) as writes:
        with pytest.raises(ValueError):
            module.run_development(out, config=config, resume=True)
    assert tree_snapshot(out.parent) == before
    assert writes == []


@pytest.mark.parametrize("record", [
    {"bytes": False, "sha256": hashlib.sha256(b"").hexdigest()},
    {"bytes": 0.0, "sha256": hashlib.sha256(b"").hexdigest()},
    {"bytes": -1, "sha256": "0" * 64},
    {"bytes": 128 * 1024 * 1024 + 1, "sha256": "0" * 64},
    {"bytes": "0", "sha256": "0" * 64},
    {"bytes": 0, "sha256": "x" * 64},
    {"bytes": 0, "sha256": 0},
    {"bytes": 0, "sha256": "0" * 63},
    {"bytes": 0, "sha256": "0" * 64, "extra": None},
    {"bytes": 0}, None,
])
def test_completed_manifest_metadata_is_strict_before_summarization(case, monkeypatch, record):
    module, config, out = case
    manifest = module.run_development(out, config=config)
    manifest["files"]["transactions/rejected.ndjson"] = record
    repin_manifest(module, out, manifest)
    before = tree_snapshot(out.parent)
    monkeypatch.setattr(module, "_summarize", lambda *a: pytest.fail("summarized invalid metadata"))
    with write_guard(monkeypatch) as writes:
        with pytest.raises(ValueError, match="metadata"):
            module.run_development(out, config=config, resume=True)
    assert tree_snapshot(out.parent) == before
    assert writes == []


def test_exposure_gate_uses_current_pinned_policy_and_literal_identities():
    module = api()
    gate = module.exposure_gate(module.RAW_SOURCE_ID, module.NARRATIVE_SOURCE_ID)
    assert gate["split"] == "DEVELOPMENT"
    assert gate["fit_authorized"] is False
    assert gate["reservation_routing"] == "QUARANTINED"
    for source in ("unknown", " " + module.RAW_SOURCE_ID, "https://www.youtube.com/watch?v=hxXZTE9wWdA"):
        with pytest.raises(ValueError, match="source identity"):
            module.exposure_gate(source, module.NARRATIVE_SOURCE_ID)


@pytest.fixture
def case(tmp_path, monkeypatch):
    module = api()
    raw = tmp_path / "input" / "raw.zst"
    raw.parent.mkdir()
    data = b"".join((json.dumps({"record_type": "slot", "slot": 42,
        "recv_unix_ms": 123456, "record_index": i,
        "payload": {"slot": 42, "parent": 41, "status": "Finalized"}}) + "\n").encode()
        for i in range(100))
    raw.write_bytes(zstandard.ZstdCompressor().compress(data))
    recovery = tmp_path / "existing_recovery"
    recovery.mkdir()
    rows = [{"claim_id": str(i), "content_id": str(i), "admitted": False,
        "availability_verified": False, "available_at_ms": None, "source_state": "UNKNOWN",
        "rights_state": "UNKNOWN", "temporal_state": "UNKNOWN", "split": "development",
        "evidence": {"span_unit": "BYTE", "span_start": 0, "span_end": 1}}
        for i in range(50)]
    recovered = recovery / "recovered_sources.jsonl"
    recovered.write_bytes(b"".join((json.dumps(row) + "\n").encode() for row in rows))
    manifest = recovery / "manifest.json"
    manifest.write_text(json.dumps({"schema_version": "narrative_sources_v1", "admitted": False,
        "split": "development", "counts": {"claims": 50, "exact_prefix_spans": 50},
        "files": {recovered.name: {"sha256": hashlib.sha256(recovered.read_bytes()).hexdigest(),
                                  "bytes": recovered.stat().st_size}}}), encoding="utf-8")
    monkeypatch.setattr(module, "_source_binding", lambda config: {"scope": "SYNTHETIC_TEST_ONLY"})
    config = module.default_config()
    config["raw_path"] = str(raw)
    config["raw_sha256"] = hashlib.sha256(raw.read_bytes()).hexdigest()
    config["recovery_manifest_path"] = str(manifest)
    config["recovery_manifest_sha256"] = hashlib.sha256(manifest.read_bytes()).hexdigest()
    # Synthetic fixture injection only: production catalog has exactly one pinned pair.
    monkeypatch.setattr(module, "_APPROVED_INPUTS", {
        (str(raw), config["raw_sha256"], str(manifest), config["recovery_manifest_sha256"])})
    return module, config, tmp_path / "result"


def test_composition_conserves_rows_leaves_joins_null_and_resumes_readonly(case, monkeypatch):
    module, config, out = case
    result = module.run_development(out, config=config)
    assert result["state"] == "COMPLETE_UNADMITTED_DEVELOPMENT"
    assert result["conservation"]["raw_projection"] == {"input": 100, "accepted": 100, "rejected": 0}
    assert result["conservation"]["partition"] == {"input": 100, "output": 100}
    assert result["conservation"]["transaction_truth"]["attempted"] == 0
    assert result["conservation"]["recovery_input"] == {"input": 50, "referenced": 50, "transformed": 0}
    assert result["joins"] == {"source_context": None, "mint": None, "available_at_ms": None,
                               "reason": "NO_VERIFIED_CLOCK_AND_MINT_PROOF"}
    assert result["states"]["source_admission"] == "UNADMITTED"
    assert result["states"]["training_eligible"] is False
    assert (out / "COMPLETE.json").exists()
    before = tree_snapshot(out.parent)
    monkeypatch.setattr(module.raw_adapter, "project_bounded_file", lambda *a, **k: pytest.fail("reprojected"))
    monkeypatch.setattr(module.partitions, "write_partition", lambda *a, **k: pytest.fail("writer called"))
    monkeypatch.setattr(module.transaction_truth, "sample_bounded_file", lambda *a, **k: pytest.fail("resampled"))
    with write_guard(monkeypatch) as writes:
        assert module.run_development(out, config=config, resume=True) == result
    assert tree_snapshot(out.parent) == before
    assert writes == []


def test_summarize_missing_partition_cannot_create_even_without_inventory_gate(case, monkeypatch):
    module, config, out = case
    result = module.run_development(out, config=config)
    for name in ("events.parquet", "events.parquet.receipt.json"):
        (out / name).unlink()
    before = tree_snapshot(out.parent)
    with write_guard(monkeypatch) as writes:
        with pytest.raises((ValueError, FileNotFoundError)):
            module._summarize(out, result["identity"])
    assert tree_snapshot(out.parent) == before
    assert writes == []


@pytest.mark.parametrize("stage", ["raw", "partition", "transactions", "manifest"])
def test_interruption_never_claims_completion_and_partial_resume_refuses(case, monkeypatch, stage):
    module, config, out = case
    def stop(name):
        if name == stage:
            raise RuntimeError("simulated interruption")
    monkeypatch.setattr(module, "_checkpoint", stop)
    with pytest.raises(RuntimeError, match="interruption"):
        module.run_development(out, config=config)
    assert not (out / "COMPLETE.json").exists()
    with pytest.raises(ValueError, match="incomplete.*new target"):
        module.run_development(out, config=config, resume=True)
    assert Path(config["raw_path"]).exists()


@pytest.mark.parametrize("field,value", [("batch_rows", 16), ("source_commitment", "CONFIRMED")])
def test_resume_config_mismatch_refuses(case, field, value):
    module, config, out = case
    module.run_development(out, config=config)
    changed = dict(config, **{field: value})
    with pytest.raises(ValueError, match="identity mismatch"):
        module.run_development(out, config=changed, resume=True)


def test_code_and_output_hash_resume_refusal(case, monkeypatch):
    module, config, out = case
    module.run_development(out, config=config)
    old = module._code_identity
    monkeypatch.setattr(module, "_code_identity", lambda: {**old(), "driver": "0" * 64})
    with pytest.raises(ValueError, match="identity mismatch"):
        module.run_development(out, config=config, resume=True)
    monkeypatch.setattr(module, "_code_identity", old)
    with (out / "raw" / "events.jsonl").open("ab") as handle:
        handle.write(b" ")
    with pytest.raises(ValueError, match="hash|integrity"):
        module.run_development(out, config=config, resume=True)


def test_input_hash_mismatch_and_unknown_path_refuse_before_transform(case):
    module, config, out = case
    changed = dict(config, raw_path=str(Path(config["raw_path"]).with_name("other.zst")))
    with pytest.raises(ValueError, match="approved input"):
        module.run_development(out, config=changed)
    with Path(config["raw_path"]).open("ab") as handle:
        handle.write(b"x")
    with pytest.raises(ValueError, match="input hash"):
        module.run_development(out, config=config)
    assert not out.exists()


def test_recovery_corruption_refuses_without_transform_or_deletion(case):
    module, config, out = case
    path = Path(config["recovery_manifest_path"]).with_name("recovered_sources.jsonl")
    path.write_bytes(b"bad")
    with pytest.raises(ValueError, match="integrity"):
        module.run_development(out, config=config)
    assert path.read_bytes() == b"bad"
    assert not out.exists()


@pytest.mark.parametrize("field,value", [("raw_limit", 101), ("transaction_limit", 11),
    ("recovery_limit", 51), ("raw_limit", True), ("batch_rows", 0), ("extra", 1)])
def test_bounds_and_extra_config_refuse(case, field, value):
    module, config, out = case
    with pytest.raises(ValueError, match="config"):
        module.run_development(out, config=dict(config, **{field: value}))
    assert not out.exists()


def test_policy_bytes_mutation_refuses_before_input_access(tmp_path, monkeypatch):
    module = api()
    policy_dir = tmp_path / "north_star"
    policy_dir.mkdir()
    for name in module.POLICIES:
        (policy_dir / name).write_bytes((module.ROOT / "north_star" / name).read_bytes() + b" ")
    monkeypatch.setattr(module, "ROOT", tmp_path)
    with pytest.raises(ValueError, match="policy file hash"):
        module.exposure_gate(module.RAW_SOURCE_ID, module.NARRATIVE_SOURCE_ID)


@pytest.mark.parametrize("filename", ["raw/events.jsonl", "events.parquet", "transactions/reconciled.ndjson", "manifest.json"])
def test_missing_output_completed_resume_never_recreates(case, filename):
    module, config, out = case
    module.run_development(out, config=config)
    target = out / filename
    target.unlink()  # synthetic fault only; driver must not heal or discard evidence
    with pytest.raises((FileNotFoundError, ValueError)):
        module.run_development(out, config=config, resume=True)
    assert not target.exists()


def test_no_overwrite_existing_target_and_no_resume_new_target(case):
    module, config, out = case
    with pytest.raises(ValueError, match="existing completed"):
        module.run_development(out, config=config, resume=True)
    assert not out.exists()
    module.run_development(out, config=config)
    with pytest.raises(FileExistsError):
        module.run_development(out, config=config)


def test_commit_marker_publication_interruption_and_input_mutation(case, monkeypatch):
    module, config, out = case
    original = module._publish_json
    def fail_marker(path, document):
        if path.name == "COMPLETE.json":
            raise OSError("simulated marker publication failure")
        original(path, document)
    monkeypatch.setattr(module, "_publish_json", fail_marker)
    with pytest.raises(OSError, match="publication failure"):
        module.run_development(out, config=config)
    assert (out / "manifest.json").exists() and not (out / "COMPLETE.json").exists()
    with pytest.raises(ValueError, match="incomplete.*new target"):
        module.run_development(out, config=config, resume=True)
    monkeypatch.setattr(module, "_publish_json", original)
    def mutate(stage):
        if stage == "transactions":
            with Path(config["raw_path"]).open("ab") as handle:
                handle.write(b"changed")
    monkeypatch.setattr(module, "_checkpoint", mutate)
    another = out.with_name("another")
    with pytest.raises(ValueError, match="input hash"):
        module.run_development(another, config=config)
    assert not (another / "COMPLETE.json").exists()


def test_transaction_branch_preserves_real_helper_rejection_accounting(case, monkeypatch):
    module, config, out = case
    import base58
    key = base58.b58encode(bytes([1]) * 32).decode()
    sig = base58.b58encode(bytes([2]) * 64).decode()
    row = {"record_type": "transaction", "slot": 10, "recv_unix_ms": 20, "record_index": 0,
           "payload": {"signature_b58": sig, "signatures_b58": [sig], "raw_hash": "x", "tx_index": 0,
                       "message": {"account_keys_b58": [key]}, "meta": {
                           "loaded_writable_addresses_b58": [], "loaded_readonly_addresses_b58": [],
                           "err_is_none": True, "err_hex": None, "fee": 5,
                           "pre_balances": [100], "post_balances": [95],
                           "pre_token_balances": [], "post_token_balances": []}}}
    selected = []
    for i in range(12):
        current = deepcopy(row)
        current["record_index"] = i
        if i == 3:
            current["payload"]["meta"]["pre_balances"] = []
        selected.append(current)
    raw = Path(config["raw_path"])
    raw.write_bytes(zstandard.ZstdCompressor().compress(b"".join(
        (json.dumps(current) + "\n").encode() for current in selected)))
    config["raw_sha256"] = hashlib.sha256(raw.read_bytes()).hexdigest()
    monkeypatch.setattr(module, "_APPROVED_INPUTS", {(config["raw_path"], config["raw_sha256"],
        config["recovery_manifest_path"], config["recovery_manifest_sha256"])})
    result = module.run_development(out, config=config)
    counts = result["conservation"]["transaction_truth"]
    assert counts["attempted"] == 10 and counts["reconciled"] == 9 and counts["rejected"] == 1
    assert counts["native_endpoint_rows"] == 9 and counts["token_endpoint_rows"] == 0
    assert result["states"]["canonical_transaction_admissions"] == 0


def test_reserved_clock_is_not_silently_allowed_by_exposed_label(case, monkeypatch):
    module, config, out = case
    # Fixtures may change bytes/catalog; gate must still refuse reserved-time reads
    # before publishing success, even under the exposed dataset label.
    raw = Path(config["raw_path"])
    row = {"record_type": "slot", "slot": 42, "recv_unix_ms": 1789542000000,
           "record_index": 0, "payload": {"slot": 42, "parent": 41, "status": "Finalized"}}
    raw.write_bytes(zstandard.ZstdCompressor().compress((json.dumps(row) + "\n").encode()))
    config["raw_sha256"] = hashlib.sha256(raw.read_bytes()).hexdigest()
    monkeypatch.setattr(module, "_APPROVED_INPUTS", {(config["raw_path"], config["raw_sha256"],
        config["recovery_manifest_path"], config["recovery_manifest_sha256"])})
    with pytest.raises(ValueError, match="reserved window"):
        module.run_development(out, config=config)
    assert not (out / "COMPLETE.json").exists()



def test_real_source_identity_is_proven_by_pinned_capture_manifest():
    module = api()
    assert hasattr(module, "_source_binding"), "source path label alone is not identity proof"
    evidence = module._source_binding(module.default_config())
    assert evidence["session_id"] == "20260909_144906_000490"
    assert evidence["raw_file"]["sha256"] == module.RAW_SHA256
    assert evidence["raw_file"]["bytes"] == 40872244
    assert evidence["source_id"] == module.RAW_SOURCE_ID
