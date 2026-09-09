"""Exact recorder-v1 endpoint balance reconciliation, NOT venue execution truth.

Only accounts present in transaction metadata are observed. No instruction-level
transfers, temporary accounts, beneficial owners, finality, fees by component,
cost basis or trade sides can be inferred from this projection. Integer token
amount strings (not UI amounts) are authoritative for the arithmetic.

Missing token endpoints always fail closed, including possible creation/closure:
zero native lamports alone is not an explicit token amount/owner observation.
Malformed non-token account keys remain literal with identity quarantine; they
are never repaired, looked up, or admitted as canonical Solana identities.
"""
from collections import Counter
import hashlib
import io
import json
from pathlib import Path
import re

import zstandard

import base58

from north_star.raw_adapter import RawProjectionError, project_record


class TransactionTruthError(ValueError):
    """Stable rejection reason; no partially reconciled transaction is returned."""

    def __init__(self, reason):
        self.reason = reason
        super().__init__(reason)


def _require(condition, reason):
    if not condition:
        raise TransactionTruthError(reason)


def _u64(value, name):
    _require(type(value) is int and 0 <= value < 2**64, "invalid_" + name)
    return value


def _list(value, name):
    _require(isinstance(value, list), "invalid_" + name)
    return value


def _dict(value, name):
    _require(isinstance(value, dict), "invalid_" + name)
    return value


def _base58_size(value, size):
    # Never trim/repair identifiers. Bound input before base58 conversion.
    if not isinstance(value, str) or not 1 <= len(value) <= 2 * size:
        return False
    if re.fullmatch(r"[1-9A-HJ-NP-Za-km-z]+", value) is None:
        return False
    try:
        raw = base58.b58decode(value)
        return len(raw) == size and base58.b58encode(raw).decode("ascii") == value
    except ValueError:
        return False


def _tokens(rows, keys, name):
    parsed = {}
    for row in _list(rows, name):
        _dict(row, "token_balance")
        index = row.get("account_index")
        _require(type(index) is int and 0 <= index < len(keys),
                 "invalid_token_account_index")
        _require(index not in parsed, "duplicate_token_account_index")
        _require(_base58_size(keys[index], 32), "invalid_token_account_key")
        for field in ("mint", "owner", "program_id"):
            _require(_base58_size(row.get(field), 32), "invalid_token_" + field)
        ui = _dict(row.get("ui_token_amount"), "ui_token_amount")
        decimals = ui.get("decimals")
        _require(type(decimals) is int and 0 <= decimals <= 255,
                 "invalid_token_decimals")
        text = ui.get("amount")
        _require(isinstance(text, str) and len(text) <= 20 and
                 re.fullmatch(r"0|[1-9][0-9]*", text) is not None,
                 "invalid_token_amount")
        amount = _u64(int(text), "token_amount")
        parsed[index] = {"mint": row["mint"], "owner": row["owner"],
                         "program_id": row["program_id"], "decimals": decimals,
                         "amount_raw": amount, "amount_source_text": text}
    return parsed


def reconcile_transaction(raw_record, *, source_path, source_sha256):
    """Reconcile exact NDJSON bytes using caller-supplied source provenance.

    Returns development-only movements including zero deltas. A returned record
    is not source admission or an executed fill. `canonical_identity_status` is
    independent of endpoint arithmetic: malformed non-token keys quarantine the
    whole identity while retaining index-addressable arithmetic for inspection.
    The file sampler verifies the compressed SHA; this pure function cannot.
    Native deltas already include the observed total transaction fee. The fee is
    exposed separately for reconciliation, NOT subtracted a second time. Owner
    is the token-balance metadata authority, not a proven person or trader.
    """
    try:
        projection = project_record(raw_record, source_path=source_path,
                                    source_sha256=source_sha256)
    except RawProjectionError as exc:
        raise TransactionTruthError(exc.reason) from exc
    _require(projection["record_type"] == "transaction", "not_transaction")
    # The adapter has validated JSON duplicate keys, scalar types and bounds.
    payload = json.loads(raw_record)["payload"]
    meta = payload["meta"]
    for signature in projection["signatures"]:
        _require(_base58_size(signature, 64), "invalid_signature")
    keys = projection["account_keys"]
    _require(bool(keys) and len(keys) <= 256, "invalid_account_key_count")
    _require(len(keys) == len(set(keys)), "duplicate_account_key")
    succeeded = meta.get("err_is_none")
    _require(type(succeeded) is bool, "unknown_transaction_status")
    _require("err_hex" in meta, "unknown_transaction_status")
    error = meta["err_hex"]
    _require((succeeded and error is None) or
             (not succeeded and isinstance(error, str) and
              re.fullmatch(r"(?:[0-9a-f]{2})+", error) is not None),
             "transaction_status_mismatch")
    fee = _u64(meta.get("fee"), "fee")
    pre = _list(meta.get("pre_balances"), "pre_balances")
    post = _list(meta.get("post_balances"), "post_balances")
    _require(len(pre) == len(post) == len(keys), "native_balance_length_mismatch")
    for value in pre + post:
        _u64(value, "native_balance")
    pre_token = _tokens(meta.get("pre_token_balances"), keys, "pre_token_balances")
    post_token = _tokens(meta.get("post_token_balances"), keys, "post_token_balances")
    _require(pre_token.keys() == post_token.keys(), "missing_token_counterpart")
    movements = []
    mint_metadata = {}
    program_kinds = {
        "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA": "spl_token",
        "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb": "token_2022",
    }
    for index in sorted(pre_token):
        before, after = pre_token[index], post_token[index]
        for field, reason in (("mint", "token_mint_changed"),
                              ("owner", "token_owner_changed"),
                              ("program_id", "token_program_changed"),
                              ("decimals", "token_decimals_mismatch")):
            _require(before[field] == after[field], reason)
        previous = mint_metadata.setdefault(before["mint"], before)
        _require(previous["decimals"] == before["decimals"], "mint_decimals_mismatch")
        _require(previous["program_id"] == before["program_id"], "mint_program_mismatch")
        delta = after["amount_raw"] - before["amount_raw"]
        _require(succeeded or delta == 0, "failed_transaction_token_change")
        movements.append({
            "account_index": index, "account_key": keys[index],
            "mint": before["mint"], "owner": before["owner"],
            "owner_evidence": "matching_pre_post_token_balance_metadata",
            "program_id": before["program_id"],
            "program_kind": program_kinds.get(before["program_id"], "unknown"),
            "decimals": before["decimals"],
            "pre_amount_raw": before["amount_raw"],
            "post_amount_raw": after["amount_raw"], "delta_raw": delta,
            "pre_amount_source_text": before["amount_source_text"],
            "post_amount_source_text": after["amount_source_text"],
            "endpoint_status": "both_observed",
        })
    residual = sum(post) - sum(pre) + fee
    _require(residual == 0, "native_balance_fee_mismatch")
    # Only fee-only failed endpoints are supported. Conservation alone also
    # admits arbitrary transfers. Exceptional nonfee effects (e.g. nonce
    # handling) need explicit support, not a claim that they are impossible.
    _require(succeeded or all(
        after - before == (-fee if index == 0 else 0)
        for index, (before, after) in enumerate(zip(pre, post))
    ), "unsupported_failed_transaction_native_change")
    sources = (["static"] * len(projection["static_account_keys"]) +
               ["loaded_writable"] * len(projection["loaded_writable_account_keys"]) +
               ["loaded_readonly"] * len(projection["loaded_readonly_account_keys"]))
    # Loaded readonly status is explicit; static writability is not projected.
    _require(all(pre[i] == post[i] for i, source in enumerate(sources)
                 if source == "loaded_readonly"), "loaded_readonly_native_change")
    _require(all(movement["delta_raw"] == 0 for movement in movements
                 if sources[movement["account_index"]] == "loaded_readonly"),
             "loaded_readonly_token_change")
    accounts = [{"account_index": i, "account_key": key,
                 "key_source": sources[i], "valid_solana_pubkey": _base58_size(key, 32)}
                for i, key in enumerate(keys)]
    issues = [{"account_index": a["account_index"], "reason": "invalid_solana_pubkey"}
              for a in accounts if not a["valid_solana_pubkey"]]
    return {
        "schema_version": "transaction_balance_reconciliation_v1",
        "scope": "development_endpoint_balances_only", "training_eligible": False,
        "source_admission": "unadmitted", "event_id": projection["event_id"],
        "source_path": source_path, "source_sha256": source_sha256,
        "raw_record_sha256": projection["raw_record_sha256"],
        "raw_record_hash_scope": projection["raw_record_hash_scope"],
        "record_index": projection["record_index"], "slot": projection["slot"],
        "recv_unix_ms": projection["observed_at_unix_ms"],
        "transaction_index": projection["transaction_index"],
        "signature": projection["signature"], "signatures": projection["signatures"],
        "transaction_status": "succeeded" if succeeded else "failed", "err_hex": error,
        "finality": None, "accounts": accounts, "identity_issues": issues,
        "canonical_identity_status": "quarantined" if issues else "format_valid_only",
        "token_movements": movements,
        "native_movements": [{"account_index": i, "account_key": key,
                              "pre_lamports": pre[i], "post_lamports": post[i],
                              "delta_lamports": post[i] - pre[i]}
                             for i, key in enumerate(keys)],
        "fee_lamports": fee, "fee_already_in_native_deltas": True,
        "native_reconciliation_residual_lamports": residual,
        "side": None, "trader": None, "quote_asset": None,
        "executed_venue_fill": None, "cost_breakdown": None,
    }


def sample_bounded_file(source, output_dir, *, expected_sha256, max_transactions=10):
    """First <=10 transaction attempts within first100 lines; never seek deeper.

    Fixed prefix restriction is a guard, not permission to expose a new source.
    Caller must supply an already-exposed development file and its pinned hash.
    Files are new-only; receipt is written last and is the completion marker.
    Reconciled counts mean arithmetic accepted, not canonical identity admission.
    """
    _require(type(max_transactions) is int and 1 <= max_transactions <= 10,
             "invalid_max_transactions")
    _require(isinstance(expected_sha256, str) and
             re.fullmatch(r"[0-9a-f]{64}", expected_sha256) is not None,
             "invalid_expected_sha256")
    source, output_dir = Path(source), Path(output_dir)
    if output_dir.exists():
        raise FileExistsError(output_dir)
    accepted, rejected, selected = [], [], []
    records_read = 0
    with source.open("rb") as compressed:
        source_hash = hashlib.file_digest(compressed, "sha256").hexdigest()
        _require(source_hash == expected_sha256, "source_sha256_mismatch")
        compressed.seek(0)
        with zstandard.ZstdDecompressor().stream_reader(compressed, closefd=False) as stream:
            with io.BufferedReader(stream) as reader:
                for line_number in range(1, 101):
                    raw = reader.readline(8 * 1024 * 1024 + 1)
                    if not raw:
                        break
                    _require(len(raw) <= 8 * 1024 * 1024, "raw_record_too_large")
                    records_read = line_number
                    try:
                        row = json.loads(raw)
                    except (ValueError, UnicodeDecodeError) as exc:
                        raise TransactionTruthError("invalid_prefix_json") from exc
                    _require(isinstance(row, dict), "invalid_prefix_envelope")
                    if row.get("record_type") != "transaction":
                        continue
                    selected.append(row.get("record_index"))
                    try:
                        accepted.append(reconcile_transaction(
                            raw, source_path=str(source), source_sha256=source_hash))
                    except TransactionTruthError as exc:
                        payload = row.get("payload")
                        rejected.append({
                            "record_index": row.get("record_index"),
                            "source_line_number": line_number,
                            "signature": payload.get("signature_b58") if isinstance(payload, dict) else None,
                            "raw_record_sha256": hashlib.sha256(raw).hexdigest(),
                            "source_path": str(source), "source_sha256": source_hash,
                            "reason": exc.reason, "training_eligible": False})
                    if len(selected) == max_transactions:
                        break
        compressed.seek(0)
        _require(hashlib.file_digest(compressed, "sha256").hexdigest() == source_hash,
                 "source_changed_during_sample")
    _require(len(accepted) + len(rejected) == len(selected), "sample_count_mismatch")
    receipt = {
        "schema_version": "transaction_truth_sample_receipt_v1",
        "source_path": str(source), "source_sha256": source_hash,
        "selection": "first_transaction_attempts_within_first100_exposed_raw_lines",
        "max_transactions": max_transactions, "records_read": records_read,
        "transactions_attempted": len(selected), "selected_record_indices": selected,
        "reconciled_count": len(accepted), "rejected_count": len(rejected),
        "rejection_reasons": dict(sorted(Counter(x["reason"] for x in rejected).items())),
        "canonical_identity_quarantined_count": sum(
            x["canonical_identity_status"] == "quarantined" for x in accepted),
        "transaction_status_counts": dict(sorted(Counter(
            x["transaction_status"] for x in accepted).items())),
        "token_movement_rows": sum(len(x["token_movements"]) for x in accepted),
        "native_movement_rows": sum(len(x["native_movements"]) for x in accepted),
        "training_eligible": False, "source_admission": "unadmitted",
        "output_sha256": {},
    }
    output_dir.mkdir(parents=True, exist_ok=False)
    for filename, rows in (("reconciled.ndjson", accepted), ("rejected.ndjson", rejected)):
        data = b"".join((json.dumps(x, sort_keys=True, separators=(",", ":")) + "\n").encode()
                        for x in rows)
        with (output_dir / filename).open("xb") as output:
            output.write(data)
        receipt["output_sha256"][filename] = hashlib.sha256(data).hexdigest()
    with (output_dir / "receipt.json").open("x", encoding="utf-8") as output:
        json.dump(receipt, output, sort_keys=True, indent=2)
        output.write("\n")
    return receipt
