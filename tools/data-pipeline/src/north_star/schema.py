"""Initial North Star validation primitives, NOT full consumer schemas/admission.

No coercion: source rounded floats cannot become exact monetary truth by casting.
"""
import json
from collections.abc import Mapping
from pathlib import Path


REGISTRY_PATH = Path(__file__).resolve().parents[2] / "schemas/north_star/CANONICAL_REGISTRY.json"


def load_registry():
    """Read the versioned local specification, not source data."""
    return json.loads(REGISTRY_PATH.read_text(encoding="utf-8"))


def validate_identity(row, *, table):
    """Return the exact composite key; no alias/Unicode or numeric coercion.

    Checks only identity, not complete row schema, uniqueness or referenced rows.
    Integer revision zero is allowed; versions are positive integers or exact
    nonempty version tokens. Unknown token availability cannot form its UTC key.
    """
    if not isinstance(row, Mapping):
        raise ValueError("row must be a mapping")
    registry = load_registry()
    tables = {**registry["canonical_tables"], **registry["supplemental_objects"],
              **registry["supporting_objects"]}
    if not isinstance(table, str) or table not in tables:
        raise ValueError("unknown table")
    keys = tables[table]["primary_key"]
    for key in ["schema_version", *keys]:
        value = row.get(key)
        if key in {"available_at", "revision"}:
            valid = type(value) is int and value >= 0
        elif key in {"version", "split_version"}:
            valid = (type(value) is int and value > 0) or (isinstance(value, str) and bool(value.strip()))
        else:
            valid = isinstance(value, str) and bool(value.strip())
        if not valid:
            raise ValueError(f"missing or invalid identity field: {key}")
    return tuple(row[key] for key in keys)


def validate_null_reason(value, reason):
    """Reject unexplained nulls and reasons attached to non-null values."""
    allowed = load_registry()["contracts"]["canonical_envelope_v1"]["null_reasons"]["allowed"]
    if value is None:
        if not isinstance(reason, str) or reason not in allowed:
            raise ValueError("null value requires a registered null reason")
    elif reason is not None:
        raise ValueError("present value cannot have a null reason")


def validate_native_amount(value, *, unit, decimals=None):
    """Validate an unsigned on-chain u64 amount in named native units."""
    if type(value) is not int or not 0 <= value < 2**64:
        raise ValueError("native amount must be an exact unsigned 64-bit integer")
    if unit not in {"lamport", "raw_token"}:
        raise ValueError("unit must be lamport or raw_token")
    if unit == "raw_token":
        if type(decimals) is not int or not 0 <= decimals <= 255:
            raise ValueError("raw_token requires verified mint decimals (u8)")
    elif decimals is not None:
        raise ValueError("lamport has no configurable mint decimals")
    return value
