"""Synthetic accounting fixtures only; never source truth or training examples."""
import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


def accounting():
    path = ROOT / "src/north_star/accounting.py"
    assert path.exists(), "exact accounting module not implemented"
    name = "north_star_accounting_test"
    if name not in sys.modules:
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
    return sys.modules[name]


def test_allocation_preserves_every_lamport_with_cumulative_floor():
    m = accounting()
    assert m.allocate_lamports(10, (1, 1, 1)) == (3, 3, 4)
    assert m.allocate_lamports(2**64 - 1, (1, 2, 4)) == tuple(
        ((2**64 - 1) * end // 7) - ((2**64 - 1) * start // 7)
        for start, end in [(0, 1), (1, 3), (3, 7)]
    )
    assert m.allocate_lamports(-2, (1, 1, 1)) == (-1, -1, 0)
    assert m.allocate_lamports(0, (1,)) == (0,)


@pytest.mark.parametrize("total,weights", [
    (True, (1,)), (1.0, (1,)), ("1", (1,)), (1, ()),
    (1, (0,)), (1, (-1, 2)), (1, (True,)), (1, (1.0,)),
])
def test_allocation_rejects_non_integer_or_nonpositive_weights(total, weights):
    with pytest.raises(ValueError):
        accounting().allocate_lamports(total, weights)


def action(kind, quantity, lamports=0, **kwargs):
    m = accounting()
    return m.Action(action_id=kwargs.pop("action_id", "a"), kind=kind,
                    wallet=kwargs.pop("wallet", "W"), mint=kwargs.pop("mint", "M"),
                    decimals=kwargs.pop("decimals", 6), raw_quantity=quantity,
                    lamports=lamports, **kwargs)


def post(ledger, sequence, *actions, fee=0, status="confirmed", tx_id=None):
    return ledger.apply(accounting().Transaction(
        tx_id=tx_id or f"synthetic-{sequence}", sequence=sequence,
        status=status, fee_lamports=fee, actions=tuple(actions)))


def test_fifo_multiple_buys_partial_exit_and_reentry_exact_cash():
    ledger = accounting().Ledger()
    post(ledger, 1, action("BUY", 3, 10), fee=2)
    post(ledger, 2, action("BUY", 2, 9))
    result = post(ledger, 3, action("SELL", 4, 21), fee=1)
    assert [s.raw_quantity for s in result.sales] == [3, 1]
    assert [s.cost_lamports for s in result.sales] == [10, 4]
    assert [s.proceeds_lamports for s in result.sales] == [15, 6]
    assert ledger.quantity("W", "M") == 1
    assert ledger.inventory_cost("W", "M") == 5
    assert ledger.realized_pnl_lamports == 4
    assert ledger.cash_delta_lamports == -1
    post(ledger, 4, action("SELL", 1, 2))
    assert ledger.quantity("W", "M") == 0
    assert ledger.realized_pnl_lamports == ledger.cash_delta_lamports == 1
    post(ledger, 5, action("BUY", 1, 7))
    assert ledger.inventory_cost("W", "M") == 7


def test_many_tiny_exits_preserve_cost_and_large_integer_units():
    ledger = accounting().Ledger()
    total = 2**64 - 1
    post(ledger, 1, action("BUY", 7, total))
    costs = []
    for sequence in range(2, 9):
        costs.extend(s.cost_lamports for s in post(ledger, sequence, action("SELL", 1, 1)).sales)
    assert sum(costs) == total
    assert costs == list(accounting().allocate_lamports(total, (1,) * 7))
    assert ledger.realized_pnl_lamports == 7 - total


def test_transfer_preserves_origin_fifo_basis_and_never_books_proceeds():
    ledger = accounting().Ledger()
    post(ledger, 1, action("BUY", 3, 10))
    post(ledger, 2, action("BUY", 1, 20, wallet="B"))
    result = post(ledger, 3, action("TRANSFER", 2, destination="B", owner_evidence="synthetic-proof"))
    assert result.sales == ()
    assert ledger.realized_pnl_lamports == 0
    assert ledger.cash_delta_lamports == -30
    assert ledger.inventory_cost("W", "M") == 4
    assert ledger.inventory_cost("B", "M") == 26
    sale = post(ledger, 4, action("SELL", 1, 5, wallet="B")).sales[0]
    assert sale.cost_lamports == 3
    assert sale.origin_id == ("synthetic-1", "a")
    assert ledger.inventory_cost("B", "M") == 23


def test_external_transfers_are_capital_not_trade_pnl():
    ledger = accounting().Ledger()
    post(ledger, 1, action("TRANSFER_IN", 3, 10))
    result = post(ledger, 2, action("TRANSFER_OUT", 2))
    assert result.sales == ()
    assert result.transferred_cost_lamports == 6
    assert ledger.cash_delta_lamports == ledger.realized_pnl_lamports == 0
    assert ledger.inventory_cost("W", "M") == 4


def test_unknown_opening_cost_preserves_fill_but_fails_closed_pnl():
    ledger = accounting().Ledger()
    post(ledger, 1, action("OPEN", 2, None, unknown_cost_reason="MISSING_HISTORY"))
    assert ledger.inventory_cost("W", "M") is None
    result = post(ledger, 2, action("SELL", 1, 100))
    assert result.sales[0].raw_quantity == 1
    assert result.sales[0].proceeds_lamports == 100
    assert result.sales[0].cost_lamports is None
    assert result.sales[0].pnl_lamports is None
    assert result.sales[0].unknown_cost_reason == "MISSING_HISTORY"
    assert ledger.realized_pnl_lamports is None
    assert ledger.cash_delta_lamports == 100
    post(ledger, 3, action("SELL", 1, 1))
    assert ledger.quantity("W", "M") == 0
    assert ledger.realized_pnl_lamports is None


def test_failed_fee_only_has_no_fill_and_cannot_reset_inventory():
    ledger = accounting().Ledger()
    post(ledger, 1, action("BUY", 2, 10))
    result = post(ledger, 2, fee=3, status="failed")
    assert result.sales == ()
    assert result.fills == ()
    assert ledger.quantity("W", "M") == 2
    assert ledger.realized_pnl_lamports == -3
    assert ledger.cash_delta_lamports == -13
    with pytest.raises(ValueError, match="failed"):
        post(ledger, 3, action("BUY", 100, 0), fee=4, status="failed")
    assert ledger.cash_delta_lamports == -13


def test_duplicate_transaction_or_action_rejected_without_side_effects():
    ledger = accounting().Ledger()
    tx = accounting().Transaction("same", 1, "confirmed", 3, (action("BUY", 3, 10),))
    ledger.apply(tx)
    with pytest.raises(ValueError, match="duplicate transaction"):
        ledger.apply(tx)
    with pytest.raises(ValueError, match="duplicate transaction"):
        post(ledger, 2, action("SELL", 1, 5), tx_id="same")
    with pytest.raises(ValueError, match="duplicate action"):
        post(ledger, 2, action("BUY", 1, 1), action("BUY", 1, 1))
    assert ledger.quantity("W", "M") == 3
    assert ledger.cash_delta_lamports == -13
    post(ledger, 2, action("BUY", 1, 1), action("BUY", 1, 1, action_id="b"), fee=2)
    assert ledger.cash_delta_lamports == -17


def test_oversell_and_late_action_failure_roll_back_entire_transaction():
    ledger = accounting().Ledger()
    post(ledger, 1, action("BUY", 2, 7))
    with pytest.raises(ValueError, match="insufficient inventory"):
        post(ledger, 2, action("SELL", 1, 9), action("SELL", 2, 9, action_id="b"), fee=3)
    assert ledger.quantity("W", "M") == 2
    assert ledger.inventory_cost("W", "M") == 7
    assert ledger.realized_pnl_lamports == 0
    assert ledger.cash_delta_lamports == -7
    post(ledger, 2, action("SELL", 2, 9))
    assert ledger.realized_pnl_lamports == 2


@pytest.mark.parametrize("change", [
    {"raw_quantity": True}, {"raw_quantity": 0}, {"raw_quantity": -1},
    {"raw_quantity": 1.0}, {"lamports": 1.0}, {"lamports": True},
    {"lamports": -1}, {"lamports": None}, {"decimals": True},
    {"decimals": 256}, {"decimals": -1}, {"wallet": ""},
    {"mint": ""}, {"kind": "SWAP"}, {"action_id": ""},
    {"unknown_cost_reason": "SPURIOUS"},
])
def test_action_validation_fails_closed(change):
    m = accounting()
    args = dict(action_id="a", kind="BUY", wallet="W", mint="M", decimals=6,
                raw_quantity=1, lamports=1)
    args.update(change)
    ledger = m.Ledger()
    with pytest.raises(ValueError):
        post(ledger, 1, m.Action(**args))
    assert ledger.cash_delta_lamports == 0
    assert ledger.quantity("W", "M") == 0


def test_order_decimals_unknown_cost_and_ownership_are_not_guessed():
    ledger = accounting().Ledger()
    post(ledger, 1, action("BUY", 3, 10))
    for candidate in [
        action("SELL", 1, 4, decimals=9),
        action("TRANSFER", 1, destination="B"),
        action("TRANSFER", 1, destination="W", owner_evidence="proof"),
        action("TRANSFER_OUT", 1, 2),
        action("TRANSFER_IN", 1, None),
        action("OPEN", 1, 2),
    ]:
        with pytest.raises(ValueError):
            post(ledger, 2, candidate)
    with pytest.raises(ValueError, match="order"):
        post(ledger, 0, action("SELL", 1, 1))
    assert ledger.quantity("W", "M") == 3
    assert ledger.cash_delta_lamports == -10


@pytest.mark.parametrize("change", [
    {"tx_id": ""}, {"tx_id": " "}, {"sequence": True}, {"sequence": 1.0},
    {"sequence": -1}, {"status": "pending"}, {"status": "timeout"},
    {"status": "reverted"}, {"fee_lamports": None}, {"fee_lamports": True},
    {"fee_lamports": 1.0}, {"fee_lamports": -1}, {"fee_lamports": 2**64},
    {"actions": []}, {"actions": ("not an action",)},
])
def test_transaction_boundary_validation_is_atomic(change):
    m = accounting()
    ledger = m.Ledger()
    args = dict(tx_id="T", sequence=1, status="confirmed", fee_lamports=0, actions=())
    args.update(change)
    with pytest.raises(ValueError):
        ledger.apply(m.Transaction(**args))
    assert ledger.realized_pnl_lamports == ledger.cash_delta_lamports == 0
    post(ledger, 1, action("BUY", 1, 1), tx_id="T")


@pytest.mark.parametrize("field", ["raw_quantity", "lamports"])
def test_input_amounts_over_u64_fail_without_aggregate_overflow(field):
    m = accounting()
    ledger = m.Ledger()
    args = dict(action_id="a", kind="BUY", wallet="W", mint="M", decimals=6,
                raw_quantity=1, lamports=1)
    args[field] = 2**64
    with pytest.raises(ValueError):
        post(ledger, 1, m.Action(**args))
    post(ledger, 1, action("BUY", 2**64 - 1, 2**64 - 1))
    post(ledger, 2, action("BUY", 2**64 - 1, 2**64 - 1))
    assert ledger.quantity("W", "M") == 2 * (2**64 - 1)
    assert ledger.inventory_cost("W", "M") == 2 * (2**64 - 1)


def test_zero_proceeds_retains_loss_and_confirmed_free_lot_is_not_unknown():
    ledger = accounting().Ledger()
    post(ledger, 1, action("OPEN", 1, 0))
    post(ledger, 2, action("BUY", 1, 7))
    result = post(ledger, 3, action("SELL", 2, 0), fee=1)
    assert [s.pnl_lamports for s in result.sales] == [0, -7]
    assert ledger.realized_pnl_lamports == -8
    assert ledger.inventory_cost("W", "M") == 0


def test_unknown_cost_survives_transfer_and_known_sales_do_not_hide_it():
    ledger = accounting().Ledger()
    post(ledger, 1, action("TRANSFER_IN", 2, None, unknown_cost_reason="UNOBSERVED_ACQUISITION"))
    post(ledger, 2, action("TRANSFER", 1, destination="B", owner_evidence="proof"))
    assert ledger.inventory_cost("B", "M") is None
    post(ledger, 3, action("BUY", 1, 2, wallet="B"))
    result = post(ledger, 4, action("SELL", 2, 10, wallet="B"))
    assert [s.cost_lamports for s in result.sales] == [None, 2]
    assert [s.pnl_lamports for s in result.sales] == [None, 3]
    post(ledger, 5, action("BUY", 1, 1, wallet="B"))
    post(ledger, 6, action("SELL", 1, 100, wallet="B"))
    assert ledger.realized_pnl_lamports is None
    assert ledger.inventory_cost("W", "M") is None


def test_transaction_fee_charged_once_across_mints_with_no_embedded_double_cost():
    ledger = accounting().Ledger()
    result = post(ledger, 1, action("BUY", 1, 3),
                  action("BUY", 1, 4, mint="N", action_id="b"), fee=5)
    assert len(result.fills) == 2
    assert result.cash_delta_lamports == -12
    assert ledger.realized_pnl_lamports == -5
    assert ledger.inventory_cost("W", "M") == 3
    assert ledger.inventory_cost("W", "N") == 4
    post(ledger, 2, action("SELL", 1, 10),
         action("SELL", 1, 11, mint="N", action_id="b"), fee=2)
    assert ledger.realized_pnl_lamports == ledger.cash_delta_lamports == 7


def test_open_cannot_restart_history_and_identity_is_literal():
    ledger = accounting().Ledger()
    post(ledger, 1, action("OPEN", 1, 3, wallet=" W "), tx_id=" T ")
    assert ledger.quantity("W", "M") == 0
    post(ledger, 2, action("SELL", 1, 4, wallet=" W "), tx_id="T")
    with pytest.raises(ValueError, match="opening"):
        post(ledger, 3, action("OPEN", 1, 0, wallet=" W "))
    assert ledger.realized_pnl_lamports == 1


def test_unknown_transferred_basis_remains_explicit_and_no_missing_history_sale():
    ledger = accounting().Ledger()
    with pytest.raises(ValueError, match="insufficient inventory"):
        post(ledger, 1, action("SELL", 1, 100))
    post(ledger, 1, action("OPEN", 1, None, unknown_cost_reason="SOURCE_OUTAGE"))
    result = post(ledger, 2, action("TRANSFER_OUT", 1))
    assert result.transferred_cost_lamports is None
    assert result.sales == result.fills == ()
    assert ledger.realized_pnl_lamports == ledger.cash_delta_lamports == 0


def test_transfer_roundtrip_and_randomized_paths_conserve_integer_basis():
    import random

    rng = random.Random(1847)  # Synthetic engineering seed, not a strategy parameter.
    for _ in range(40):
        ledger = accounting().Ledger()
        quantity = rng.randrange(2, 30)
        cost = rng.randrange(0, 2**64)
        post(ledger, 1, action("BUY", quantity, cost))
        sequence = 2
        realized_cost = 0
        realized_revenue = 0
        realized_quantity = 0
        while ledger.quantity("W", "M") + ledger.quantity("B", "M"):
            wallets = [w for w in ("W", "B") if ledger.quantity(w, "M")]
            wallet = rng.choice(wallets)
            amount = rng.randrange(1, ledger.quantity(wallet, "M") + 1)
            if rng.randrange(3) == 0:
                other = "B" if wallet == "W" else "W"
                post(ledger, sequence, action("TRANSFER", amount, wallet=wallet,
                     destination=other, owner_evidence="synthetic-proof"))
            else:
                revenue = rng.randrange(0, 2**64)
                result = post(ledger, sequence, action("SELL", amount, revenue, wallet=wallet))
                realized_cost += sum(s.cost_lamports for s in result.sales)
                realized_revenue += revenue
                realized_quantity += amount
                assert sum(s.proceeds_lamports for s in result.sales) == revenue
                assert sum(s.raw_quantity for s in result.sales) == amount
            sequence += 1
            assert realized_cost + sum(ledger.inventory_cost(w, "M") for w in ("W", "B")) == cost
            assert realized_quantity + sum(ledger.quantity(w, "M") for w in ("W", "B")) == quantity
        assert ledger.realized_pnl_lamports == ledger.cash_delta_lamports == realized_revenue - cost


def test_lot_snapshots_and_receipts_cannot_mutate_state():
    from dataclasses import FrozenInstanceError

    ledger = accounting().Ledger()
    receipt = post(ledger, 1, action("BUY", 1, 7))
    lots = ledger.lots("W", "M")
    with pytest.raises(FrozenInstanceError):
        lots[0].stop = 100
    with pytest.raises(FrozenInstanceError):
        receipt.fills[0].lamports = 0
    post(ledger, 2, action("SELL", 1, 5))
    assert lots[0].raw_quantity == 1
    assert ledger.quantity("W", "M") == 0
