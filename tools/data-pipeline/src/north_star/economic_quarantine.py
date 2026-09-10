"""Fail-closed, reference-only access to one byte-pinned Slinky sample.

Not wired into legacy loaders or main admission. Python callers can ignore this
API; it is not a process/filesystem security boundary. No override knobs, corpus
scans, target computation, calibration, source writes, or training approval.
"""
from dataclasses import dataclass
from fnmatch import fnmatchcase
import hashlib
import json
from pathlib import Path
from types import MappingProxyType
from typing import Mapping

REGISTRY_PATH = Path(__file__).resolve().parents[2] / "north_star" / "SLINKY_ECONOMICS_QUARANTINE.json"
REGISTRY_SHA256 = "0019b3455d9c6aebcafa85ee5fe0139aa9933583089a4bbd4d4d001dc62a4c9b"
SAMPLE_PATH = "D:/mev_bot-artifacts/north_star/development/slinky_economics_audit_v1/development_samples.json"
SAMPLE_SHA256 = "d1068309e3c16053d32c7ba55f8fcb7e6d44851c4ca8c03004331716f6054ae7"
SAMPLE_BYTES = 1531054
_LAYERS = ("counterfactual_trade_v3", "pump_outcome_v3", "pump_state_v3")

# Finite reviewed request vocabulary, NOT permissive prefix matching. Fields
# outside this bounded vocabulary fail closed, even if present in the sample.
_CF_FIELDS = frozenset("""
economic_class economic_class_reason eligibility_reason eligible
entry_fee_bps entry_fee_lamports entry_fee_sol entry_price_lamports entry_price_sol
entry_size_lamports entry_size_sol entry_slippage_bp entry_slippage_sol
entry_tip_lamports entry_total_cost_sol event_time_unix_ms
exact_entry_eff_price_sol exact_entry_impact_bp exact_entry_tokens
exact_exit_impact_bp exact_exit_sol_out exact_gross_pnl_sol exact_gross_return_bp
exact_net_pnl_sol exact_net_return_bp execution_assumptions_version execution_config_hash
exit_feasibility_note exit_feasible exit_fee_bps exit_fee_lamports exit_fee_sol
exit_price_lamports exit_price_sol exit_reason exit_slippage_bp exit_slippage_sol
exit_tip_lamports exit_total_revenue_sol gross_pnl_lamports gross_pnl_sol
hold_duration_seconds latency_ms max_hold_seconds mint net_pnl_lamports net_pnl_sol
net_return_bp net_return_pct pipeline_version risk_reward_ratio run_uuid
slippage_default_bp state_id stop_loss_bp tp_target_bp
""".split()) | frozenset(
    f"sz{size}_{suffix}" for size in ("005", "010", "025", "050", "100")
    for suffix in ("entry_eff_price_sol", "entry_impact_bp", "entry_tokens",
                   "exit_impact_bp", "exit_sol", "feasible", "gross_pnl_sol",
                   "gross_return_bp", "net_pnl_sol", "net_return_bp")
)
_STATE_FIELDS = frozenset("""
state_id mint event_time_unix_ms code_config_hash pipeline_version producer_version
run_uuid source source_hash git_sha trade_side is_buy sol_amount_lamports
sol_amount_sol token_amount_raw token_amount_tokens seq venue price_sol
v_sol_bonding_curve_lamports v_sol_bonding_curve_sol v_tokens_bonding_curve_raw
""".split())
_OUTCOME_FIELDS = frozenset("""
state_id mint event_time_unix_ms pipeline_version run_uuid exit_event_time_ms
exit_v_sol_lamports exit_v_tok_raw observation_end_ms observed_through_300s
right_censored_300s venue_censored venue_censoring_reason has_future_trade_300s
""".split())
_FIELDS = frozenset(
    f"{layer}.{field}" for layer, fields in zip(_LAYERS, (_CF_FIELDS, _OUTCOME_FIELDS, _STATE_FIELDS))
    for field in fields
)
_DEPENDENT_FIELDS = frozenset(f"dependent_targets.{name}" for name in (
    "utility", "robust_utility", "relative_rank", "recommended_action",
    "recommended_size", "target_gate", "feasibility", "capacity",
    "median_net_return_pct", "profitability", "size", "rank",
))
_TARGET_USES = frozenset(("calibration", "teacher", "target", "training"))


@dataclass(frozen=True)
class ReferenceSample:
    """Projected legacy values plus explicit, immutable non-admission flags."""
    rows: tuple[Mapping[str, object], ...]
    field_status: Mapping[str, str]
    sample_sha256: str = SAMPLE_SHA256
    registry_sha256: str = REGISTRY_SHA256
    disposition: str = "REFERENCE_ONLY_ALLOWED"
    admitted: bool = False
    training_approved: bool = False
    economic_certified: bool = False
    fit_authorized: bool = False
    split: str = "DEVELOPMENT_EXPOSED"


def _read_pinned(path: Path, expected_bytes: int, digest: str, reason: str) -> bytes:
    try:
        with path.open("rb") as handle:
            raw = handle.read(expected_bytes + 1)
    except OSError as exc:
        raise ValueError(f"{reason}: unavailable") from exc
    if len(raw) != expected_bytes or hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError(f"{reason}: bytes/hash mismatch")
    return raw


def _registry():
    # Always first. No cache: an on-disk policy change invalidates the next call.
    raw = _read_pinned(REGISTRY_PATH, 30461, REGISTRY_SHA256, "REGISTRY_PIN")
    registry = json.loads(raw)
    evidence = [r for r in registry["evidence"] if r["path"] == SAMPLE_PATH]
    if evidence != [{"path": SAMPLE_PATH, "bytes": SAMPLE_BYTES, "sha256": SAMPLE_SHA256}]:
        raise ValueError("REGISTRY_PIN: sample binding mismatch")
    return registry


def _expected_lineage(registry):
    lineage = registry["lineage"]
    producer = next(r for r in lineage["source_receipts"]
                    if r["path"] == "D:/repos/mev_bot/tools/data-pipeline/src/build_slinky_gold_v3.py")
    return {
        "source": lineage["source"], "run_uuid": lineage["run_uuid"],
        "pipeline_version": lineage["pipeline_version"],
        "source_hash": lineage["source_hash_literal"],
        "code_config_hash": lineage["manifest_code_config_hash_literal"],
        "execution_config_hash": lineage["execution_config_hash_literal"],
        "producer_sha256": producer["sha256"],
    }


def _quarantined(field, registry):
    if field in _DEPENDENT_FIELDS:
        return True
    layer, column = field.split(".")
    if layer != "counterfactual_trade_v3":
        return False
    patterns = [pattern for defect in registry["defects"]
                for pattern in defect.get("affected_field_patterns", ())]
    # Third registry defect describes censored feasibility in prose, not patterns.
    return (any(fnmatchcase(column, pattern) for pattern in patterns)
            or column in {f"sz{size}_feasible" for size in ("005", "010", "025", "050", "100")})


def _validate_sample(data, expected):
    if type(data) is not dict or set(data) != set(_LAYERS):
        raise ValueError("SAMPLE_SHAPE: expected exactly three saved layers")
    for layer in _LAYERS:
        rows = data[layer]
        if type(rows) is not list or len(rows) != 128 or any(type(r) is not dict for r in rows):
            raise ValueError("SAMPLE_SHAPE: expected 128 rows per layer")
        for row in rows:
            required = {k: expected[k] for k in ("pipeline_version", "run_uuid")}
            if layer == "pump_state_v3":
                required.update({k: expected[k] for k in ("code_config_hash", "source_hash")})
                required.update(producer_version=expected["source"], source="pumpdev", git_sha="fd43b8f1e9e8")
            elif layer == "counterfactual_trade_v3":
                required.update(execution_config_hash=expected["execution_config_hash"],
                                execution_assumptions_version="exec_v3.0")
            if any(row.get(k) != v for k, v in required.items()):
                raise ValueError("SAMPLE_LINEAGE: producer identity mismatch")
    identities = []
    for index in range(128):
        joins = [tuple(data[layer][index].get(k) for k in ("state_id", "mint", "event_time_unix_ms"))
                 for layer in _LAYERS]
        if any(x is None for x in joins[0]) or any(j != joins[0] for j in joins):
            raise ValueError("SAMPLE_JOIN: mismatched identity")
        identities.append(joins[0][0])
    if len(set(identities)) != 128:
        raise ValueError("SAMPLE_JOIN: duplicate states")


def _load(*, sample_path, fields, lineage, split, use, max_rows):
    registry = _registry()
    if type(sample_path) is not str or sample_path != SAMPLE_PATH:
        raise ValueError("EXACT_PATH: only the literal pinned saved sample is supported")
    if type(split) is not str or split != registry["scope"]["exposure"]:
        raise ValueError("PROTECTED_SPLIT: only DEVELOPMENT_EXPOSED is supported")
    expected = _expected_lineage(registry)
    if type(lineage) is not dict or lineage != expected:
        raise ValueError("UNKNOWN_LINEAGE: exact reviewed identity required")
    if type(max_rows) is not int or not 1 <= max_rows <= 128:
        raise ValueError("ROW_BOUND: integer 1..128 required")
    if (type(fields) not in (tuple, list) or not fields
            or len(fields) > len(_FIELDS | _DEPENDENT_FIELDS)
            or any(type(f) is not str for f in fields) or len(set(fields)) != len(fields)):
        raise ValueError("FIELD_REQUEST: distinct exact qualified fields required")
    fields = tuple(fields)
    if any(f not in _FIELDS | _DEPENDENT_FIELDS for f in fields):
        raise ValueError("UNKNOWN_FIELD: outside reviewed finite vocabulary")
    if type(use) is not str or use not in _TARGET_USES | {"audit_reference", "raw_observation_inventory"}:
        raise ValueError("UNKNOWN_USE")
    quarantined = {f for f in fields if _quarantined(f, registry)}
    if use == "raw_observation_inventory" and any(
            not f.startswith(("pump_state_v3.", "pump_outcome_v3.")) for f in fields):
        raise ValueError("RAW_OBSERVATION_ONLY: economic and dependent fields excluded")
    if use in _TARGET_USES:
        if quarantined:
            raise ValueError("ECONOMIC_QUARANTINE: " + ", ".join(sorted(quarantined)))
        raise ValueError("UNADMITTED_NOT_DEFECT_ATTRIBUTION: reference access is not training approval")
    if any(f in _DEPENDENT_FIELDS for f in fields):
        raise ValueError("DEPENDENT_TARGET_NOT_SAVED: no utility/target materialization")
    # All policy, exact request and declared lineage checks precede payload IO.
    # The byte pin precedes JSON decoding, projection, and semantic provenance.
    sample = Path(SAMPLE_PATH)
    try:
        if sample.resolve(strict=True) != sample:
            raise ValueError("EXACT_PATH: redirected sample path")
    except OSError as exc:
        raise ValueError("EXACT_PATH: sample unavailable") from exc
    raw = _read_pinned(sample, SAMPLE_BYTES, SAMPLE_SHA256, "SAMPLE_PIN")
    data = json.loads(raw)
    _validate_sample(data, expected)
    rows = []
    for index in range(max_rows):
        row = {}
        for field in fields:
            layer, column = field.split(".")
            if column not in data[layer][index]:
                raise ValueError("SAMPLE_SHAPE: requested field absent")
            row[field] = data[layer][index][column]
        rows.append(MappingProxyType(row))
    status = {
        f: ("ECONOMIC_QUARANTINED_UNADMITTED" if f in quarantined else
            "NOT_ATTRIBUTED_TO_THESE_DEFECTS_UNADMITTED" if f.startswith("counterfactual_trade_v3.")
            else "OBSERVATION_REFERENCE_UNADMITTED") for f in fields
    }
    return ReferenceSample(tuple(rows), MappingProxyType(status))


def load_saved_sample(*, sample_path, fields, lineage, split, use, max_rows=128):
    """Read only audit references; calibration/teacher/target/training always fail.

    fields are exact ``layer.column`` names in the finite vocabulary above.
    ``dependent_targets.*`` names are denial selectors, not computed columns.
    The saved JSON is decoded in full (128 rows/layer); max_rows bounds the
    returned prefix, not JSON parser work. No original parquet is opened.
    """
    return _load(sample_path=sample_path, fields=fields, lineage=lineage,
                 split=split, use=use, max_rows=max_rows)


def inventory_saved_observations(*, sample_path, fields, lineage, split, max_rows=128):
    """Separate observation-only access, preserving unadmitted/censored values.

    These are saved legacy observations, not independently authenticated raw
    transaction bytes; this function makes no source-truth certification.
    """
    return _load(sample_path=sample_path, fields=fields, lineage=lineage,
                 split=split, use="raw_observation_inventory", max_rows=max_rows)
