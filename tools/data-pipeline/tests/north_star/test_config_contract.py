"""Test-only numeric examples are not approved trading parameters."""
import importlib
from pathlib import Path
import sys

import pytest

SRC = Path(__file__).resolve().parents[2] / "src"
sys.path.insert(0, str(SRC))


def api():
    assert (SRC / "north_star" / "contracts.py").exists(), "operating contract implementation missing"
    return importlib.import_module("north_star.contracts")


def fixture_contract():
    # Arbitrary, deliberately tiny TEST ONLY values; not corpus or production config.
    return {
        "version": "fixture-only-v1", "chain": "solana", "venues": ["pumpfun", "pumpswap"],
        "trader_role": "Qwen:trader_only", "developer_role": "Astra:rust_developer",
        "objective": "kelly_net_returned_sol", "rights_policy": "permissive_only",
        "risk_policy_sha256": "a" * 64, "fee_model_sha256": "b" * 64,
        "baseline_sha256": "c" * 64, "evaluation_policy_sha256": "d" * 64,
        "numeric_limits": {
            "capital_lamports": 100, "max_order_lamports": 10, "max_positions": 2,
            "max_hold_ms": 20, "max_latency_ms": 3, "max_drawdown_bps": 10,
            "max_correlated_exposure_bps": 100, "fractional_kelly_cap_bps": 100,
            "min_uplift_lamports": 1, "uncertainty_confidence_bps": 9500,
            "max_cost_stress_bps": 100,
        },
        "numeric_approval": {"approved": True, "evidence_id": "TEST_ONLY_APPROVAL", "approved_at_utc_ms": 1},
        "live_orders_authorized": False,
    }


@pytest.mark.parametrize("stage", ["inventory", "parser_fixture", "ledger_fixture"])
def test_unknown_numeric_limits_do_not_block_safe_fixtures(stage):
    api().assert_stage_allowed({}, stage=stage)


@pytest.mark.parametrize("stage", ["strategy_tuning", "model_tuning", "economic_tuning", "promotion"])
def test_unknown_or_unapproved_numeric_limits_block_tuning(stage):
    m = api()
    with pytest.raises(ValueError):
        m.assert_stage_allowed({}, stage=stage)
    d = fixture_contract()
    m.assert_stage_allowed(d, stage=stage)
    d["numeric_approval"]["approved"] = False
    with pytest.raises(ValueError, match="approval"):
        m.assert_stage_allowed(d, stage=stage)


@pytest.mark.parametrize("field", ["version", "risk_policy_sha256", "fee_model_sha256", "baseline_sha256", "evaluation_policy_sha256", "rights_policy"])
def test_missing_version_risk_fee_baseline_rights_refuses_go(field):
    d = fixture_contract()
    del d[field]
    with pytest.raises(ValueError):
        api().assert_stage_allowed(d, stage="strategy_tuning")


@pytest.mark.parametrize("bad", [None, 0, -1, True, "TBD", float("nan"), float("inf"), 0.5])
def test_invalid_or_placeholder_numeric_limits_fail_closed(bad):
    d = fixture_contract()
    d["numeric_limits"]["capital_lamports"] = bad
    with pytest.raises(ValueError):
        api().assert_stage_allowed(d, stage="strategy_tuning")


@pytest.mark.parametrize("mutation", ["wrong_chain", "wrong_venue", "empty_venues", "bad_hash", "kelly_overflow", "order_above_capital", "missing_limit", "wrong_role", "wrong_objective", "approval_without_evidence"])
def test_contract_scope_and_consistency(mutation):
    d = fixture_contract()
    if mutation == "wrong_chain":
        d["chain"] = "ethereum"
    elif mutation == "wrong_venue":
        d["venues"].append("unapproved")
    elif mutation == "empty_venues":
        d["venues"] = []
    elif mutation == "bad_hash":
        d["fee_model_sha256"] = "placeholder"
    elif mutation == "kelly_overflow":
        d["numeric_limits"]["fractional_kelly_cap_bps"] = 10001
    elif mutation == "order_above_capital":
        d["numeric_limits"]["max_order_lamports"] = 101
    elif mutation == "missing_limit":
        del d["numeric_limits"]["max_latency_ms"]
    elif mutation == "wrong_role":
        d["trader_role"] = "Qwen:rust_developer"
    elif mutation == "wrong_objective":
        d["objective"] = "fixed_take_profit"
    else:
        del d["numeric_approval"]["evidence_id"]
    with pytest.raises(ValueError):
        api().assert_stage_allowed(d, stage="promotion")


@pytest.mark.parametrize("stage", ["live_orders", "training_run", "unknown"])
def test_this_contract_never_grants_live_training_or_unknown_stage(stage):
    d = fixture_contract()
    d["live_orders_authorized"] = True
    with pytest.raises(ValueError):
        api().assert_stage_allowed(d, stage=stage)


@pytest.mark.parametrize("path,bad", [
    (("numeric_approval",), None), (("numeric_approval",), []),
    (("numeric_approval", "evidence_id"), True), (("numeric_approval", "evidence_id"), ["id"]),
    (("numeric_approval", "evidence_id"), {"id": "id"}), (("numeric_approval", "evidence_id"), " "),
    (("numeric_approval", "approved"), 1), (("numeric_approval", "approved_at_utc_ms"), True),
    (("venues",), [[]]), (("venues",), [{}]), (("venues",), "pumpfun"),
    (("live_orders_authorized",), "false"), (("numeric_limits", "extra"), []),
])
def test_contract_nested_types_fail_closed(path, bad):
    d = fixture_contract()
    target = d
    for key in path[:-1]:
        target = target[key]
    target[path[-1]] = bad
    with pytest.raises(ValueError):
        api().assert_stage_allowed(d, stage="promotion")


@pytest.mark.parametrize("bad", [None, [], "contract", 1])
@pytest.mark.parametrize("stage", ["inventory", "promotion"])
def test_contract_requires_object_even_for_safe_fixtures(bad, stage):
    with pytest.raises(ValueError):
        api().assert_stage_allowed(bad, stage=stage)


@pytest.mark.parametrize("bad", [[], {}, None])
def test_stage_type_is_validated(bad):
    with pytest.raises(ValueError):
        api().assert_stage_allowed(fixture_contract(), stage=bad)
