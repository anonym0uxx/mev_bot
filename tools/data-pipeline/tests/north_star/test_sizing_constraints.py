"""Offline sizing fixtures only; no order submission or operational approval."""
import hashlib
import importlib
import importlib.util
import json
from pathlib import Path
import sys

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "src"))


def api():
    assert importlib.util.find_spec("north_star.sizing_constraints") is not None, "offline sizing predicate is missing"
    return importlib.import_module("north_star.sizing_constraints")


def test_boundary_is_only_an_offline_per_order_cap_check_with_pinned_source():
    module = api()
    result = module.evaluate_entry_size(250000000)
    source_path = ROOT / result["source_contract_ref"]
    raw = source_path.read_bytes()
    source = json.loads(raw)
    assert result == {
        "size_constraints_satisfied": True,
        "reasons": [],
        "max_entry_lamports": 250000000,
        "cap_scope": "per_order_entry",
        "cap_lift_requires_explicit_operator_change": True,
        "cumulative_position_cap_lamports": None,
        "add_semantics_approved": False,
        "live_orders_authorized": False,
        "full_operating_contract_approved": False,
        "selected_bankroll_lamports": None,
        "maximum_drawdown": None,
        "credits_remaining": None,
        "cash_bound_checked": False,
        "pending_reservations_accounted_for": False,
        "spendable_cash_lamports": None,
        "source_contract_ref": "north_star/OPERATOR_SCOPE_UPDATE.json",
        "source_contract_sha256": "ca1632456b13c93d398a4043a82c5a26d004ffdc1c3c4a15747f08da6dcdff27",
    }
    assert hashlib.sha256(raw).hexdigest() == result["source_contract_sha256"]
    assert type(result["max_entry_lamports"]) is int
    assert type(source["sizing"]["max_entry_lamports"]) is int
    assert source["sizing"]["max_entry_lamports"] == result["max_entry_lamports"]
    assert source["sizing"]["cap_lift_requires_explicit_operator_change"] is True
    assert source["sizing"]["selected_bankroll_lamports"] is None
    assert source["execution"]["live_orders_authorized_by_this_update"] is False


@pytest.mark.parametrize("size", [250000001, 500000000, 10**100])
def test_above_cap_is_rejected_without_clipping(size):
    result = api().evaluate_entry_size(size)
    assert result["size_constraints_satisfied"] is False
    assert result["reasons"] == ["entry_size_exceeds_per_order_cap"]


class IntSubclass(int):
    pass


@pytest.mark.parametrize("size", [True, False, -1, 0.0, 1.0, 250000000.0,
                                  float("nan"), float("inf"), None, "1", [], {}, IntSubclass(1)])
def test_size_requires_exact_native_nonnegative_integer(size):
    result = api().evaluate_entry_size(size)
    assert result["size_constraints_satisfied"] is False
    assert result["reasons"] == ["entry_size_requires_native_nonnegative_integer"]


def test_omitted_size_is_explicitly_rejected():
    result = api().evaluate_entry_size()
    assert result["size_constraints_satisfied"] is False
    assert result["reasons"] == ["entry_size_requires_native_nonnegative_integer"]


@pytest.mark.parametrize("cash,size,passed", [(100, 101, False), (100, 100, True), (0, 1, False)])
def test_known_cash_is_an_additional_bound_not_a_bankroll_selection(cash, size, passed):
    result = api().evaluate_entry_size(size, available_cash_lamports=cash)
    assert result["size_constraints_satisfied"] is passed
    assert result["cash_bound_checked"] is True
    assert result["pending_reservations_accounted_for"] is False
    assert result["spendable_cash_lamports"] is None
    assert result["selected_bankroll_lamports"] is None
    assert result["reasons"] == ([] if passed else ["entry_size_exceeds_known_available_cash"])


@pytest.mark.parametrize("cash,pending,size,passed,spendable", [
    (100, 40, 60, True, 60), (100, 40, 61, False, 60),
    (100, 0, 100, True, 100), (100, 100, 1, False, 0),
    (100, 101, 0, False, 0), (10**100, 0, 250000001, False, 10**100),
])
def test_known_reservations_reduce_cash_but_never_lift_entry_cap(cash, pending, size, passed, spendable):
    result = api().evaluate_entry_size(size, available_cash_lamports=cash,
                                       pending_reservations_lamports=pending)
    assert result["size_constraints_satisfied"] is passed
    assert result["cash_bound_checked"] is True
    assert result["pending_reservations_accounted_for"] is True
    assert result["spendable_cash_lamports"] == spendable
    if pending > cash:
        assert "pending_reservations_exceed_known_available_cash" in result["reasons"]
    elif size > spendable:
        assert "entry_size_exceeds_cash_after_pending_reservations" in result["reasons"]
    if size > 250000000:
        assert "entry_size_exceeds_per_order_cap" in result["reasons"]


def test_pending_without_cash_does_not_infer_cash_or_bankroll():
    result = api().evaluate_entry_size(1, pending_reservations_lamports=10**100)
    assert result["size_constraints_satisfied"] is True
    assert result["cash_bound_checked"] is False
    assert result["pending_reservations_accounted_for"] is False
    assert result["spendable_cash_lamports"] is None
    assert result["selected_bankroll_lamports"] is None


@pytest.mark.parametrize("field", ["available_cash_lamports", "pending_reservations_lamports"])
@pytest.mark.parametrize("bad", [True, False, -1, 0.0, 100.0, float("nan"),
                                 float("inf"), "100", [], {}, IntSubclass(100)])
def test_malformed_known_cash_or_reservations_are_rejected(field, bad):
    kwargs = {"available_cash_lamports": 100, "pending_reservations_lamports": 0}
    kwargs[field] = bad
    result = api().evaluate_entry_size(1, **kwargs)
    assert result["size_constraints_satisfied"] is False
    assert field + "_requires_native_nonnegative_integer" in result["reasons"]
    assert result["pending_reservations_accounted_for"] is False
    assert result["spendable_cash_lamports"] is None


def test_malformed_pending_is_rejected_even_when_cash_is_unknown():
    result = api().evaluate_entry_size(1, pending_reservations_lamports=-1)
    assert result["size_constraints_satisfied"] is False
    assert result["reasons"] == ["pending_reservations_lamports_requires_native_nonnegative_integer"]
