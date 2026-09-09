"""Structural operating preconditions, never an authorization authority.

Callers must independently authenticate approval evidence, verify artifact hashes,
check EvaluationBoundary.assert_before_fit and enforce rights/admission gates.
Passing this validator alone cannot start tuning, training, promotion or orders.
"""
import re


SAFE_STAGES = frozenset({"inventory", "parser_fixture", "ledger_fixture"})
GATED_STAGES = frozenset({"strategy_tuning", "model_tuning", "economic_tuning", "promotion"})
REQUIRED_LIMITS = frozenset({
    "capital_lamports", "max_order_lamports", "max_positions", "max_hold_ms",
    "max_latency_ms", "max_drawdown_bps", "max_correlated_exposure_bps",
    "fractional_kelly_cap_bps", "min_uplift_lamports", "uncertainty_confidence_bps",
    "max_cost_stress_bps",
})


def assert_stage_allowed(contract, *, stage):
    """Check the numeric/scope gate; test stages need no invented bankroll."""
    if type(contract) is not dict or type(stage) is not str:
        raise ValueError("invalid contract or stage type")
    if stage in SAFE_STAGES:
        return
    if stage not in GATED_STAGES:
        raise ValueError("stage not authorized by this foundation contract")
    expected = {"chain": "solana", "trader_role": "Qwen:trader_only",
                "developer_role": "Astra:rust_developer", "objective": "kelly_net_returned_sol",
                "rights_policy": "permissive_only"}
    for key, value in expected.items():
        if contract.get(key) != value:
            raise ValueError(f"invalid or missing {key}")
    version = contract.get("version")
    if not isinstance(version, str) or not version.strip() or version.upper() in {"TBD", "UNKNOWN"}:
        raise ValueError("missing contract version")
    venues = contract.get("venues")
    if (not isinstance(venues, list) or not venues
            or any(type(v) is not str or v not in {"pumpfun", "pumpswap"} for v in venues)):
        raise ValueError("unapproved chain/venue scope")
    for field in ("risk_policy_sha256", "fee_model_sha256", "baseline_sha256", "evaluation_policy_sha256"):
        value = contract.get(field)
        if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
            raise ValueError(f"missing or malformed {field}")
    limits = contract.get("numeric_limits")
    if not isinstance(limits, dict) or set(limits) != REQUIRED_LIMITS:
        raise ValueError("unknown numeric limits")
    for key in REQUIRED_LIMITS:
        value = limits[key]
        if type(value) is not int or value <= 0:
            raise ValueError(f"unknown or invalid numeric limit: {key}")
        if key in {"max_drawdown_bps", "max_correlated_exposure_bps", "fractional_kelly_cap_bps"} and value > 10000:
            raise ValueError(f"fraction out of range: {key}")
        if key == "uncertainty_confidence_bps" and value >= 10000:
            raise ValueError("uncertainty confidence must be below certainty")
    if limits["max_order_lamports"] > limits["capital_lamports"]:
        raise ValueError("order size exceeds capital")
    approval = contract.get("numeric_approval", {})
    if type(approval) is not dict:
        raise ValueError("numeric approval must be an object")
    at = approval.get("approved_at_utc_ms")
    evidence = approval.get("evidence_id")
    if (approval.get("approved") is not True or type(evidence) is not str or not evidence.strip()
            or type(at) is not int or at < 0):
        raise ValueError("numeric approval evidence missing")
    if "live_orders_authorized" in contract and type(contract["live_orders_authorized"]) is not bool:
        raise ValueError("invalid live authorization flag")
