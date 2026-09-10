"""Synthetic layout/guard tests; no corpus, execution or person attribution."""
import base64
import hashlib
import importlib
import json
from pathlib import Path

import base58
import pytest
import zstandard

SYSTEM = "11111111111111111111111111111111"
TOKEN = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
TOKEN22 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
KEYS = [base58.b58encode(bytes([i]) * 32).decode() for i in range(1, 8)]
SIG = base58.b58encode(bytes([9]) * 64).decode()


def mod():
    return importlib.import_module("north_star.instruction_truth")


def b64(data):
    return base64.b64encode(data).decode()


def ix(data=bytes.fromhex("030100000000000000"), accounts=(0, 1, 2), program=3):
    return dict(program_id_index=program, accounts_b64=b64(bytes(accounts)), data_b64=b64(data))


def record(instruction=None, program=TOKEN, index=7):
    return dict(record_type="transaction", slot=42, recv_unix_ms=100, record_index=index,
                payload=dict(signature_b58=SIG, signatures_b58=[SIG], raw_hash="b" * 64,
                             tx_index=4, message=dict(account_keys_b58=KEYS[:3] + [program],
                             instructions=[instruction or ix()]),
                             meta=dict(loaded_writable_addresses_b58=[],
                                       loaded_readonly_addresses_b58=[], err_is_none=True,
                                       err_hex=None, inner_instructions=[])))


def raw(row):
    return (json.dumps(row) + "\n").encode()


def trace(row):
    return mod().trace_transaction(raw(row), source_path="synthetic.zst", source_sha256="a" * 64)


@pytest.mark.parametrize("program,data,accounts,kind,amount,decimals", [
    (SYSTEM, bytes.fromhex("020000000100000000000000"), (0, 1), "system_transfer", 1, None),
    (TOKEN, bytes.fromhex("030100000000000000"), (0, 1, 2), "token_transfer", 1, None),
    (TOKEN22, bytes.fromhex("030100000000000000"), (0, 1, 2), "token_transfer", 1, None),
    (TOKEN, bytes.fromhex("0c010000000000000002"), (0, 1, 2, 4), "token_transfer_checked", 1, 2),
    (TOKEN22, bytes.fromhex("0cffffffffffffffffff"), (0, 1, 2, 4), "token_transfer_checked", 2**64-1, 255),
    (SYSTEM, bytes.fromhex("020000000000000000000000"), (0, 1), "system_transfer", 0, None),
])
def test_reference_vectors(program, data, accounts, kind, amount, decimals):
    result = mod().decode_instruction(ix(data, accounts), KEYS[:3] + [program, KEYS[3]])
    assert result["classification_status"] == "accepted"
    assert result["instruction_kind"] == kind
    assert result["amount_raw"] == amount and type(result["amount_raw"]) is int
    assert result["decimals_argument"] == decimals
    assert result["source"]["account_index"] == 0
    assert result["destination"]["account_index"] == (2 if kind.endswith("checked") else 1)
    assert result["owner"] is None and result["beneficial_person"] is None
    assert result["trade"] is None and result["net_received_raw"] is None


@pytest.mark.parametrize("field,value,reason", [
    ("program_id_index", True, "invalid_program_id_index"),
    ("program_id_index", 3.0, "invalid_program_id_index"),
    ("program_id_index", "3", "invalid_program_id_index"),
    ("program_id_index", -1, "invalid_program_id_index"),
    ("program_id_index", 4, "invalid_program_id_index"),
    ("program_id_index", 256, "invalid_program_id_index"),
    ("accounts_b64", [0, True, 2], "invalid_accounts_b64"),
    ("accounts_b64", "AAEC\n", "invalid_accounts_b64"),
    ("accounts_b64", "AAH/", "invalid_account_index"),
    ("data_b64", "!", "invalid_data_b64"),
    ("data_b64", None, "invalid_data_b64"),
    ("data_b64", "Ax==", "invalid_data_b64"),
])
def test_bad_wire_fields(field, value, reason):
    instruction = ix(); instruction[field] = value
    with pytest.raises(mod().InstructionTruthError, match=reason):
        mod().decode_instruction(instruction, KEYS[:3] + [TOKEN])


@pytest.mark.parametrize("program,data,accounts", [
    (SYSTEM, bytes.fromhex("020000000100000000000000"), (0, 1)),
    (TOKEN, bytes.fromhex("030100000000000000"), (0, 1, 2)),
    (TOKEN22, bytes.fromhex("0c010000000000000002"), (0, 1, 2, 4)),
])
@pytest.mark.parametrize("change", ["truncate", "append"])
def test_exact_lengths(program, data, accounts, change):
    data = data[:-1] if change == "truncate" else data + b"\x00"
    with pytest.raises(mod().InstructionTruthError, match="invalid_instruction_data_length"):
        mod().decode_instruction(ix(data, accounts), KEYS[:3] + [program, KEYS[3]])


@pytest.mark.parametrize("bad", ["1"*33, "1"*31, " " + SYSTEM, "0"*32])
def test_no_identifier_repair(bad):
    with pytest.raises(mod().InstructionTruthError, match="invalid_program_id"):
        mod().decode_instruction(ix(), KEYS[:3] + [bad])
    with pytest.raises(mod().InstructionTruthError, match="invalid_instruction_account_key"):
        mod().decode_instruction(ix(), [bad] + KEYS[1:3] + [TOKEN])


@pytest.mark.parametrize("program,data,accounts,reason", [
    (KEYS[5], b"\x03" + bytes(8), (0, 1, 2), "unsupported_program"),
    (TOKEN, b"\x09", (0, 1, 2), "unsupported_opcode"),
    (TOKEN22, b"\x1a" + bytes(10), (0, 1, 2), "unsupported_opcode"),
    (SYSTEM, bytes(12), (0, 1), "unsupported_opcode"),
    (TOKEN, b"\x03" + bytes(8), (0, 1, 2, 4), "unsupported_extra_accounts"),
])
def test_unknown_not_reinterpreted(program, data, accounts, reason):
    result = mod().decode_instruction(ix(data, accounts), KEYS[:3] + [program, KEYS[3]])
    assert result["classification_status"] == "unknown" and result["reason"] == reason
    assert result["amount_raw"] is None


def test_too_few_accounts_reject():
    with pytest.raises(mod().InstructionTruthError, match="invalid_instruction_account_count"):
        mod().decode_instruction(ix(accounts=(0, 1)), KEYS[:3] + [TOKEN])


def test_trace_provenance_success_and_no_economic_claim():
    row = record(); result = trace(row); instruction = result["instructions"][0]
    assert result["raw_record_sha256"] == hashlib.sha256(raw(row)).hexdigest()
    assert result["transaction_status"] == "succeeded"
    assert instruction["execution_status"] == "outer_instruction_succeeded"
    assert instruction["executed_transfer_raw"] is None
    assert result["training_eligible"] is False and result["finality"] is None
    assert result["canonical_identity_status"] == "format_valid_only"


def test_failure_does_not_mean_executed_transfer():
    row = record(); row["payload"]["meta"].update(err_is_none=False, err_hex="0100")
    instruction = trace(row)["instructions"][0]
    assert instruction["classification_status"] == "accepted"
    assert instruction["execution_status"] == "transaction_failed_no_committed_transfer"
    assert instruction["executed_transfer_raw"] is None


def test_cpi_status_is_unknown_even_successful_outer_transaction():
    row = record(); row["payload"]["meta"]["inner_instructions"] = [dict(index=0, instructions=[dict(ix(), stack_height=2)])]
    result = trace(row); inner = result["instructions"][1]
    assert inner["outer_instruction_index"] == 0 and inner["inner_instruction_index"] == 0
    assert inner["stack_height"] == 2
    assert inner["execution_status"] == "unknown_cpi_outcome"
    assert inner["caller_identity"] is None and inner["executed_transfer_raw"] is None


def test_loaded_keys_order_and_malformed_unrelated_identity_quarantine():
    row = record(ix(accounts=(4, 0, 2)))
    meta = row["payload"]["meta"]
    meta["loaded_writable_addresses_b58"] = [KEYS[3]]
    meta["loaded_readonly_addresses_b58"] = ["1"*33]
    result = trace(row)
    assert result["instructions"][0]["source"] == dict(account_index=4, account_key=KEYS[3])
    assert result["account_keys"][-1] == "1"*33
    assert result["canonical_identity_status"] == "quarantined"


def test_malformed_instruction_does_not_hide_other_instructions():
    row = record(); row["payload"]["message"]["instructions"] += [dict(ix(), program_id_index=True), ix(b"\x09")]
    result = trace(row)
    assert [x["classification_status"] for x in result["instructions"]] == ["accepted", "rejected", "unknown"]
    assert result["instructions"][1]["raw_instruction"]["program_id_index"] is True


@pytest.mark.parametrize("value", [None, True, -1, 1, "0"])
def test_invalid_cpi_parent_rejects_transaction(value):
    row = record(); row["payload"]["meta"]["inner_instructions"] = [dict(index=value, instructions=[ix()])]
    with pytest.raises(mod().InstructionTruthError, match="invalid_inner_parent_index"):
        trace(row)


@pytest.mark.parametrize("changes,reason", [
    ({"err_is_none": None}, "unknown_transaction_status"),
    ({"err_is_none": True, "err_hex": "00"}, "transaction_status_mismatch"),
    ({"err_is_none": False, "err_hex": None}, "transaction_status_mismatch"),
    ({"inner_instructions": None}, "invalid_inner_instructions"),
])
def test_meta_rejections(changes, reason):
    row = record(); row["payload"]["meta"].update(changes)
    with pytest.raises(mod().InstructionTruthError, match=reason):
        trace(row)


def test_duplicate_json_rejects():
    data = raw(record()).replace(b'"slot": 42', b'"slot": 42, "slot": 43')
    with pytest.raises(mod().InstructionTruthError, match="duplicate_json_key"):
        mod().trace_transaction(data, source_path="synthetic", source_sha256="a"*64)


@pytest.mark.parametrize("height", [True, 1, -1, 2**32, "2"])
def test_bad_cpi_height_is_rejected_without_caller_guess(height):
    row = record()
    row["payload"]["meta"]["inner_instructions"] = [dict(index=0, instructions=[dict(ix(), stack_height=height)])]
    inner = trace(row)["instructions"][1]
    assert inner["classification_status"] == "rejected"
    assert inner["reason"] == "invalid_stack_height"
    assert inner["caller_identity"] is None and inner["executed_transfer_raw"] is None


def test_duplicate_inner_group_rejects_not_overwrites():
    row = record()
    row["payload"]["meta"]["inner_instructions"] = [dict(index=0, instructions=[ix()])] * 2
    with pytest.raises(mod().InstructionTruthError, match="duplicate_inner_parent_index"):
        trace(row)


@pytest.mark.parametrize("key_change,reason", [("duplicate", "duplicate_account_key"), ("too_many", "invalid_account_key_count")])
def test_ambiguous_account_domain_rejects(key_change, reason):
    row = record()
    keys = row["payload"]["message"]["account_keys_b58"]
    if key_change == "duplicate":
        keys.append(keys[0])
    else:
        keys.extend(KEYS * 40)
    with pytest.raises(mod().InstructionTruthError, match=reason):
        trace(row)


def test_failed_system_transfer_is_argument_not_executed_lamports():
    row = record(ix(bytes.fromhex("02000000ffffffffffffffff"), accounts=(0, 1)), SYSTEM)
    row["payload"]["meta"].update(err_is_none=False, err_hex="0100")
    got = trace(row)["instructions"][0]
    assert got["amount_raw"] == 2**64-1
    assert got["execution_status"] == "transaction_failed_no_committed_transfer"
    assert got["executed_transfer_raw"] is None


def test_readonly_group_is_after_loaded_writable():
    row = record(ix(bytes.fromhex("0c010000000000000002"), accounts=(0, 5, 1, 2)))
    row["payload"]["meta"].update(loaded_writable_addresses_b58=[KEYS[3]], loaded_readonly_addresses_b58=[KEYS[4]])
    assert trace(row)["instructions"][0]["mint"] == dict(account_index=5, account_key=KEYS[4])


def test_sampler_must_not_write_inside_source_directory(tmp_path):
    with pytest.raises(mod().InstructionTruthError, match="output_overlaps_source_directory"):
        mod().sample_bounded_file(tmp_path / "source.zst", tmp_path / "out", expected_sha256="a"*64)


def make_source(tmp_path, rows):
    folder = tmp_path / "raw"; folder.mkdir()
    path = folder / "synthetic.zst"
    data = zstandard.ZstdCompressor().compress(b"".join(rows)); path.write_bytes(data)
    return path, hashlib.sha256(data).hexdigest()


def test_sampler_first_ten_counts_hashes_and_immutable_source(tmp_path):
    rows = [raw(record(index=i)) for i in range(11)]
    broken = record(index=2); broken["payload"]["meta"]["err_is_none"] = None; rows[2] = raw(broken)
    unknown = record(ix(b"\x09"), index=4); rows[4] = raw(unknown)
    source, sha = make_source(tmp_path, rows); before = source.read_bytes(); out = tmp_path / "new"
    result = mod().sample_bounded_file(source, out, expected_sha256=sha)
    assert result["transactions_attempted"] == 10
    assert result["selected_record_indices"] == list(range(10)) and result["records_read"] == 10
    assert (result["traced_count"], result["rejected_count"]) == (9, 1)
    assert result["instruction_status_counts"] == {"accepted": 8, "rejected": 0, "unknown": 1}
    assert source.read_bytes() == before and result["source_sha256_after"] == sha
    assert json.loads((out / "receipt.json").read_text()) == result
    for name, digest in result["output_sha256"].items():
        assert hashlib.sha256((out / name).read_bytes()).hexdigest() == digest
    assert set(result["transform_sha256"]) == {"instruction_truth.py", "raw_adapter.py"}
    for name, digest in result["transform_sha256"].items():
        assert hashlib.sha256((Path(mod().__file__).parent / name).read_bytes()).hexdigest() == digest
    with pytest.raises(FileExistsError):
        mod().sample_bounded_file(source, out, expected_sha256=sha)


def test_sampler_prefix_bound_and_hash_mismatch(tmp_path):
    slot = b'{"record_type":"slot"}\n'
    source, sha = make_source(tmp_path, [slot]*100 + [raw(record())])
    out = tmp_path / "new"
    result = mod().sample_bounded_file(source, out, expected_sha256=sha)
    assert result["records_read"] == 100 and result["transactions_attempted"] == 0
    with pytest.raises(mod().InstructionTruthError, match="source_sha256_mismatch"):
        mod().sample_bounded_file(source, tmp_path / "bad", expected_sha256="f"*64)
    assert not (tmp_path / "bad").exists()


@pytest.mark.parametrize("limit", [True, 0, 11, 1.5])
def test_sampler_invalid_limits(tmp_path, limit):
    with pytest.raises(mod().InstructionTruthError, match="invalid_max_transactions"):
        mod().sample_bounded_file(tmp_path / "missing", tmp_path / "new", expected_sha256="a"*64, max_transactions=limit)
