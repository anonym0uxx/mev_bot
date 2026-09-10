"""Bounded saved-sample enforcement; no parquet/corpus access or source writes."""
import importlib
import io
from pathlib import Path

import pytest

SAMPLE = "D:/mev_bot-artifacts/north_star/development/slinky_economics_audit_v1/development_samples.json"
LINEAGE = {
    "source": "slinky21", "run_uuid": "ef113bd1-f5e",
    "pipeline_version": "3.1.0", "source_hash": "42132c2533b9effd",
    "code_config_hash": "af87ffefd9988a42",
    "execution_config_hash": "89207735e3c0f866",
    "producer_sha256": "db2530be539a1cfb03f01aa096e74a88d4eb920c458b0e13e029e25379722034",
}
ECON = "counterfactual_trade_v3.sz005_net_return_bp"


def api():
    assert importlib.util.find_spec("north_star.economic_quarantine") is not None, "quarantine loader missing"
    return importlib.import_module("north_star.economic_quarantine")


def request(**changes):
    values = dict(sample_path=SAMPLE, fields=(ECON,), lineage=dict(LINEAGE),
                  split="DEVELOPMENT_EXPOSED", use="target")
    values.update(changes)
    return values


def guard_payload(monkeypatch):
    opened = []
    original = io.open
    def guarded(file, *args, **kwargs):
        if str(file).replace("\\", "/") == SAMPLE:
            opened.append(str(file))
            pytest.fail("payload opened before refusal")
        return original(file, *args, **kwargs)
    monkeypatch.setattr(io, "open", guarded)
    return opened


def test_real_economic_target_refused_before_payload_open(monkeypatch):
    module = api()
    opened = guard_payload(monkeypatch)
    with pytest.raises(ValueError, match="ECONOMIC_QUARANTINE"):
        module.load_saved_sample(**request())
    assert opened == []


@pytest.mark.parametrize("use", ["calibration", "teacher", "target", "training"])
@pytest.mark.parametrize("field", [ECON, "counterfactual_trade_v3.sz100_exit_sol",
    "counterfactual_trade_v3.sz050_net_pnl_sol", "counterfactual_trade_v3.exact_net_return_bp",
    "counterfactual_trade_v3.sz005_feasible", "dependent_targets.utility",
    "dependent_targets.relative_rank", "dependent_targets.recommended_action"])
def test_economic_and_dependent_uses_fail_before_payload(monkeypatch, use, field):
    module = api()
    guard_payload(monkeypatch)
    with pytest.raises(ValueError, match="ECONOMIC_QUARANTINE"):
        module.load_saved_sample(**request(fields=(field,), use=use))


@pytest.mark.parametrize("changes,reason", [
    ({"fields": ("counterfactual_trade_v3.sz005_net_fake",)}, "UNKNOWN_FIELD"),
    ({"fields": ("counterfactual_trade_v3.*",)}, "UNKNOWN_FIELD"),
    ({"fields": ("../sz005_net_return_bp",)}, "UNKNOWN_FIELD"),
    ({"fields": (ECON, ECON)}, "FIELD_REQUEST"),
    ({"fields": ()}, "FIELD_REQUEST"),
    ({"fields": ECON}, "FIELD_REQUEST"),
    ({"fields": (None,)}, "FIELD_REQUEST"),
    ({"fields": (ECON, "pump_state_v3.no_such_field")}, "UNKNOWN_FIELD"),
    ({"sample_path": SAMPLE.replace("development/", "protected_future/")}, "EXACT_PATH"),
    ({"sample_path": SAMPLE.replace("development_samples", "./development_samples")}, "EXACT_PATH"),
    ({"sample_path": SAMPLE + ":stream"}, "EXACT_PATH"),
    ({"split": "PROTECTED_FUTURE"}, "PROTECTED_SPLIT"),
    ({"split": "DEVELOPMENT"}, "PROTECTED_SPLIT"),
    ({"use": "approved"}, "UNKNOWN_USE"),
    ({"max_rows": 129}, "ROW_BOUND"),
    ({"max_rows": 0}, "ROW_BOUND"),
    ({"max_rows": True}, "ROW_BOUND"),
])
def test_requests_failclosed_before_payload(monkeypatch, changes, reason):
    module = api()
    guard_payload(monkeypatch)
    with pytest.raises(ValueError, match=reason):
        module.load_saved_sample(**request(**changes))


@pytest.mark.parametrize("key", list(LINEAGE))
def test_changed_requested_producer_lineage_fails_before_payload(monkeypatch, key):
    module = api()
    guard_payload(monkeypatch)
    lineage = dict(LINEAGE, **{key: "unknown"})
    with pytest.raises(ValueError, match="UNKNOWN_LINEAGE"):
        module.load_saved_sample(**request(lineage=lineage, use="audit_reference"))


@pytest.mark.parametrize("lineage", [None, {}, {**LINEAGE, "approved": True}])
def test_missing_or_extra_lineage_failsclosed(monkeypatch, lineage):
    module = api()
    guard_payload(monkeypatch)
    with pytest.raises(ValueError, match="UNKNOWN_LINEAGE"):
        module.load_saved_sample(**request(lineage=lineage))


def substitute_read(monkeypatch, path, transform):
    original = io.open
    def opened(file, *args, **kwargs):
        handle = original(file, *args, **kwargs)
        if Path(file) == Path(path):
            with handle:
                return io.BytesIO(transform(handle.read()))
        return handle
    monkeypatch.setattr(io, "open", opened)


def test_tampered_registry_checked_even_before_bad_request(monkeypatch):
    module = api()
    guard_payload(monkeypatch)
    substitute_read(monkeypatch, module.REGISTRY_PATH, lambda b: b.replace(b"QUARANTINED", b"QUARANTINEd", 1))
    with pytest.raises(ValueError, match="REGISTRY_PIN"):
        module.load_saved_sample(**request(fields=("unknown",)))


def test_sample_redirect_to_protected_path_refused_before_open(monkeypatch):
    module = api()
    guard_payload(monkeypatch)
    original = Path.resolve
    def resolve(path, *args, **kwargs):
        if path == Path(SAMPLE):
            return Path("D:/mev_bot-artifacts/north_star/protected_future/development_samples.json")
        return original(path, *args, **kwargs)
    monkeypatch.setattr(Path, "resolve", resolve)
    with pytest.raises(ValueError, match="EXACT_PATH"):
        module.load_saved_sample(**request(use="audit_reference"))


def test_sample_tampering_refused_before_decode(monkeypatch):
    module = api()
    substitute_read(monkeypatch, SAMPLE, lambda b: b.replace(b"ef113bd1-f5e", b"ef113bd1-f5x", 1))
    original = module.json.loads
    def decode(b, *args, **kwargs):
        assert len(b) < 100000, "sample decoded before hash check"
        return original(b, *args, **kwargs)
    monkeypatch.setattr(module.json, "loads", decode)
    with pytest.raises(ValueError, match="SAMPLE_PIN"):
        module.load_saved_sample(**request(use="audit_reference"))


def test_bounded_audit_preserves_original_bytes_and_field_scope():
    module = api()
    before = Path(SAMPLE).read_bytes()
    fields = (ECON, "counterfactual_trade_v3.net_return_bp",
              "counterfactual_trade_v3.sz050_exit_sol", "pump_outcome_v3.observed_through_300s")
    result = module.load_saved_sample(**request(fields=fields, use="audit_reference"))
    assert result.disposition == "REFERENCE_ONLY_ALLOWED"
    assert result.training_approved is False and result.admitted is False
    assert result.economic_certified is False and result.fit_authorized is False
    assert result.sample_sha256 == "d1068309e3c16053d32c7ba55f8fcb7e6d44851c4ca8c03004331716f6054ae7"
    assert result.registry_sha256 == "0019b3455d9c6aebcafa85ee5fe0139aa9933583089a4bbd4d4d001dc62a4c9b"
    assert len(result.rows) == 128
    saved = module.json.loads(before)
    for index, row in enumerate(result.rows):
        assert set(row) == set(fields)
        for field in fields:
            layer, column = field.split(".")
            assert row[field] == saved[layer][index][column]
    assert result.field_status[ECON] == "ECONOMIC_QUARANTINED_UNADMITTED"
    assert result.field_status[fields[1]] == "NOT_ATTRIBUTED_TO_THESE_DEFECTS_UNADMITTED"
    assert result.field_status[fields[2]] == "NOT_ATTRIBUTED_TO_THESE_DEFECTS_UNADMITTED"
    assert result.field_status[fields[3]] == "OBSERVATION_REFERENCE_UNADMITTED"
    assert Path(SAMPLE).read_bytes() == before
    with pytest.raises(TypeError):
        result.rows[0][ECON] = 0


def test_raw_inventory_is_separate_unadmitted_and_bounded():
    module = api()
    kwargs = request(fields=("pump_state_v3.state_id", "pump_state_v3.trade_side",
                             "pump_state_v3.v_sol_bonding_curve_lamports"), max_rows=3)
    del kwargs["use"]
    result = module.inventory_saved_observations(**kwargs)
    assert len(result.rows) == 3
    assert result.disposition == "REFERENCE_ONLY_ALLOWED"
    assert result.admitted is False and result.training_approved is False
    assert set(result.field_status.values()) == {"OBSERVATION_REFERENCE_UNADMITTED"}


@pytest.mark.parametrize("field", [ECON, "counterfactual_trade_v3.net_return_bp", "dependent_targets.utility"])
def test_raw_inventory_cannot_launder_targets(monkeypatch, field):
    module = api()
    guard_payload(monkeypatch)
    kwargs = request(fields=(field,))
    del kwargs["use"]
    with pytest.raises(ValueError, match="RAW_OBSERVATION_ONLY"):
        module.inventory_saved_observations(**kwargs)


def test_unaffected_barrier_not_certified_or_blanket_economically_invalid(monkeypatch):
    module = api()
    guard_payload(monkeypatch)
    with pytest.raises(ValueError, match="UNADMITTED_NOT_DEFECT_ATTRIBUTION"):
        module.load_saved_sample(**request(fields=("counterfactual_trade_v3.net_return_bp",)))


@pytest.mark.parametrize("key", ["allow_quarantined", "training_approved", "registry_sha256", "sample_sha256", "registry_path"])
def test_no_caller_pin_or_approval_overrides(key):
    module = api()
    with pytest.raises(TypeError):
        module.load_saved_sample(**request(**{key: True}))


@pytest.mark.parametrize("layer,column,value", [
    ("pump_state_v3", "code_config_hash", "changed"),
    ("pump_state_v3", "source_hash", "changed"),
    ("counterfactual_trade_v3", "execution_config_hash", "changed"),
    ("pump_outcome_v3", "run_uuid", "changed"),
    ("pump_outcome_v3", "state_id", "different_join"),
])
def test_decoded_lineage_and_join_failclosed(monkeypatch, layer, column, value):
    # Defensive semantic validation is tested independently of the byte pin.
    module = api()
    original = module.json.loads
    def decoded(b, *args, **kwargs):
        result = original(b, *args, **kwargs)
        if "pump_state_v3" in result:
            result[layer][0][column] = value
        return result
    monkeypatch.setattr(module.json, "loads", decoded)
    with pytest.raises(ValueError, match="SAMPLE_LINEAGE|SAMPLE_JOIN"):
        module.load_saved_sample(**request(use="audit_reference"))
