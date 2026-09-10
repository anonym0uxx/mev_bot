"""Synthetic native-cash fixtures only; no source/admission/economic evidence."""
import importlib.util
from dataclasses import FrozenInstanceError, replace
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


def cash():
    path = ROOT / "src/north_star/cash_ledger.py"
    assert path.exists(), "native cash reference ledger not implemented"
    name = "north_star_cash_ledger_test"
    if name not in sys.modules:
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
    return sys.modules[name]


def test_known_opening_is_actual_wallet_cash_not_a_bankroll_or_flow():
    m = cash()
    ledger = m.CashLedger()
    ledger.open_wallet("W", 100, evidence="synthetic-opening")
    ledger.open_wallet("B", 7, evidence="synthetic-other-opening")
    state = ledger.wallet("W")
    assert state.cash_lamports == 100
    assert state.opening_lamports == 100
    assert state.external_flow_lamports == 0
    assert state.trade_consideration_lamports == 0
    assert state.fee_expense_lamports == 0
    assert state.nontrading_income_lamports == 0
    assert state.recoverable_rent_lamports == 0
    assert ledger.wallet("B").cash_lamports == 7
    assert state.economics_eligible is False
    assert state.training_eligible is False


def test_unknown_opening_retains_reason_without_zero_or_bankroll():
    ledger = cash().CashLedger()
    ledger.open_wallet("W", None, evidence="synthetic-gap", unknown_reason="missing history")
    state = ledger.wallet("W")
    assert state.opening_lamports is None
    assert state.cash_lamports is None
    assert state.opening_evidence == "synthetic-gap"
    assert state.unresolved_reasons == ("missing history",)
    assert state.reconciled is False


@pytest.mark.parametrize("value", [True, False, 1.0, "1", -1, 2**64])
def test_opening_native_units_reject_malformed_atomically(value):
    ledger = cash().CashLedger()
    with pytest.raises(ValueError):
        ledger.open_wallet("W", value, evidence="synthetic")
    ledger.open_wallet("W", 0, evidence="synthetic")
    assert ledger.wallet("W").cash_lamports == 0


@pytest.mark.parametrize("wallet,amount,evidence,reason", [
    ("", 1, "synthetic", None), (True, 1, "synthetic", None),
    ("W", 1, " ", None), ("W", None, "synthetic", None),
    ("W", None, "synthetic", ""), ("W", 1, "synthetic", "unknown"),
])
def test_opening_requires_identity_evidence_and_explicit_unknown_reason(wallet, amount, evidence, reason):
    with pytest.raises(ValueError):
        cash().CashLedger().open_wallet(wallet, amount, evidence=evidence, unknown_reason=reason)


def test_opening_cannot_reset_existing_wallet():
    ledger = cash().CashLedger()
    ledger.open_wallet("W", 0, evidence="synthetic")
    with pytest.raises(ValueError):
        ledger.open_wallet("W", 100, evidence="synthetic")
    assert ledger.wallet("W").cash_lamports == 0


def opened(amount=100):
    ledger = cash().CashLedger()
    ledger.open_wallet("W", amount, evidence="synthetic")
    return ledger


def movement(kind, amount, action_id="a", wallet="W", **kwargs):
    return cash().Movement(action_id, wallet, kind, amount, "synthetic-movement", **kwargs)


def transaction(sequence, *actions, endpoints, fee=0, payer="W", **kwargs):
    m = cash()
    return m.CashTransaction(
        tx_id=kwargs.pop("tx_id", f"synthetic-{sequence}"), sequence=sequence,
        status=kwargs.pop("status", "confirmed"), actions=tuple(actions),
        fee=m.TransactionFee(fee, payer, "synthetic-fee", **kwargs),
        endpoints=tuple(m.CashEndpoint(w, n, "synthetic-endpoint") for w, n in endpoints))


def test_flat_cash_is_not_flat_flow_or_trade_pnl():
    ledger = cash().CashLedger()
    ledger.open_wallet("W", 100, evidence="synthetic-opening")
    ledger.apply(transaction(1, movement("EXTERNAL_DEPOSIT", 30),
                             endpoints=(("W", 130),), overlap="nonoverlap"))
    ledger.apply(transaction(2, movement("TRADE_DEBIT", 20),
                             endpoints=(("W", 110),), overlap="nonoverlap"))
    receipt = ledger.apply(transaction(3, movement("TRADE_CREDIT", 10, "sale"),
                                      movement("EXTERNAL_WITHDRAWAL", 20, "withdraw"),
                                      endpoints=(("W", 100),), overlap="nonoverlap"))
    state = ledger.wallet("W")
    assert state.cash_lamports == state.opening_lamports == 100
    assert state.external_flow_lamports == 10
    assert state.trade_consideration_lamports == -10
    assert state.reconciled is True
    assert receipt.reconciled is True
    assert receipt.economics_eligible is False
    assert not hasattr(state, "realized_pnl_lamports")
    assert receipt.reconciliations[0].expected_lamports == 100
    assert receipt.reconciliations[0].unexplained_delta_lamports == 0


def test_unexplained_endpoint_gap_stays_suspense_even_if_later_cancels():
    ledger = cash().CashLedger()
    ledger.open_wallet("W", 100, evidence="synthetic")
    first = ledger.apply(transaction(1, endpoints=(("W", 107),), overlap="nonoverlap"))
    assert first.reconciled is False
    state = ledger.wallet("W")
    assert state.cash_lamports == 107
    assert state.suspense_lamports == 7
    assert state.external_flow_lamports == state.nontrading_income_lamports == 0
    assert state.reconciled is False
    second = ledger.apply(transaction(2, endpoints=(("W", 100),), overlap="nonoverlap"))
    assert ledger.wallet("W").suspense_lamports == 0
    assert len(ledger.wallet("W").unresolved_reasons) == 2
    assert second.reconciled is False
    assert ledger.receipts == (first, second)


def test_unknown_opening_is_not_backsolved_from_observed_endpoint():
    ledger = cash().CashLedger()
    ledger.open_wallet("W", None, evidence="synthetic", unknown_reason="missing opening")
    receipt = ledger.apply(transaction(1, movement("EXTERNAL_DEPOSIT", 5),
                                      endpoints=(("W", 12),), overlap="nonoverlap"))
    assert receipt.reconciliations[0].expected_lamports is None
    assert receipt.reconciliations[0].unexplained_delta_lamports is None
    assert ledger.wallet("W").opening_lamports is None
    assert ledger.wallet("W").cash_lamports == 12
    assert ledger.wallet("W").suspense_lamports is None
    assert receipt.reconciled is False


@pytest.mark.parametrize("kind,end,signed", [("UNKNOWN_CREDIT", 104, 4), ("UNKNOWN_DEBIT", 96, -4)])
def test_explicit_unknown_movement_never_becomes_trade_or_income(kind, end, signed):
    ledger = cash().CashLedger()
    ledger.open_wallet("W", 100, evidence="synthetic")
    receipt = ledger.apply(transaction(1, movement(kind, 4, unknown_reason="unclassified witness"),
                                      endpoints=(("W", end),), overlap="nonoverlap"))
    state = ledger.wallet("W")
    assert state.suspense_lamports == signed
    assert state.trade_consideration_lamports == state.nontrading_income_lamports == 0
    assert receipt.reconciliations[0].unexplained_delta_lamports == 0
    assert receipt.reconciled is False
    assert state.economics_eligible is False


@pytest.mark.parametrize("kind,amount,end,consideration", [
    ("TRADE_DEBIT", 23, 77, -20), ("TRADE_CREDIT", 17, 117, 20),
])
def test_embedded_fee_normalizes_cash_once_and_gross_consideration(kind, amount, end, consideration):
    ledger = opened()
    receipt = ledger.apply(transaction(1, movement(kind, amount), endpoints=(("W", end),),
                                      fee=3, overlap="overlap", embedded_action_id="a"))
    state = ledger.wallet("W")
    assert receipt.reconciled is True
    assert state.cash_lamports == end
    assert state.trade_consideration_lamports == consideration
    assert state.fee_expense_lamports == 3


def test_separate_fee_payer_does_not_debit_trader_and_multi_action_fee_once():
    ledger = opened()
    ledger.open_wallet("B", 10, evidence="synthetic")
    receipt = ledger.apply(transaction(1, movement("TRADE_DEBIT", 20),
                                      movement("TRADE_CREDIT", 7, "b"),
                                      endpoints=(("W", 87), ("B", 7)),
                                      fee=3, payer="B", overlap="nonoverlap"))
    assert receipt.reconciled is True
    assert ledger.wallet("W").fee_expense_lamports == 0
    assert ledger.wallet("B").fee_expense_lamports == 3
    assert ledger.wallet("B").trade_consideration_lamports == 0


def test_recoverable_rent_partial_refund_is_asset_not_expense_or_income():
    ledger = opened()
    ledger.apply(transaction(1, movement("RENT_DEPOSIT", 10, rent_id="synthetic-account"),
                             endpoints=(("W", 90),), overlap="nonoverlap"))
    ledger.apply(transaction(2, movement("RENT_REFUND", 4, rent_id="synthetic-account"),
                             endpoints=(("W", 94),), overlap="nonoverlap"))
    state = ledger.wallet("W")
    assert state.recoverable_rent_lamports == 6
    assert state.fee_expense_lamports == state.nontrading_income_lamports == 0
    assert state.trade_consideration_lamports == 0
    ledger.apply(transaction(3, movement("RENT_REFUND", 6, rent_id="synthetic-account"),
                             movement("NONTRADING_INCOME", 2, "income"),
                             endpoints=(("W", 102),), overlap="nonoverlap"))
    assert ledger.wallet("W").recoverable_rent_lamports == 0
    assert ledger.wallet("W").nontrading_income_lamports == 2


def test_failed_transaction_only_witnessed_fee_debit():
    ledger = opened()
    receipt = ledger.apply(transaction(1, endpoints=(("W", 97),), fee=3,
                                      overlap="nonoverlap", status="failed"))
    assert receipt.reconciled is True
    assert ledger.wallet("W").trade_consideration_lamports == 0
    assert ledger.wallet("W").fee_expense_lamports == 3


def base_tx():
    return transaction(1, movement("TRADE_DEBIT", 10), endpoints=(("W", 87),),
                       fee=3, overlap="nonoverlap")


def assert_atomic_rejection(ledger, tx):
    before = (ledger.wallet("W"), ledger.receipts)
    with pytest.raises(ValueError):
        ledger.apply(tx)
    assert (ledger.wallet("W"), ledger.receipts) == before


@pytest.mark.parametrize("value", [True, False, 1.0, "1", -1, 2**64, None])
@pytest.mark.parametrize("field", ["action", "fee", "endpoint", "sequence"])
def test_all_native_fields_strict_and_rejection_does_not_consume_identity(value, field):
    ledger, tx = opened(), base_tx()
    if field == "action":
        bad = replace(tx, actions=(replace(tx.actions[0], lamports=value),))
    elif field == "fee":
        bad = replace(tx, fee=replace(tx.fee, lamports=value))
    elif field == "endpoint":
        bad = replace(tx, endpoints=(replace(tx.endpoints[0], lamports=value),))
    else:
        bad = replace(tx, sequence=value)
    assert_atomic_rejection(ledger, bad)
    assert ledger.apply(tx).reconciled is True


@pytest.mark.parametrize("change", [
    {"overlap": None}, {"overlap": "unknown"}, {"overlap": True},
    {"overlap": "overlap"}, {"embedded_action_id": "a"},
    {"overlap": "overlap", "embedded_action_id": "absent"},
    {"payer": "absent"}, {"payer": ""}, {"evidence": ""},
    {"overlap": "overlap", "embedded_action_id": "a", "lamports": 11},
])
def test_fee_coverage_must_be_explicit_coherent_and_witnessed(change):
    ledger, tx = opened(), base_tx()
    assert_atomic_rejection(ledger, replace(tx, fee=replace(tx.fee, **change)))


@pytest.mark.parametrize("change", [
    {"action_id": ""}, {"wallet": "absent"}, {"evidence": ""}, {"kind": "TX_FEE"},
    {"kind": "GUESS"}, {"kind": True}, {"kind": "UNKNOWN_DEBIT"},
    {"unknown_reason": "not unknown"}, {"rent_id": "not rent"},
    {"kind": "RENT_DEPOSIT"},
])
def test_movement_classification_is_explicit_no_inference(change):
    ledger, tx = opened(), base_tx()
    assert_atomic_rejection(ledger, replace(tx, actions=(replace(tx.actions[0], **change),)))


@pytest.mark.parametrize("change", [
    {"status": "processed"}, {"status": "pending"}, {"status": True},
    {"tx_id": ""}, {"tx_id": True}, {"endpoints": ()},
    {"endpoints": []}, {"actions": []}, {"fee": None},
])
def test_transaction_shape_failclosed(change):
    assert_atomic_rejection(opened(), replace(base_tx(), **change))


def test_unique_transaction_and_action_identity_sequence_and_immutable_snapshot():
    ledger, tx = opened(), base_tx()
    assert_atomic_rejection(ledger, replace(tx, actions=tx.actions * 2))
    first = ledger.apply(tx)
    assert_atomic_rejection(ledger, tx)
    assert_atomic_rejection(ledger, replace(tx, sequence=2))
    assert_atomic_rejection(ledger, replace(tx, tx_id="new"))
    assert_atomic_rejection(ledger, replace(tx, tx_id="new", sequence=0))
    # Action identity is (tx_id, action_id); the same local index in a new tx is legitimate.
    second = ledger.apply(replace(tx, tx_id="new", sequence=2,
                                  endpoints=(replace(tx.endpoints[0], lamports=74),)))
    assert ledger.receipts == (first, second)
    with pytest.raises(FrozenInstanceError):
        ledger.wallet("W").cash_lamports = 200
    assert first.transaction == tx
    assert first.training_eligible is False


def test_endpoint_uniqueness_coverage_and_evidence():
    ledger, tx = opened(), base_tx()
    for endpoints in (tx.endpoints * 2, (replace(tx.endpoints[0], evidence=""),),
                      (replace(tx.endpoints[0], wallet="absent"),)):
        assert_atomic_rejection(ledger, replace(tx, endpoints=endpoints))
    ledger.open_wallet("B", 10, evidence="synthetic")
    assert_atomic_rejection(ledger, replace(tx, fee=replace(tx.fee, payer="B")))
    assert_atomic_rejection(ledger, replace(tx, fee=replace(tx.fee, payer="B", overlap="overlap",
                                                          embedded_action_id="a"),
                                           endpoints=tx.endpoints + (cash().CashEndpoint("B", 10, "synthetic"),)))


@pytest.mark.parametrize("status", ["failed"])
def test_failed_transaction_refuses_actions_unwitnessed_or_inconsistent_fee(status):
    tx = replace(base_tx(), status=status)
    assert_atomic_rejection(opened(), tx)
    assert_atomic_rejection(opened(), replace(tx, actions=(), fee=replace(tx.fee, evidence="")))
    assert_atomic_rejection(opened(), replace(tx, actions=()))  # 87 is not 100 - 3
    assert_atomic_rejection(opened(), replace(tx, actions=(), fee=replace(tx.fee, overlap="overlap",
                                                                        embedded_action_id="a")))


def test_rent_refund_requires_prior_same_wallet_asset_and_is_atomic():
    ledger = opened()
    ledger.apply(transaction(1, movement("RENT_DEPOSIT", 10, rent_id="r"),
                             endpoints=(("W", 90),), overlap="nonoverlap"))
    for rent_id, amount in (("missing", 1), ("r", 11)):
        assert_atomic_rejection(ledger, transaction(2, movement("RENT_REFUND", amount, rent_id=rent_id),
                                                   endpoints=(("W", 100),), overlap="nonoverlap"))
    # A valid first action must not mutate rent before a later invalid action rejects.
    assert_atomic_rejection(ledger, transaction(2, movement("RENT_REFUND", 5, "a", rent_id="r"),
                                               movement("RENT_REFUND", 6, "b", rent_id="r"),
                                               endpoints=(("W", 101),), overlap="nonoverlap"))
    result = ledger.apply(transaction(2, movement("RENT_REFUND", 10, rent_id="r"),
                                      endpoints=(("W", 100),), overlap="nonoverlap"))
    assert result.reconciled is True
    assert ledger.wallet("W").recoverable_rent_lamports == 0


def test_wallet_cash_identity_holds_without_pnl_or_rent_expensing():
    ledger = opened()
    steps = [
        (movement("EXTERNAL_DEPOSIT", 20), 120),
        (movement("TRADE_DEBIT", 10), 110),
        (movement("RENT_DEPOSIT", 4, rent_id="r"), 106),
        (movement("NONTRADING_INCOME", 3), 109),
        (movement("UNKNOWN_CREDIT", 2, unknown_reason="unclassified"), 111),
        (movement("RENT_REFUND", 4, rent_id="r"), 115),
        (movement("EXTERNAL_WITHDRAWAL", 5), 110),
    ]
    for sequence, (action, endpoint) in enumerate(steps):
        ledger.apply(transaction(sequence, action, endpoints=(("W", endpoint),), overlap="nonoverlap"))
        state = ledger.wallet("W")
        assert state.cash_lamports == (
            state.opening_lamports + state.external_flow_lamports + state.trade_consideration_lamports
            - state.fee_expense_lamports + state.nontrading_income_lamports
            - state.recoverable_rent_lamports + state.suspense_lamports)
    assert state.reconciled is False


def test_rent_is_not_refundable_by_different_wallet():
    ledger = opened()
    ledger.open_wallet("B", 100, evidence="synthetic")
    ledger.apply(transaction(1, movement("RENT_DEPOSIT", 10, rent_id="r"),
                             endpoints=(("W", 90),), overlap="nonoverlap"))
    before = ledger.wallet("B")
    assert_atomic_rejection(ledger, transaction(2, movement("RENT_REFUND", 10, wallet="B", rent_id="r"),
                                               endpoints=(("W", 90), ("B", 110)), overlap="nonoverlap"))
    assert ledger.wallet("B") == before


def test_late_multiwallet_failure_rejects_every_wallet_and_fee():
    ledger = opened()
    ledger.open_wallet("B", 0, evidence="synthetic")
    before = ledger.wallet("B")
    assert_atomic_rejection(ledger, transaction(1, movement("TRADE_DEBIT", 10),
                                               movement("TRADE_DEBIT", 1, "b", wallet="B"),
                                               endpoints=(("W", 90), ("B", 0)), overlap="nonoverlap"))
    assert ledger.wallet("B") == before
    assert ledger.apply(transaction(1, endpoints=(("W", 100),), overlap="nonoverlap")).reconciled


@pytest.mark.parametrize("value", [None, {}, True])
def test_invalid_record_objects_raise_valueerror(value):
    assert_atomic_rejection(opened(), value)
    assert_atomic_rejection(opened(), replace(base_tx(), actions=(value,)))
    assert_atomic_rejection(opened(), replace(base_tx(), endpoints=(value,)))


def test_literal_identifiers_preserved_and_finalized_still_not_admitted():
    ledger = cash().CashLedger()
    ledger.open_wallet(" W ", 1, evidence=" opening ")
    tx = transaction(1, endpoints=((" W ", 1),), payer=" W ",
                     tx_id=" tx ", status="finalized", overlap="nonoverlap")
    receipt = ledger.apply(tx)
    assert receipt.transaction.tx_id == " tx "
    assert ledger.wallet(" W ").opening_evidence == " opening "
    assert receipt.reconciled and not receipt.economics_eligible and not receipt.training_eligible
    with pytest.raises(KeyError):
        ledger.wallet("W")


def force_field(record, field, value, method):
    """Exercise frozen-dataclass bypasses, not a hostile Python sandbox."""
    if method == "dict":
        record.__dict__[field] = value
    else:
        object.__setattr__(record, field, value)
    assert getattr(record, field) == value


@pytest.mark.parametrize("method", ["dict", "setattr"])
def test_wallet_result_mutation_cannot_falsely_reconcile_cash(method):
    ledger = opened()
    exposed = ledger.wallet("W")
    force_field(exposed, "cash_lamports", 1000, method)
    receipt = ledger.apply(transaction(1, endpoints=(("W", 1000),), overlap="nonoverlap"))
    assert receipt.reconciled is False
    assert receipt.reconciliations[0].expected_lamports == 100
    assert receipt.reconciliations[0].unexplained_delta_lamports == 900
    assert ledger.wallet("W").opening_lamports == 100
    assert ledger.wallet("W").suspense_lamports == 900
    # Even replacing the returned unresolved-reason tuple cannot erase history.
    force_field(ledger.wallet("W"), "unresolved_reasons", (), method)
    later = ledger.apply(transaction(2, endpoints=(("W", 1000),), overlap="nonoverlap"))
    state = ledger.wallet("W")
    assert later.reconciled is False
    assert state.cash_lamports == state.opening_lamports + state.suspense_lamports
    assert ledger.receipts == (receipt, later)


@pytest.mark.parametrize("method", ["dict", "setattr"])
@pytest.mark.parametrize("origin", ["original", "apply", "receipts"])
@pytest.mark.parametrize("path,field,value", [
    ((), "tx_id", "edited"),
    ((), "sequence", 999),
    ((), "status", "failed"),
    ((), "actions", ()),
    ((), "fee", None),
    ((), "endpoints", ()),
    (("fee",), "lamports", 999),
    (("fee",), "evidence", "edited-fee"),
    (("actions", 0), "lamports", 999),
    (("actions", 0), "kind", "NONTRADING_INCOME"),
    (("endpoints", 0), "lamports", 999),
    (("endpoints", 0), "evidence", "edited-endpoint"),
])
def test_transaction_graph_mutation_is_detached(origin, method, path, field, value):
    ledger, tx = opened(), base_tx()
    result = ledger.apply(tx)
    # Independent expected values, never a second reference to the same graph.
    expected = cash().CashReceipt(base_tx(), (cash().Reconciliation("W", 87, 87, 0),), True)
    target = {"original": tx, "apply": result.transaction,
              "receipts": ledger.receipts[0].transaction}[origin]
    for part in path:
        target = target[part] if isinstance(part, int) else getattr(target, part)
    force_field(target, field, value, method)
    assert ledger.receipts == (expected,)
    if origin != "original":
        assert tx == base_tx()
    if origin != "apply":
        assert result == expected
    if origin != "original":
        # Two-way isolation: editing a caller input must not alter a prior output.
        force_field(tx.fee, "lamports", 888, method)
        assert ledger.receipts == (expected,)
        if origin == "receipts":
            assert result == expected
    # A later action failure must roll back rent, wallet totals and history,
    # and leave identity/sequence reusable after the external mutation attempt.
    assert_atomic_rejection(ledger, transaction(
        2, movement("RENT_DEPOSIT", 5, "deposit", rent_id="r"),
        movement("RENT_REFUND", 6, "refund", rent_id="r"),
        endpoints=(("W", 88),), overlap="nonoverlap"))
    assert ledger.wallet("W").recoverable_rent_lamports == 0
    later = ledger.apply(transaction(2, movement("TRADE_CREDIT", 10),
                                     endpoints=(("W", 95),), fee=2, overlap="nonoverlap"))
    assert later.reconciled is True
    state = ledger.wallet("W")
    assert state.cash_lamports == 95
    assert state.cash_lamports == (
        state.opening_lamports + state.external_flow_lamports + state.trade_consideration_lamports
        - state.fee_expense_lamports + state.nontrading_income_lamports
        - state.recoverable_rent_lamports + state.suspense_lamports)
    assert ledger.receipts == (expected, later)
    assert not state.economics_eligible and not state.training_eligible
    assert not later.economics_eligible and not later.training_eligible


@pytest.mark.parametrize("method", ["dict", "setattr"])
@pytest.mark.parametrize("origin", ["apply", "receipts"])
@pytest.mark.parametrize("path,field,value", [
    ((), "transaction", None),
    ((), "reconciled", False),
    ((), "reconciliations", ()),
    (("reconciliations", 0), "expected_lamports", 999),
    (("reconciliations", 0), "observed_lamports", 999),
    (("reconciliations", 0), "unexplained_delta_lamports", 999),
])
def test_receipt_graph_mutation_is_detached(origin, method, path, field, value):
    ledger = opened()
    result = ledger.apply(base_tx())
    prior_read = ledger.receipts[0]
    expected = cash().CashReceipt(base_tx(), (cash().Reconciliation("W", 87, 87, 0),), True)
    target = result if origin == "apply" else ledger.receipts[0]
    for part in path:
        target = target[part] if isinstance(part, int) else getattr(target, part)
    force_field(target, field, value, method)
    assert ledger.receipts == (expected,)
    assert prior_read == expected
    if origin == "receipts":
        assert result == expected
    assert_atomic_rejection(ledger, base_tx())
    later = ledger.apply(transaction(2, endpoints=(("W", 87),), overlap="nonoverlap"))
    assert later.reconciled is True
    state = ledger.wallet("W")
    assert state.cash_lamports == state.opening_lamports + state.trade_consideration_lamports - state.fee_expense_lamports
    assert ledger.receipts == (expected, later)


def test_large_native_units_no_float_roundtrip_and_invalid_derived_cash_atomic():
    ledger = opened(2**64 - 1)
    receipt = ledger.apply(transaction(1, movement("TRADE_DEBIT", 2**53 + 1),
                                      endpoints=(("W", (2**64 - 1) - (2**53 + 1)),),
                                      overlap="nonoverlap"))
    assert receipt.reconciled is True
    assert ledger.wallet("W").trade_consideration_lamports == -(2**53 + 1)
    assert_atomic_rejection(opened(0), transaction(1, movement("TRADE_DEBIT", 1),
                                                   endpoints=(("W", 0),), overlap="nonoverlap"))
    assert_atomic_rejection(opened(2**64 - 1), transaction(1, movement("EXTERNAL_DEPOSIT", 1),
                                                          endpoints=(("W", 2**64 - 1),), overlap="nonoverlap"))
