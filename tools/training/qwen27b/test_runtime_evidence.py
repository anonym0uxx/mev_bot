"""Runtime evidence: the emit must be best-effort and must never alter training."""
import importlib.util
import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

spec = importlib.util.spec_from_file_location("trainer", HERE / "train_qwen27b.py")
trainer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(trainer)


class FakeArgs:
    phase = "cpt"
    release_manifest = "/training/rel/CANDIDATE_RELEASE.json"
    runtime_evidence_out = "/tmp/rt.json"
    world_size = 3


class FakeTrainerArgs:
    world_size = 3
    process_index = 0


class FakeState:
    global_step = 23
    log_history = [
        {"loss": 1.2, "step": 10},
        {"eval_loss": 0.87, "eval_loss_tokens": 137004, "step": 23},
    ]


class FakeAccelState:
    class deepspeed_plugin:
        zero_stage = 3


class FakeAccelerator:
    state = FakeAccelState()


class FakeTrainer:
    args = FakeTrainerArgs()
    state = FakeState()
    accelerator = FakeAccelerator()

    def denom_stats_summary(self):
        return {"windows_sampled": 3, "weighted_over_raw_ratio": {"p50": 1.5}}


def test_collect_reports_world_size_and_zero_stage():
    payload = trainer.collect_runtime_evidence(
        FakeTrainer(), {"release_id": "astra_north_star_v3"}, FakeArgs(), False, 23)
    assert payload["schema"] == "qwen27b_runtime_evidence_v1"
    assert payload["world_size"] == 3
    assert payload["deepspeed_zero_stage"] == 3
    assert payload["native_deepspeed"] is True
    assert payload["steps_completed"] == 23
    assert payload["release_id"] == "astra_north_star_v3"


def test_collect_picks_the_last_validation_mass():
    payload = trainer.collect_runtime_evidence(
        FakeTrainer(), {}, FakeArgs(), False, 23)
    assert payload["validation_loss_token_mass"] == 137004
    assert payload["validation_loss"] == 0.87


def test_collect_survives_a_trainer_without_deepspeed():
    class NoDS(FakeTrainer):
        class accelerator:
            class state:
                pass
    payload = trainer.collect_runtime_evidence(NoDS(), {}, FakeArgs(), False, 23)
    assert payload["deepspeed_zero_stage"] is None
    assert payload["native_deepspeed"] is False


def test_write_is_atomic_and_round_trips(tmp_path):
    target = tmp_path / "rt.json"
    assert trainer.write_runtime_evidence(str(target), {"a": 1}) is True
    assert json.loads(target.read_text(encoding="utf-8")) == {"a": 1}
    assert not (tmp_path / "rt.json.tmp").exists()


def test_write_never_raises_on_an_unwritable_path():
    """An evidence failure must not be able to kill a training run."""
    assert trainer.write_runtime_evidence(
        "/nonexistent-dir-xyz/rt.json", {"a": 1}) is False


def test_write_never_raises_on_unserialisable_payload(tmp_path):
    class Unserialisable:
        pass
    assert trainer.write_runtime_evidence(
        str(tmp_path / "rt.json"), {"bad": Unserialisable()}) is False
