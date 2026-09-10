"""Exact, development-only instruction layout classification, not flow/trade truth.

Local official Rust references and hashes are listed in the build receipt. Only
System Transfer (bincode u32 tag 2 + LE u64), SPL/2022 Transfer (3 + LE u64),
and TransferChecked (12 + LE u64 + u8) with exact base account counts are decoded.
Extra accounts/extensions/multisig semantics fail closed as unknown. Instruction
arguments are NOT net received amounts. A successful transaction cannot prove
individual CPI success (errors may be caught), CPI caller, token owner, beneficial
person, trade, finality or endpoint effects. No executed quantities are emitted.
"""
import base64
import binascii
from collections import Counter
import hashlib
import io
import json
from pathlib import Path
import re

import base58
import zstandard

from north_star import raw_adapter

SYSTEM = "11111111111111111111111111111111"
TOKEN = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
TOKEN22 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
MAX_RECORD_BYTES = 8 * 1024 * 1024


class InstructionTruthError(ValueError):
    """Stable structural rejection reason; never a repaired interpretation."""

    def __init__(self, reason):
        self.reason = reason
        super().__init__(reason)


def _require(condition, reason):
    if not condition:
        raise InstructionTruthError(reason)


def _pubkey(value, size=32):
    if not isinstance(value, str) or not 1 <= len(value) <= 2 * size:
        return False
    if re.fullmatch(r"[1-9A-HJ-NP-Za-km-z]+", value) is None:
        return False
    decoded = base58.b58decode(value)
    return len(decoded) == size and base58.b58encode(decoded).decode() == value


def _keys(keys):
    _require(isinstance(keys, list) and 1 <= len(keys) <= 256,
             "invalid_account_key_count")
    _require(all(isinstance(key, str) for key in keys), "invalid_account_keys")
    _require(len(keys) == len(set(keys)), "duplicate_account_key")


def _bytes(value, field, limit):
    reason = "invalid_" + field
    _require(isinstance(value, str) and len(value) <= 4 * ((limit + 2) // 3), reason)
    try:
        decoded = base64.b64decode(value, validate=True)
    except (ValueError, binascii.Error) as exc:
        raise InstructionTruthError(reason) from exc
    _require(len(decoded) <= limit and base64.b64encode(decoded).decode() == value, reason)
    return decoded


def _unknown(reason, program, indices):
    return dict(classification_status="unknown", reason=reason, program_id=program,
                account_indices=indices, instruction_kind=None, amount_raw=None,
                decimals_argument=None, source=None, destination=None, mint=None,
                authority=None, owner=None, beneficial_person=None, trade=None,
                net_received_raw=None, extension_semantics="unknown")


def decode_instruction(instruction, account_keys):
    """Classify a recorder compiled instruction; structural faults raise.

    Account indices arrive as bytes in canonical base64, not coercible JSON
    numbers. Program index must be an actual int/u8. All referenced identities
    must decode to exactly 32 bytes; malformed unused keys are handled by tracer
    quarantine. Supported-program checks use literal IDs, with no aliases.
    """
    _keys(account_keys)
    _require(isinstance(instruction, dict), "invalid_instruction")
    index = instruction.get("program_id_index")
    _require(type(index) is int and 0 <= index < len(account_keys) and index <= 255,
             "invalid_program_id_index")
    program = account_keys[index]
    _require(_pubkey(program), "invalid_program_id")
    indices = list(_bytes(instruction.get("accounts_b64"), "accounts_b64", 256))
    _require(all(i < len(account_keys) for i in indices), "invalid_account_index")
    _require(all(_pubkey(account_keys[i]) for i in indices), "invalid_instruction_account_key")
    data = _bytes(instruction.get("data_b64"), "data_b64", 65536)
    result = _unknown("unsupported_program", program, indices)
    if program not in (SYSTEM, TOKEN, TOKEN22):
        return result
    tag_size = 4 if program == SYSTEM else 1
    _require(len(data) >= tag_size, "invalid_instruction_data_length")
    tag = int.from_bytes(data[:tag_size], "little")
    layouts = ({2: (12, 2, "system_transfer")} if program == SYSTEM else
               {3: (9, 3, "token_transfer"), 12: (10, 4, "token_transfer_checked")})
    if tag not in layouts:
        result["reason"] = "unsupported_opcode"
        return result
    size, count, kind = layouts[tag]
    _require(len(data) == size, "invalid_instruction_data_length")
    _require(len(indices) >= count, "invalid_instruction_account_count")
    if len(indices) != count:
        result["reason"] = "unsupported_extra_accounts"
        return result

    def account(position):
        i = indices[position]
        return dict(account_index=i, account_key=account_keys[i])

    checked = kind == "token_transfer_checked"
    result.update(classification_status="accepted", reason=None, instruction_kind=kind,
                  amount_raw=int.from_bytes(data[tag_size:tag_size + 8], "little"),
                  amount_unit="lamports" if program == SYSTEM else "token_base_units",
                  amount_evidence="instruction_argument_only",
                  decimals_argument=data[9] if checked else None,
                  source=account(0), destination=account(2 if checked else 1),
                  mint=account(1) if checked else None,
                  authority=None if program == SYSTEM else account(3 if checked else 2),
                  authority_evidence=None if program == SYSTEM else "owner_or_delegate_or_multisig_unresolved")
    return result


def _list(value, name):
    _require(isinstance(value, list), "invalid_" + name)
    return value


def trace_transaction(raw_record, *, source_path, source_sha256):
    """Preserve every outer/CPI instruction with accepted/rejected/unknown status.

    A traced transaction can contain rejected instructions and quarantined keys;
    this is not whole-transaction acceptance. No dependency on balance matching:
    endpoint absence/creation/closure does not turn an argument into a movement.
    """
    _require(type(raw_record) is bytes, "raw_record_must_be_bytes")
    _require(len(raw_record) <= MAX_RECORD_BYTES, "raw_record_too_large")
    try:
        projection = raw_adapter.project_record(raw_record, source_path=source_path,
                                               source_sha256=source_sha256)
    except raw_adapter.RawProjectionError as exc:
        raise InstructionTruthError(exc.reason) from exc
    _require(projection["record_type"] == "transaction", "not_transaction")
    _require(all(_pubkey(s, 64) for s in projection["signatures"]), "invalid_signature")
    keys = projection["account_keys"]
    _keys(keys)
    payload = json.loads(raw_record)["payload"]
    meta = payload["meta"]
    succeeded = meta.get("err_is_none")
    _require(type(succeeded) is bool and "err_hex" in meta, "unknown_transaction_status")
    error = meta["err_hex"]
    _require((succeeded and error is None) or
             (not succeeded and isinstance(error, str) and
              re.fullmatch(r"(?:[0-9a-f]{2})+", error) is not None),
             "transaction_status_mismatch")
    outer = _list(payload["message"].get("instructions"), "instructions")
    _require(len(outer) <= 256, "too_many_outer_instructions")
    inner = _list(meta.get("inner_instructions"), "inner_instructions")
    groups = {}
    for group in inner:
        _require(isinstance(group, dict), "invalid_inner_group")
        parent = group.get("index")
        _require(type(parent) is int and 0 <= parent < len(outer), "invalid_inner_parent_index")
        _require(parent not in groups, "duplicate_inner_parent_index")
        groups[parent] = _list(group.get("instructions"), "inner_group_instructions")
    _require(len(outer) + sum(map(len, groups.values())) <= 4096, "too_many_instructions")
    traced = []

    def append(instruction, outer_index, inner_index=None):
        height = None
        try:
            if inner_index is not None:
                _require(isinstance(instruction, dict), "invalid_instruction")
                height = instruction.get("stack_height")
                _require(height is None or (type(height) is int and 2 <= height < 2**32),
                         "invalid_stack_height")
            result = decode_instruction(instruction, keys)
        except InstructionTruthError as exc:
            result = _unknown(exc.reason, None, None)
            result["classification_status"] = "rejected"
        execution = ("transaction_failed_no_committed_transfer" if not succeeded else
                     "unknown_cpi_outcome" if inner_index is not None else
                     "outer_instruction_succeeded")
        result.update(outer_instruction_index=outer_index, inner_instruction_index=inner_index,
                      stack_height=height, raw_instruction=instruction,
                      execution_status=execution, caller_identity=None, executed_transfer_raw=None)
        traced.append(result)

    for i, instruction in enumerate(outer):
        append(instruction, i)
        for j, instruction in enumerate(groups.get(i, [])):
            append(instruction, i, j)
    issues = [dict(account_index=i, account_key=key, reason="invalid_solana_pubkey")
              for i, key in enumerate(keys) if not _pubkey(key)]
    result = {field: projection[field] for field in (
        "event_id", "source_path", "source_sha256", "raw_record_sha256", "raw_record_hash_scope",
        "record_index", "slot", "transaction_index", "signature", "signatures", "account_keys",
        "static_account_keys", "loaded_writable_account_keys", "loaded_readonly_account_keys")}
    result.update(schema_version="instruction_truth_v1", scope="development_instruction_layout_only",
                  recv_unix_ms=projection["observed_at_unix_ms"], training_eligible=False,
                  source_admission="unadmitted", finality=None, instructions=traced,
                  transaction_status="succeeded" if succeeded else "failed", err_hex=error,
                  canonical_identity_status="quarantined" if issues else "format_valid_only",
                  identity_issues=issues, cpi_coverage="not_certified",
                  instruction_status_counts=_counts(traced))
    return result


def _counts(instructions):
    counts = Counter(x["classification_status"] for x in instructions)
    return {status: counts[status] for status in ("accepted", "rejected", "unknown")}


def _sha(path):
    with Path(path).open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def sample_bounded_file(source, output_dir, *, expected_sha256, max_transactions=10):
    """New-only artifact: first <=10 transactions inside exposed first100 lines.

    Source must be already authorized/exposed by the caller. Hashing all compressed
    bytes is not decoding/admitting the corpus. Completion receipt is written last.
    Original bytes are opened read-only and checked before/after. No sampler seeks
    beyond the fixed prefix to compensate for rejection or obtain nicer counts.
    """
    _require(type(max_transactions) is int and 1 <= max_transactions <= 10,
             "invalid_max_transactions")
    _require(isinstance(expected_sha256, str) and
             re.fullmatch(r"[0-9a-f]{64}", expected_sha256) is not None, "invalid_expected_sha256")
    source, output_dir = Path(source).resolve(), Path(output_dir).resolve()
    _require(not output_dir.is_relative_to(source.parent) and not source.is_relative_to(output_dir),
             "output_overlaps_source_directory")
    if output_dir.exists():
        raise FileExistsError(output_dir)
    transforms = {p.name: _sha(p) for p in (Path(__file__), Path(raw_adapter.__file__))}
    before = source.stat()
    source_sha = _sha(source)
    _require(source_sha == expected_sha256, "source_sha256_mismatch")
    accepted, rejected, selected = [], [], []
    records_read = 0
    with source.open("rb") as compressed:
        with zstandard.ZstdDecompressor().stream_reader(compressed) as stream:
            with io.BufferedReader(stream) as reader:
                for line_number in range(1, 101):
                    data = reader.readline(MAX_RECORD_BYTES + 1)
                    if not data:
                        break
                    records_read = line_number
                    _require(len(data) <= MAX_RECORD_BYTES, "raw_record_too_large")
                    try:
                        # Duplicate envelope kinds cannot silently evade selection.
                        row = json.loads(data, object_pairs_hook=raw_adapter._object)
                    except (ValueError, UnicodeDecodeError, RecursionError) as exc:
                        raise InstructionTruthError("invalid_prefix_json") from exc
                    _require(isinstance(row, dict), "invalid_prefix_envelope")
                    if row.get("record_type") != "transaction":
                        continue
                    selected.append(row.get("record_index"))
                    try:
                        result = trace_transaction(data, source_path=str(source), source_sha256=source_sha)
                        result["source_line_number"] = line_number
                        accepted.append(result)
                    except InstructionTruthError as exc:
                        rejected.append(dict(record_index=row.get("record_index"), source_line_number=line_number,
                                             raw_record_sha256=hashlib.sha256(data).hexdigest(),
                                             source_path=str(source), source_sha256=source_sha,
                                             reason=exc.reason, training_eligible=False))
                    if len(selected) == max_transactions:
                        break
    after_sha = _sha(source)
    after = source.stat()
    _require(after_sha == source_sha and before.st_size == after.st_size and
             before.st_mtime_ns == after.st_mtime_ns, "source_changed_during_sample")
    _require(transforms == {p.name: _sha(p) for p in (Path(__file__), Path(raw_adapter.__file__))},
             "transform_changed_during_sample")
    instructions = [instruction for tx in accepted for instruction in tx["instructions"]]
    counts = _counts(instructions)
    _require(len(accepted) + len(rejected) == len(selected) and sum(counts.values()) == len(instructions),
             "sample_count_mismatch")
    receipt = dict(schema_version="instruction_truth_sample_receipt_v1", source_path=str(source),
                   source_sha256=source_sha, source_sha256_after=after_sha, source_unchanged=True,
                   transform_sha256=transforms, max_transactions=max_transactions, records_read=records_read,
                   selection="first_transaction_attempts_within_first100_exposed_raw_lines",
                   transactions_attempted=len(selected), selected_record_indices=selected,
                   traced_count=len(accepted), rejected_count=len(rejected),
                   transaction_rejection_reasons=dict(Counter(x["reason"] for x in rejected)),
                   instruction_count=len(instructions), instruction_status_counts=counts,
                   instruction_reasons=dict(Counter(x["reason"] for x in instructions if x["reason"])),
                   accepted_kind_counts=dict(Counter(x["instruction_kind"] for x in instructions
                                                   if x["classification_status"] == "accepted")),
                   execution_status_counts=dict(Counter(x["execution_status"] for x in instructions)),
                   transaction_status_counts=dict(Counter(x["transaction_status"] for x in accepted)),
                   canonical_identity_quarantined_count=sum(x["canonical_identity_status"] == "quarantined"
                                                           for x in accepted),
                   executed_transfer_quantity_count=0, full_source_decoded=False,
                   training_eligible=False, source_admission="unadmitted", output_sha256={})
    output_dir.mkdir(parents=True, exist_ok=False)
    for name, rows in (("traced.ndjson", accepted), ("rejected.ndjson", rejected)):
        encoded = b"".join((json.dumps(x, sort_keys=True, separators=(",", ":")) + "\n").encode() for x in rows)
        with (output_dir / name).open("xb") as output:
            output.write(encoded)
        receipt["output_sha256"][name] = hashlib.sha256(encoded).hexdigest()
    with (output_dir / "receipt.json").open("x", encoding="utf-8") as output:
        json.dump(receipt, output, sort_keys=True, indent=2)
        output.write("\n")
    return receipt
