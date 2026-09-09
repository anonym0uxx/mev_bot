"""Pure per-field availability arithmetic; no clock inference or raw parsing.

Dependency keys must identify immutable field versions in the caller's registry.
This module trusts supplied dependency metadata, not its source authenticity.
No timestamp comparison alone proves same-time causal event ordering.
"""
from collections.abc import Mapping


CLOCKS = {"UTC_KNOWN", "RELATIVE_ONLY", "UNKNOWN"}


def _millis(value, name):
    if type(value) is not int or value < 0:
        raise ValueError(f"{name} must be a nonnegative integer millisecond value")


def _clock(record):
    if not isinstance(record, Mapping):
        raise ValueError("clock metadata must be a mapping")
    clock = record.get("clock")
    domain = record.get("clock_domain")
    at = record.get("available_at")
    if not isinstance(clock, str) or clock not in CLOCKS:
        raise ValueError("clock must be explicitly known, relative-only, or unknown")
    if clock == "RELATIVE_ONLY":
        if not isinstance(domain, str) or not domain.strip():
            raise ValueError("relative clock requires an exact domain/session")
    elif domain is not None:
        raise ValueError("only relative clocks may carry a clock_domain")
    if at is not None:
        _millis(at, "available_at")
        if clock == "UNKNOWN":
            raise ValueError("unknown clock cannot claim a known available_at")
    return clock, domain, at


def compute_available_at(dependency_ids, dependencies, *, compute_delay_ms):
    """Compute max of referenced available times plus this field's delay.

    Returns UNKNOWN/null for empty/missing/unknown or mixed-domain inputs.
    Relative-only output can be used only with an explicit matching relative
    cutoff, never as an absolute chronological training/evaluation timestamp.
    Dependencies are pre-resolved; this function does not traverse a graph.
    """
    _millis(compute_delay_ms, "compute_delay_ms")
    if not isinstance(dependency_ids, (list, tuple)) or any(
        not isinstance(key, str) or not key.strip() for key in dependency_ids
    ):
        raise ValueError("dependency_ids must be a list/tuple of exact field references")
    if not isinstance(dependencies, Mapping):
        raise ValueError("dependencies must be a mapping")
    result = {"available_at": None, "clock": "UNKNOWN", "clock_domain": None,
              "null_reason": None, "dependencies": list(dependency_ids),
              "compute_delay_ms": compute_delay_ms}
    if not dependency_ids or any(key not in dependencies for key in dependency_ids):
        return dict(result, null_reason="MISSING_DEPENDENCY")
    clocks = [_clock(dependencies[key]) for key in dependency_ids]
    if any(clock == "UNKNOWN" or at is None for clock, domain, at in clocks):
        return dict(result, null_reason="CLOCK_UNKNOWN")
    if len({(clock, domain) for clock, domain, at in clocks}) != 1:
        return dict(result, null_reason="CLOCK_DOMAIN_MISMATCH")
    clock, domain, _ = clocks[0]
    return dict(result, available_at=max(at for _, _, at in clocks) + compute_delay_ms,
                clock=clock, clock_domain=domain)


def derive_field_availability(fields, dependencies):
    """Resolve each field against its own explicit dependency set and delay."""
    if not isinstance(fields, Mapping):
        raise ValueError("fields must be a mapping")
    result = {}
    for field, contract in fields.items():
        if not isinstance(field, str) or not field.strip() or not isinstance(contract, Mapping):
            raise ValueError("field contract must have a name and a mapping")
        if "dependencies" not in contract or "compute_delay_ms" not in contract:
            raise ValueError("each field requires dependencies and compute_delay_ms")
        result[field] = compute_available_at(contract["dependencies"], dependencies,
                                            compute_delay_ms=contract["compute_delay_ms"])
    return result


def is_available_at(availability, cutoff_ms, *, clock="UTC_KNOWN", clock_domain=None):
    """Timing predicate only, not source rights/split/training admission."""
    _millis(cutoff_ms, "cutoff_ms")
    query_clock, query_domain, _ = _clock(
        {"clock": clock, "clock_domain": clock_domain,
         "available_at": None if clock == "UNKNOWN" else cutoff_ms}
    )
    value_clock, value_domain, at = _clock(availability)
    if query_clock == "UNKNOWN" or value_clock == "UNKNOWN" or at is None:
        return False
    return (query_clock, query_domain) == (value_clock, value_domain) and at <= cutoff_ms
