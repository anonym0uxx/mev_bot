"""Development decoder tests; fixtures are not on-chain execution evidence."""
import copy
import hashlib
import importlib
import json

import base58
import pytest


def api():
    # A missing implementation must be an assertion failure in the first RED run.
    spec = importlib.util.find_spec("north_star.transaction_truth")
    assert spec is not None, "transaction reconciliation implementation is missing"
    return importlib.import_module("north_star.transaction_truth")


def key(n):
    return base58.b58encode(bytes([n]) * 32).decode()


SIG = base58.b58encode(bytes([9]) * 64).decode()
TOKEN = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
TOKEN2022 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
CONTEXT = {"source_path": "fixture.ndjson", "source_sha256": "a" * 64}


def balance(index, amount, *, owner=None, program=TOKEN, decimals=6):
    return {"account_index": index, "mint": key(20), "owner": owner or key(21),
            "program_id": program, "ui_token_amount": {
                "amount": str(amount), "decimals": decimals,
                "ui_amount": 999.5, "ui_amount_string": "deliberately not used"}}


def fixture():
    return {"record_type": "transaction", "slot": 10, "recv_unix_ms": 20,
            "record_index": 3, "payload": {
                "signature_b58": SIG, "signatures_b58": [SIG], "raw_hash": "x",
                "tx_index": 2, "message": {"account_keys_b58": [key(1), key(2)]},
                "meta": {"loaded_writable_addresses_b58": [key(3)],
                         "loaded_readonly_addresses_b58": [key(4)],
                         "err_is_none": True, "err_hex": None, "fee": 5,
                         "pre_balances": [100, 20, 30, 40],
                         "post_balances": [85, 30, 30, 40],
                         "pre_token_balances": [balance(2, 2**64 - 1), balance(1, 0)],
                         "post_token_balances": [balance(1, 1), balance(2, 2**64 - 2)]}}}


def decode(row=None):
    raw = json.dumps(row if row is not None else fixture()).encode() + b"\n"
    return api().reconcile_transaction(raw, **CONTEXT)


def reject(row, reason):
    module = api()
    with pytest.raises(module.TransactionTruthError) as err:
        decode(row)
    assert err.value.reason == reason


def test_exact_loaded_key_index_reconciliation_not_array_zip():
    out = decode()
    assert [x["account_key"] for x in out["accounts"]] == [key(i) for i in range(1, 5)]
    assert [x["key_source"] for x in out["accounts"]] == ["static", "static", "loaded_writable", "loaded_readonly"]
    assert [(x["account_index"], x["delta_raw"]) for x in out["token_movements"]] == [(1, 1), (2, -1)]
    token = out["token_movements"][1]
    assert token["pre_amount_raw"] == 2**64 - 1
    assert token["post_amount_raw"] == 2**64 - 2
    assert token["owner"] == key(21) != out["accounts"][0]["account_key"]
    assert token["account_key"] == key(3)
    assert token["decimals"] == 6
    assert out["native_movements"][0]["delta_lamports"] == -15
    assert out["fee_lamports"] == 5
    assert out["native_reconciliation_residual_lamports"] == 0
    assert out["fee_already_in_native_deltas"] is True
    assert out["signature"] == SIG and out["transaction_status"] == "succeeded"
    assert out["training_eligible"] is False
    assert out["executed_venue_fill"] is None
    assert out["trader"] is None and out["side"] is None
    assert out["source_sha256"] == CONTEXT["source_sha256"]
    assert out["raw_record_sha256"] == hashlib.sha256(json.dumps(fixture()).encode() + b"\n").hexdigest()


@pytest.mark.parametrize("field,value,reason", [
    ("pre_balances", [100], "native_balance_length_mismatch"),
    ("post_balances", None, "invalid_post_balances"),
    ("fee", -1, "invalid_fee"), ("fee", True, "invalid_fee"),
    ("fee", 2**64, "json_integer_out_of_range"), ("fee", 1.5, "invalid_fee"),
    ("err_is_none", None, "unknown_transaction_status"),
    ("err_hex", "aabb", "transaction_status_mismatch"),
    ("pre_token_balances", None, "invalid_pre_token_balances"),
])
def test_bad_metadata_fails_closed(field, value, reason):
    row = fixture()
    row["payload"]["meta"][field] = value
    reject(row, reason)


@pytest.mark.parametrize("field,value,reason", [
    ("account_index", 4, "invalid_token_account_index"),
    ("account_index", -1, "invalid_token_account_index"),
    ("account_index", True, "invalid_token_account_index"),
    ("owner", "", "invalid_token_owner"),
    ("owner", None, "invalid_token_owner"),
    ("owner", key(22), "token_owner_changed"),
    ("mint", key(23), "token_mint_changed"),
    ("program_id", TOKEN2022, "token_program_changed"),
    ("ui_token_amount", None, "invalid_ui_token_amount"),
])
def test_token_identity_and_index_rejections(field, value, reason):
    row = fixture()
    row["payload"]["meta"]["post_token_balances"][0][field] = value
    reject(row, reason)


@pytest.mark.parametrize("field,value,reason", [
    ("decimals", 9, "token_decimals_mismatch"),
    ("decimals", 256, "invalid_token_decimals"),
    ("decimals", True, "invalid_token_decimals"),
    ("amount", "18446744073709551616", "invalid_token_amount"),
    ("amount", "-1", "invalid_token_amount"),
    ("amount", "1.0", "invalid_token_amount"),
    ("amount", 1, "invalid_token_amount"),
    ("amount", "01", "invalid_token_amount"),
])
def test_exact_token_quantity_validation(field, value, reason):
    row = fixture()
    row["payload"]["meta"]["post_token_balances"][0]["ui_token_amount"][field] = value
    reject(row, reason)


@pytest.mark.parametrize("missing_side", ["pre", "post"])
@pytest.mark.parametrize("native_zero", [False, True])
def test_missing_counterpart_is_not_silently_zero_even_with_zero_lamports(missing_side, native_zero):
    row = fixture()
    meta = row["payload"]["meta"]
    meta[missing_side + "_token_balances"] = []
    if native_zero:
        meta["pre_balances"][1] = meta["post_balances"][1] = 0
        meta["post_balances"][0] += 10
    reject(row, "missing_token_counterpart")


def test_duplicate_index_is_ambiguous_even_same_owner():
    row = fixture()
    meta = row["payload"]["meta"]
    meta["pre_token_balances"].append(copy.deepcopy(meta["pre_token_balances"][0]))
    reject(row, "duplicate_token_account_index")


def test_same_mint_across_accounts_must_agree_on_decimals():
    row = fixture()
    for side in ("pre", "post"):
        for tb in row["payload"]["meta"][side + "_token_balances"]:
            if tb["account_index"] == 1:
                tb["ui_token_amount"]["decimals"] = 9
    reject(row, "mint_decimals_mismatch")


def test_failed_transaction_preserves_fee_and_zero_token_movements():
    row = fixture()
    meta = row["payload"]["meta"]
    meta.update(err_is_none=False, err_hex="abcd", post_balances=[95, 20, 30, 40],
                post_token_balances=copy.deepcopy(meta["pre_token_balances"]))
    out = decode(row)
    assert out["transaction_status"] == "failed" and out["err_hex"] == "abcd"
    assert out["fee_lamports"] == 5
    assert all(x["delta_raw"] == 0 for x in out["token_movements"])


@pytest.mark.parametrize("post_balances", [
    [85, 30, 30, 40],  # payer transfers ten lamports as well as paying the fee
    [95, 10, 40, 40],  # correct payer fee, but nonpayer transfer to loaded key
    [100, 15, 30, 40],  # fee charged to the wrong index
    [105, 10, 30, 40],  # payer credit balanced by a larger nonpayer debit
])
def test_failed_transaction_rejects_nonfee_native_endpoints(post_balances):
    row = fixture()
    meta = row["payload"]["meta"]
    meta.update(err_is_none=False, err_hex="abcd", post_balances=post_balances,
                post_token_balances=copy.deepcopy(meta["pre_token_balances"]))
    # A zero aggregate residual does not establish supported failed-tx state.
    assert sum(post_balances) - sum(meta["pre_balances"]) + meta["fee"] == 0
    reject(row, "unsupported_failed_transaction_native_change")


@pytest.mark.parametrize("delta", [-1, 1])
@pytest.mark.parametrize("asset", ["native", "token"])
def test_loaded_readonly_endpoint_changes_reject_even_when_balanced(asset, delta):
    row = fixture()
    meta = row["payload"]["meta"]
    if asset == "native":
        meta["post_balances"][3] += delta
        meta["post_balances"][2] -= delta
        assert sum(meta["post_balances"]) - sum(meta["pre_balances"]) + meta["fee"] == 0
    else:
        meta["pre_token_balances"] = [balance(3, 10), balance(2, 10)]
        meta["post_token_balances"] = [balance(2, 10 - delta), balance(3, 10 + delta)]
    reject(row, "loaded_readonly_" + asset + "_change")


@pytest.mark.parametrize("succeeded", [False, True])
def test_unchanged_loaded_readonly_token_endpoint_is_supported(succeeded):
    row = fixture()
    meta = row["payload"]["meta"]
    meta["pre_token_balances"].append(balance(3, 10))
    meta["post_token_balances"].append(balance(3, 10))
    if not succeeded:
        meta.update(err_is_none=False, err_hex="abcd", post_balances=[95, 20, 30, 40],
                    post_token_balances=copy.deepcopy(meta["pre_token_balances"]))
    out = decode(row)
    assert out["native_movements"][3]["delta_lamports"] == 0
    assert next(x for x in out["token_movements"] if x["account_index"] == 3)["delta_raw"] == 0
    # Static keys have no writability header here; do not infer read-only status.
    assert out["native_movements"][1]["delta_lamports"] == (10 if succeeded else 0)


def test_failed_transaction_cannot_claim_changed_tokens():
    row = fixture()
    row["payload"]["meta"].update(err_is_none=False, err_hex="abcd")
    reject(row, "failed_transaction_token_change")


@pytest.mark.parametrize("program,kind", [(TOKEN, "spl_token"), (TOKEN2022, "token_2022"), (key(25), "unknown")])
def test_program_preserved_without_transfer_fee_or_quote_assumptions(program, kind):
    row = fixture()
    for side in ("pre", "post"):
        for tb in row["payload"]["meta"][side + "_token_balances"]:
            tb["program_id"] = program
    out = decode(row)
    assert all(x["program_kind"] == kind for x in out["token_movements"])
    assert out["cost_breakdown"] is None
    assert out["quote_asset"] is None


def test_native_imbalance_rejected_without_invented_fee():
    row = fixture()
    row["payload"]["meta"]["post_balances"][0] += 1
    reject(row, "native_balance_fee_mismatch")


def test_malformed_recorder_key_preserved_and_identity_quarantined_not_repaired():
    row = fixture()
    bad = "1" * 33  # recorder all-zero encoder emits one surplus leading 1
    row["payload"]["message"]["account_keys_b58"][0] = bad
    out = decode(row)
    assert out["accounts"][0]["account_key"] == bad
    assert out["accounts"][0]["valid_solana_pubkey"] is False
    assert out["canonical_identity_status"] == "quarantined"
    assert out["identity_issues"] == [{"account_index": 0, "reason": "invalid_solana_pubkey"}]
    assert out["native_movements"][0]["account_key"] == bad


def test_malformed_token_account_key_cannot_support_owner_reconciliation():
    row = fixture()
    row["payload"]["message"]["account_keys_b58"][1] = "1" * 33
    reject(row, "invalid_token_account_key")


def test_signature_not_truncated_or_repaired():
    row = fixture()
    row["payload"].update(signature_b58=SIG[:32], signatures_b58=[SIG[:32]])
    reject(row, "invalid_signature")


def test_duplicate_account_keys_rejected():
    row = fixture()
    row["payload"]["meta"]["loaded_readonly_addresses_b58"] = [key(1)]
    reject(row, "duplicate_account_key")


def test_token_balance_on_loaded_readonly_key_is_indexed_not_discarded():
    row = fixture()
    for side in ("pre", "post"):
        row["payload"]["meta"][side + "_token_balances"].append(balance(3, 77))
    out = decode(row)
    assert out["token_movements"][-1]["account_key"] == key(4)
    assert out["token_movements"][-1]["delta_raw"] == 0


def test_bounded_sampler_hash_counts_roundtrip_no_overwrite(tmp_path):
    import zstandard
    module = api()
    assert hasattr(module, "sample_bounded_file"), "bounded real-source sampler missing"
    rows = []
    for i in range(101):
        row = fixture()
        row["record_index"] = i
        if i == 1:
            row["payload"]["meta"]["post_token_balances"] = []
        rows.append(json.dumps(row).encode() + b"\n")
    source = tmp_path / "source.zst"
    source.write_bytes(zstandard.ZstdCompressor().compress(b"".join(rows)))
    sha = hashlib.sha256(source.read_bytes()).hexdigest()
    output = tmp_path / "new_output"
    receipt = module.sample_bounded_file(source, output, expected_sha256=sha)
    assert receipt["transactions_attempted"] == 10
    assert receipt["records_read"] == 10
    assert receipt["reconciled_count"] == 9 and receipt["rejected_count"] == 1
    assert receipt["rejection_reasons"] == {"missing_token_counterpart": 1}
    assert receipt["source_sha256"] == sha
    assert receipt["selected_record_indices"] == list(range(10))
    outputs = [json.loads(x) for x in (output / "reconciled.ndjson").read_bytes().splitlines()]
    assert len(outputs) == 9
    assert receipt["output_sha256"]["reconciled.ndjson"] == hashlib.sha256((output / "reconciled.ndjson").read_bytes()).hexdigest()
    assert json.loads((output / "receipt.json").read_text()) == receipt
    with pytest.raises(FileExistsError):
        module.sample_bounded_file(source, output, expected_sha256=sha)
    with pytest.raises(module.TransactionTruthError) as err:
        module.sample_bounded_file(source, tmp_path / "wrong_hash", expected_sha256="b" * 64)
    assert err.value.reason == "source_sha256_mismatch"
    assert not (tmp_path / "wrong_hash").exists()


def test_sampler_does_not_look_past_first100(tmp_path):
    import zstandard
    module = api()
    assert hasattr(module, "sample_bounded_file"), "bounded real-source sampler missing"
    source = tmp_path / "source.zst"
    source.write_bytes(zstandard.ZstdCompressor().compress(
        b'{"record_type":"slot"}\n' * 100 + json.dumps(fixture()).encode() + b"\n"))
    sha = hashlib.sha256(source.read_bytes()).hexdigest()
    receipt = module.sample_bounded_file(source, tmp_path / "output", expected_sha256=sha)
    assert receipt["records_read"] == 100
    assert receipt["transactions_attempted"] == 0


@pytest.mark.parametrize("limit", [0, 11, True])
def test_sampler_refuses_out_of_scope_transaction_count(tmp_path, limit):
    module = api()
    assert hasattr(module, "sample_bounded_file"), "bounded real-source sampler missing"
    with pytest.raises(module.TransactionTruthError) as err:
        module.sample_bounded_file(tmp_path / "absent", tmp_path / "output",
                                   expected_sha256="a" * 64, max_transactions=limit)
    assert err.value.reason == "invalid_max_transactions"


def test_unknown_record_kind_and_duplicate_json_fail_closed():
    module = api()
    row = fixture()
    row["record_type"] = "account"
    with pytest.raises(module.TransactionTruthError):
        decode(row)
    with pytest.raises(module.TransactionTruthError) as err:
        module.reconcile_transaction(b'{"record_type":"transaction","record_type":"transaction"}', **CONTEXT)
    assert err.value.reason == "duplicate_json_key"
