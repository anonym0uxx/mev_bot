"""Synthetic software fixtures; timestamps do not represent real source events."""
import importlib.util
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


def load_availability():
    path = ROOT / "src/north_star/availability.py"
    assert path.exists(), "availability primitive not implemented"
    spec = importlib.util.spec_from_file_location("north_star_availability_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def dep(at, clock="UTC_KNOWN", domain=None):
    return {"available_at": at, "clock": clock, "clock_domain": domain}


def test_each_field_uses_own_dependencies_and_delay_including_late_link():
    api = load_availability()
    actual = api.derive_field_availability(
        {"price": {"dependencies": ["quote"], "compute_delay_ms": 3},
         "linked_claim": {"dependencies": ["text", "link"], "compute_delay_ms": 7}},
        {"quote": dep(100), "text": dep(80), "link": dep(250)},
    )
    assert actual["price"]["available_at"] == 103
    assert actual["linked_claim"]["available_at"] == 257
    assert api.is_available_at(actual["price"], 103)
    assert not api.is_available_at(actual["linked_claim"], 103)
    assert actual["price"]["dependencies"] == ["quote"]
    assert actual["linked_claim"]["compute_delay_ms"] == 7


@pytest.mark.parametrize("dependencies,expected", [
    ({"a": dep(None, "UNKNOWN")}, "CLOCK_UNKNOWN"),
    ({"a": dep(None)}, "CLOCK_UNKNOWN"),
    ({}, "MISSING_DEPENDENCY"),
])
def test_unknown_or_missing_clock_fails_closed(dependencies, expected):
    api = load_availability()
    result = api.compute_available_at(["a"], dependencies, compute_delay_ms=0)
    assert result["available_at"] is None
    assert result["clock"] == "UNKNOWN"
    assert result["null_reason"] == expected
    assert not api.is_available_at(result, 999999)


def test_relative_only_never_becomes_utc_or_cross_session():
    api = load_availability()
    deps = {"a": dep(20, "RELATIVE_ONLY", "session-A"), "b": dep(40, "RELATIVE_ONLY", "session-A")}
    result = api.compute_available_at(["a", "b"], deps, compute_delay_ms=2)
    assert result["available_at"] == 42
    assert result["clock"] == "RELATIVE_ONLY"
    assert not api.is_available_at(result, 100)
    assert api.is_available_at(result, 42, clock="RELATIVE_ONLY", clock_domain="session-A")
    assert not api.is_available_at(result, 42, clock="RELATIVE_ONLY", clock_domain="session-B")
    deps["b"] = dep(40, "RELATIVE_ONLY", "session-B")
    assert api.compute_available_at(["a", "b"], deps, compute_delay_ms=0)["null_reason"] == "CLOCK_DOMAIN_MISMATCH"
    deps["b"] = dep(40)
    assert api.compute_available_at(["a", "b"], deps, compute_delay_ms=0)["available_at"] is None


@pytest.mark.parametrize("delay", [-1, True, 1.5, "1", None])
def test_delay_requires_exact_nonnegative_integer(delay):
    with pytest.raises(ValueError):
        load_availability().compute_available_at(["a"], {"a": dep(10)}, compute_delay_ms=delay)


@pytest.mark.parametrize("bad", [dep(-1), dep(True), dep(1.5), dep("1"), dep(1, "guessed"), dep(1, "RELATIVE_ONLY"), dep(1, "UNKNOWN"), dep(1, "UTC_KNOWN", "session-A")])
def test_invalid_clock_metadata_is_not_coerced(bad):
    with pytest.raises(ValueError):
        load_availability().compute_available_at(["a"], {"a": bad}, compute_delay_ms=0)


def test_empty_dependency_set_cannot_invent_available_time():
    api = load_availability()
    result = api.compute_available_at([], {}, compute_delay_ms=0)
    assert result["null_reason"] == "MISSING_DEPENDENCY"
    assert not api.is_available_at(result, 0)


def test_delay_is_required_per_field_and_cutoff_is_validated():
    api = load_availability()
    with pytest.raises(ValueError):
        api.derive_field_availability({"x": {"dependencies": ["a"]}}, {"a": dep(1)})
    for cutoff in [-1, True, 1.5, "1", None]:
        with pytest.raises(ValueError):
            api.is_available_at(dep(1), cutoff)
