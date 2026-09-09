"""Synthetic engineering fixtures only; no source rows or training data."""
import importlib.util
import json
import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


def load_schema():
    path = ROOT / "src/north_star/schema.py"
    assert path.exists(), "canonical schema primitive not implemented"
    spec = importlib.util.spec_from_file_location("north_star_schema_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_native_money_preserves_exact_u64_boundary():
    schema = load_schema()
    assert schema.validate_native_amount(2**64 - 1, unit="lamport") == 2**64 - 1
    assert schema.validate_native_amount(0, unit="raw_token", decimals=6) == 0
    for value in [True, 1.0, "1", -1, 2**64]:
        with pytest.raises(ValueError):
            schema.validate_native_amount(value, unit="lamport")


def test_registry_enumerates_master_without_claiming_coverage():
    path = ROOT / "schemas/north_star/CANONICAL_REGISTRY.json"
    assert path.exists(), "canonical registry not implemented"
    registry = json.loads(path.read_text(encoding="utf-8"))
    master = (ROOT / "north_star/master/NORTH_STAR_WINDOWS_MASTER_V5.md").read_text(encoding="utf-8")
    inventory = master.split("Canonical tables, primary keys and join direction:")[1].split("**Multi-parent")[0]
    names = re.findall(r"`([a-z_]+)\(", inventory)
    assert len(names) == 20
    assert set(registry["canonical_tables"]) == set(names)
    objects = master.split("## Corpus objects and production order")[1].split("Dependency order:")[0]
    supplemental = re.findall(r"`([a-z_]+_v1)`", objects)
    assert len(supplemental) == 8
    assert set(registry["supplemental_objects"]) == set(supplemental)
    assert "coverage_intervals" in registry["supporting_objects"]
    for group in ["canonical_tables", "supplemental_objects", "supporting_objects"]:
        for entry in registry[group].values():
            assert entry["primary_key"]
            assert entry["common_contract"] == "canonical_envelope_v1"
            assert entry["consumer_schema_status"] == "INCOMPLETE"
            assert entry["corpus_coverage"] is None
            assert "dependencies" in entry
    contracts = registry["contracts"]["canonical_envelope_v1"]
    assert {"identity", "provenance", "null_reasons", "time", "units", "dependencies"} <= set(contracts)
    bridge = registry["supplemental_objects"]["rust_market_bridge_episode_v1"]
    assert bridge["namespace"] == "engineering_evidence"
    assert bridge["qwen_sft_eligible"] is False
    assert bridge["requirement_ids"] == ["D14"]


def test_versioned_identity_required_and_literal():
    schema = load_schema()
    row = {"source_id": "pro6per (prosper)", "version": 1, "schema_version": "sources_v1"}
    assert schema.validate_identity(row, table="sources") == ("pro6per (prosper)", 1)
    for key in row:
        with pytest.raises(ValueError):
            schema.validate_identity({k: v for k, v in row.items() if k != key}, table="sources")
    for key, bad in [("version", True), ("version", 0), ("source_id", ""), ("schema_version", None)]:
        with pytest.raises(ValueError):
            schema.validate_identity(dict(row, **{key: bad}), table="sources")
    with pytest.raises(ValueError, match="unknown table"):
        schema.validate_identity(row, table="not_canonical")


def test_composite_identity_and_revision_zero():
    schema = load_schema()
    token = {"chain": "solana", "mint": "EXACT_SYNTHETIC_MINT", "available_at": 0,
             "version": 1, "schema_version": "token_versions_v1"}
    assert schema.validate_identity(token, table="token_versions") == ("solana", "EXACT_SYNTHETIC_MINT", 0, 1)
    for value in [None, -1, True, 1.1, "123"]:
        with pytest.raises(ValueError):
            schema.validate_identity(dict(token, available_at=value), table="token_versions")
    assert schema.validate_identity({"event_id": "SYNTHETIC", "revision": 0, "schema_version": "events_v1"}, table="events") == ("SYNTHETIC", 0)


@pytest.mark.parametrize("kwargs", [
    {"unit": "SOL"}, {"unit": "raw_token"},
    {"unit": "raw_token", "decimals": True},
    {"unit": "raw_token", "decimals": -1},
    {"unit": "raw_token", "decimals": 256},
    {"unit": "lamport", "decimals": 9},
])
def test_no_ambiguous_unit_or_decimals(kwargs):
    with pytest.raises(ValueError):
        load_schema().validate_native_amount(1, **kwargs)


def test_null_reason_is_explicit_and_not_attached_to_present_value():
    schema = load_schema()
    schema.validate_null_reason(None, "SOURCE_OUTAGE")
    schema.validate_null_reason(0, None)
    for value, reason in [(None, None), (None, ""), (None, "invented"), (0, "UNKNOWN")]:
        with pytest.raises(ValueError):
            schema.validate_null_reason(value, reason)
