"""Synthetic engineering fixtures only; never training/admission evidence."""
import hashlib
import importlib.util
import json
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = "preserved/part0000.ndjson.zst"
SOURCE_SHA = "a" * 64
SIGNATURE = "2" * 88


def adapter():
    path = ROOT / "src/north_star/raw_adapter.py"
    assert path.exists(), "raw adapter not implemented"
    spec = importlib.util.spec_from_file_location("raw_adapter_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def encode(kind="slot", payload=None, **changes):
    record = dict(record_type=kind, slot=42, recv_unix_ms=123456,
                  record_index=7, payload=payload if payload is not None else
                  dict(slot=42, parent=41, status="Finalized"))
    record.update(changes)
    return (json.dumps(record) + "\n").encode()


def project(raw, **changes):
    kwargs = dict(source_path=SOURCE, source_sha256=SOURCE_SHA)
    kwargs.update(changes)
    return adapter().project_record(raw, **kwargs)


def test_slot_envelope_keeps_observation_provenance_without_finality_inference():
    raw = encode()
    event = project(raw)
    assert event["record_type"] == "slot"
    assert event["source_path"] == SOURCE
    assert event["source_sha256"] == SOURCE_SHA
    assert event["record_index"] == 7
    assert event["raw_record_sha256"] == hashlib.sha256(raw).hexdigest()
    assert event["revision"] == 0
    assert event["revision_basis"] == "adapter_initial_projection"
    assert event["slot"] == 42
    assert event["observed_at_unix_ms"] == 123456
    assert event["observation_clock"] == "recorder_host_unix_ms"
    assert event["block_time_unix_s"] is None
    assert event["source_commitment"] is None
    assert event["slot_status"] == "Finalized"
    assert event["signature"] is None
    assert event["training_eligible"] is False
    assert event["projection_scope"] == "envelope_only_development"
    assert not {"mint", "trader", "finality", "event_time", "available_at"} & event.keys()
    assert project(raw) == event


def tx_payload():
    return dict(signature_b58=SIGNATURE, signatures_b58=[SIGNATURE, "3" * 88],
                raw_hash="b" * 64, tx_index=9, is_vote=False,
                message={"account_keys_b58": ["Static", "Static"]},
                meta={"loaded_writable_addresses_b58": ["Writable"],
                      "loaded_readonly_addresses_b58": ["Readonly", "Static"]})


def account_payload():
    return dict(pubkey_b58="Account", owner_b58="Owner", lamports=5,
                executable=False, rent_epoch=2**64 - 1, data_b64="YWJj",
                data_len=3, write_version=99, txn_signature_b58=SIGNATURE,
                raw_hash="c" * 64)


def block_payload():
    return dict(slot=42, blockhash="Block", parent_slot=41,
                parent_blockhash="Parent", block_height=123,
                block_time=99, executed_transaction_count=10, entries_count=5)


def test_transaction_preserves_full_signatures_key_groups_and_duplicate_indexes():
    event = project(encode("transaction", tx_payload()),
                    source_commitment="confirmed", revision=3)
    assert event["signature"] == SIGNATURE
    assert event["signatures"] == [SIGNATURE, "3" * 88]
    assert event["static_account_keys"] == ["Static", "Static"]
    assert event["loaded_writable_account_keys"] == ["Writable"]
    assert event["loaded_readonly_account_keys"] == ["Readonly", "Static"]
    assert event["account_keys"] == ["Static", "Static", "Writable", "Readonly", "Static"]
    assert event["transaction_index"] == 9
    assert event["revision"] == 3
    assert event["revision_basis"] == "caller_supplied_projection_revision"
    assert event["source_commitment"] == "confirmed"
    assert event["block_time_unix_s"] is None
    assert event["slot_status"] is None
    assert event["source_payload_raw_hash"] == "b" * 64
    assert event["raw_record_sha256"] != event["source_payload_raw_hash"]


def test_record_hash_changes_for_same_signature_content_and_whitespace():
    payload = tx_payload()
    first = encode("transaction", payload)
    payload["tx_index"] = 10
    second = encode("transaction", payload)
    outputs = [project(raw) for raw in [first, second, first + b" "]]
    assert len({row["raw_record_sha256"] for row in outputs}) == 3
    assert len({row["event_id"] for row in outputs}) == 3
    assert len({row["signature"] for row in outputs}) == 1
    assert project(first, source_path="different/part0000.ndjson.zst")["event_id"] != outputs[0]["event_id"]


def test_account_signature_write_version_not_transaction_or_canonical_revision():
    payload = account_payload()
    event = project(encode("account", payload))
    assert event["record_type"] == "account"
    assert event["signature"] == SIGNATURE
    assert event["account_pubkey"] == "Account"
    assert event["account_write_version"] == 99
    assert event["revision"] == 0
    assert event["account_keys"] is None
    payload["txn_signature_b58"] = None
    assert project(encode("account", payload))["signature"] is None


@pytest.mark.parametrize("block_time", [99, None, -1])
def test_blockmeta_source_time_is_not_receive_time(block_time):
    payload = block_payload()
    payload["block_time"] = block_time
    event = project(encode("block_meta", payload))
    assert event["record_type"] == "block_meta"
    assert event["block_time_unix_s"] == block_time
    assert event["blockhash"] == "Block"
    assert event["observed_at_unix_ms"] == 123456
    assert event["source_commitment"] is None


@pytest.mark.parametrize("raw, reason", [
    (b"not json", "invalid_json"),
    (b"\xff", "invalid_json"),
    (b"[]", "invalid_envelope"),
    (b'{"slot": 1, "slot": 2}', "duplicate_json_key"),
    (b'{"x": NaN}', "nonfinite_json_number"),
    (encode(record_type="unknown"), "unknown_record_type"),
    (encode(record_type=[]), "unknown_record_type"),
    (encode(slot=True), "invalid_slot"),
    (encode(slot=-1), "invalid_slot"),
    (encode(recv_unix_ms=1.5), "invalid_recv_unix_ms"),
    (encode(record_index="7"), "invalid_record_index"),
    (encode(payload=[]), "invalid_payload"),
    (encode(payload={}), "invalid_slot"),
    (encode(payload=dict(slot=43, parent=41, status="Processed")), "slot_mismatch"),
    (encode(payload=dict(slot=42, parent=41, status="Unknown")), "unknown_slot_status"),
    (encode("transaction", {}), "invalid_signature_b58"),
    (encode("transaction", dict(tx_payload(), message=None)), "invalid_message"),
    (encode("transaction", dict(tx_payload(), meta=None)), "invalid_meta"),
    (encode("transaction", dict(tx_payload(), meta={})), "invalid_loaded_writable_addresses_b58"),
    (encode("transaction", dict(tx_payload(), signatures_b58=["other"])), "signature_mismatch"),
    (encode("account", dict(account_payload(), data_b64="???")), "invalid_data_b64"),
    (encode("account", dict(account_payload(), data_len=4)), "data_length_mismatch"),
    (encode("account", dict(account_payload(), write_version=True)), "invalid_write_version"),
    (encode("block_meta", dict(block_payload(), block_time=True)), "invalid_block_time"),
])
def test_malformed_records_fail_closed_with_reason(raw, reason):
    module = adapter()
    with pytest.raises(module.RawProjectionError) as error:
        module.project_record(raw, source_path=SOURCE, source_sha256=SOURCE_SHA)
    assert error.value.reason == reason


@pytest.mark.parametrize("kwargs,reason", [
    ({"source_path": ""}, "invalid_source_path"),
    ({"source_sha256": "wrong"}, "invalid_source_sha256"),
    ({"revision": True}, "invalid_revision"),
    ({"source_commitment": "ROOTED"}, "invalid_source_commitment"),
])
def test_invalid_projection_context_rejected(kwargs, reason):
    with pytest.raises(ValueError, match=reason):
        project(encode(), **kwargs)


def test_bounded_zstd_slice_real_bytes_hash_and_rejection_receipt(tmp_path):
    import zstandard
    source_dir = tmp_path / "raw"
    source_dir.mkdir()
    source = source_dir / "full_filename_part0000.ndjson.zst"
    rows = [encode(record_index=i) for i in range(101)]
    rows[4] = b"malformed\n"
    source.write_bytes(zstandard.ZstdCompressor().compress(b"".join(rows)))
    before = source.read_bytes()
    output = tmp_path / "development"
    report = adapter().project_bounded_file(str(source), output_dir=output, limit=100)
    assert report["rows_read"] == 100
    assert report["accepted"] == 99
    assert report["rejected"] == 1
    assert report["source_sha256"] == hashlib.sha256(before).hexdigest()
    assert report["source_unchanged"] is True
    assert report["training_eligible"] is False
    assert report["limit_reached"] is True
    assert report["eof_observed"] is False
    assert source.read_bytes() == before
    saved = json.loads((output / "report.json").read_text())
    assert report == saved
    events = [json.loads(line) for line in (output / "events.jsonl").read_text().splitlines()]
    assert len(events) == 99
    assert events[0]["raw_record_sha256"] == hashlib.sha256(rows[0]).hexdigest()
    assert events[-1]["record_index"] == 99
    assert report["failures"][0]["line_index"] == 4
    assert report["failures"][0]["reason"] == "invalid_json"
    with pytest.raises(FileExistsError):
        adapter().project_bounded_file(str(source), output_dir=output)


@pytest.mark.parametrize("limit", [0, 101, True])
def test_no_unbounded_or_source_directory_outputs(tmp_path, limit):
    with pytest.raises(ValueError, match="invalid_limit"):
        adapter().project_bounded_file("missing.ndjson.zst", output_dir=tmp_path / "new", limit=limit)


def test_observed_manifest_commitment_is_preserved_literally():
    assert project(encode(), source_commitment="CONFIRMED")["source_commitment"] == "CONFIRMED"


def test_nonfinite_overflow_in_unprojected_payload_fails_closed():
    raw = encode().replace(b'"parent": 41', b'"parent": 41, "extra": 1e999')
    with pytest.raises(ValueError, match="nonfinite_json_number"):
        project(raw)


@pytest.mark.parametrize("token", ["9" * 5000, "-" + "9" * 5000,
                                  str(2**64), str(-(2**63) - 1)],
                         ids=["giant_positive", "giant_negative", "above_u64", "below_i64"])
@pytest.mark.parametrize("location", ["parent", "opaque"])
def test_out_of_range_json_integer_is_stable_rejection(token, location):
    replacement = ('"parent": ' + token if location == "parent" else
                   '"parent": 41, "opaque": [{"number": ' + token + '}]')
    raw = encode().replace(b'"parent": 41', replacement.encode())
    module = adapter()
    with pytest.raises(module.RawProjectionError) as error:
        module.project_record(raw, source_path=SOURCE, source_sha256=SOURCE_SHA)
    assert error.value.reason == "json_integer_out_of_range"


def test_integer_rejection_continues_bounded_batch_with_exact_hash(tmp_path):
    import zstandard
    source_dir = tmp_path / "raw"
    source_dir.mkdir()
    source = source_dir / "synthetic.ndjson.zst"
    malformed = encode().replace(b'"parent": 41', b'"parent": ' + b"9" * 5000)
    rows = [malformed, encode(record_index=8)]
    source.write_bytes(zstandard.ZstdCompressor().compress(b"".join(rows)))
    output = tmp_path / "projection"
    report = adapter().project_bounded_file(str(source), output_dir=output, limit=2)
    assert (report["rows_read"], report["accepted"], report["rejected"]) == (2, 1, 1)
    assert report["failures"] == [dict(line_index=0, reason="json_integer_out_of_range",
                                     raw_record_sha256=hashlib.sha256(malformed).hexdigest())]
    assert json.loads((output / "report.json").read_text()) == report
    event = json.loads((output / "events.jsonl").read_bytes())
    assert event["record_index"] == 8
    assert event["source_line_index"] == 1
    assert event["source_admission"] == "unadmitted"
    assert event["training_eligible"] is False


@pytest.mark.parametrize("value", [-(2**63), 2**63 - 1, 2**63, 2**64 - 1])
def test_json_integer_domain_boundaries_in_opaque_payload_are_allowed(value):
    raw = encode(payload=dict(slot=42, parent=41, status="Finalized", opaque=value))
    assert project(raw)["record_type"] == "slot"


@pytest.mark.parametrize("block_time", [-(2**63), 2**63 - 1])
def test_signed_block_time_boundaries_are_preserved(block_time):
    assert project(encode("block_meta", dict(block_payload(), block_time=block_time)))[
        "block_time_unix_s"] == block_time


def test_unsigned_envelope_boundary_is_preserved():
    assert project(encode(record_index=2**64 - 1))["record_index"] == 2**64 - 1


@pytest.mark.parametrize("raw,reason", [
    (b'{"x":{"a":1,"a":2}}', "duplicate_json_key"),
    (b'{"x":NaN}', "nonfinite_json_number"),
    (b'{"x":Infinity}', "nonfinite_json_number"),
    (b'{"x":-Infinity}', "nonfinite_json_number"),
    (b'{"x":1e999}', "nonfinite_json_number"),
    (b'{"x":-1e999}', "nonfinite_json_number"),
    (encode("block_meta", dict(block_payload(), block_time=2**63)), "invalid_block_time"),
    (encode(slot=-1), "invalid_slot"),
])
def test_specific_parser_and_field_reasons_survive_error_normalization(raw, reason):
    module = adapter()
    with pytest.raises(module.RawProjectionError) as error:
        module.project_record(raw, source_path=SOURCE, source_sha256=SOURCE_SHA)
    assert error.value.reason == reason


@pytest.mark.parametrize("value", ["\ud800", "\udfff", "\ud800x", "\udc00\ud800"],
                         ids=["high", "low", "high_then_text", "reversed_pair"])
@pytest.mark.parametrize("location", ["projected", "opaque_value", "opaque_key", "top_key"])
def test_malformed_unicode_scalars_rejected_anywhere(value, location):
    payload = tx_payload()
    extra = {}
    if location == "projected":
        payload["message"]["account_keys_b58"] = [value]
    elif location == "opaque_value":
        payload["opaque"] = [{"nested": [value]}]
    elif location == "opaque_key":
        payload["opaque"] = [{value: "nested"}]
    else:
        extra[value] = "unknown field"
    module = adapter()
    with pytest.raises(module.RawProjectionError) as error:
        module.project_record(encode("transaction", payload, **extra),
                              source_path=SOURCE, source_sha256=SOURCE_SHA)
    assert error.value.reason == "invalid_unicode_scalar"


def test_malformed_unicode_source_path_rejected_before_projection():
    module = adapter()
    with pytest.raises(module.RawProjectionError) as error:
        module.project_record(encode(), source_path="preserved/\ud800.zst",
                              source_sha256=SOURCE_SHA)
    assert error.value.reason == "invalid_unicode_scalar"


def test_unicode_rejection_continues_batch_and_valid_pair_writes_utf8(tmp_path):
    import zstandard
    source_dir = tmp_path / "raw"
    source_dir.mkdir()
    source = source_dir / "synthetic.ndjson.zst"
    malformed = encode("account", dict(account_payload(), pubkey_b58="\ud800"))
    valid = encode("account", dict(account_payload(), pubkey_b58="\U0001f680"), record_index=8)
    assert b"\\ud83d\\ude80" in valid
    source.write_bytes(zstandard.ZstdCompressor().compress(malformed + valid))
    output = tmp_path / "projection"
    report = adapter().project_bounded_file(str(source), output_dir=output, limit=2)
    assert (report["rows_read"], report["accepted"], report["rejected"]) == (2, 1, 1)
    assert report["failures"] == [dict(line_index=0, reason="invalid_unicode_scalar",
                                     raw_record_sha256=hashlib.sha256(malformed).hexdigest())]
    assert json.loads((output / "report.json").read_text()) == report
    event = json.loads((output / "events.jsonl").read_bytes().decode("utf-8"))
    assert event["account_pubkey"] == "\U0001f680"
    assert event["source_line_index"] == 1
    assert event["source_admission"] == "unadmitted"
    assert event["training_eligible"] is False


@pytest.mark.parametrize("value", ["\ud7ff", "\ue000", "\U0010ffff", "\U0001f680", "e\u0301"])
def test_valid_unicode_scalars_preserved_without_normalization(value):
    raw = encode("account", dict(account_payload(), pubkey_b58=value))
    event = project(raw)
    assert event["account_pubkey"] == value
    assert event["raw_record_sha256"] == hashlib.sha256(raw).hexdigest()
    json.dumps(event, ensure_ascii=False).encode("utf-8")


def test_source_directory_is_never_an_output_target(tmp_path):
    with pytest.raises(ValueError, match="output_overlaps_source_directory"):
        adapter().project_bounded_file(str(tmp_path / "raw.ndjson.zst"), output_dir=tmp_path / "output")
