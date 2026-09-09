"""Synthetic clock fixtures, not recovered media or generated transcript words."""
import importlib.util
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


def api():
    path = ROOT / "src/north_star/media_clock.py"
    assert path.exists(), "media clock validator not implemented"
    spec = importlib.util.spec_from_file_location("media_clock_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def request():
    return {
        "interval": {"media_id": "synthetic-A", "start_tick": 90_000,
                     "end_tick": 180_000, "origin_pts_tick": 9_000_000,
                     "ticks_per_second": 90_000, "discontinuity_epoch": 3},
        "segments": [{"media_sequence": 400, "discontinuity_epoch": 3,
                      "pts_tick": 9_000_000, "duration_ticks": 900_000}],
        "anchors": [],
        "last_observed_utc_us": 1_030_000_000,
        "asr_completed_utc_us": 1_040_000_000,
    }


def anchor(**changes):
    return dict({"media_sequence": 400, "discontinuity_epoch": 3,
                 "pts_tick": 9_000_000, "program_date_time_utc_us": 1_000_000_000,
                 "uncertainty_us": 20_000, "verified": True,
                 "evidence_id": "synthetic-manifest:400"}, **changes)


def test_missing_anchor_preserves_relative_interval_without_launcher_fallback():
    args = request()
    result = api().map_interval(**args)
    assert result["epoch_status"] == "UNRESOLVED"
    assert result["clock"] == "RELATIVE_ONLY"
    assert result["null_reason"] == "MISSING_VERIFIED_ANCHOR"
    assert result["interval"] == args["interval"]
    assert result["utc_interval_us"] is None
    assert result["available_at_utc_us"] == 1_040_000_000


def test_midstream_origin_uses_pts_delta_not_zero_or_observation_time():
    args = request()
    args["anchors"] = [anchor()]
    result = api().map_interval(**args)
    assert result["epoch_status"] == "RESOLVED"
    assert result["utc_interval_us"] == {
        "start_lower": 1_000_980_000, "start_upper": 1_001_020_000,
        "end_lower": 1_001_980_000, "end_upper": 1_002_020_000}
    assert result["available_at_utc_us"] == 1_040_000_000
    args["last_observed_utc_us"] = 1_080_000_000
    buffered = api().map_interval(**args)
    assert buffered["utc_interval_us"] == result["utc_interval_us"]
    assert buffered["available_at_utc_us"] == 1_080_000_000


@pytest.mark.parametrize("changes", [{"verified": False}, {"verified": "true"},
                                    {"evidence_id": ""}, {"media_sequence": 399},
                                    {"pts_tick": 9_000_001}, {"discontinuity_epoch": 2}])
def test_untrusted_or_unbound_anchor_is_not_used(changes):
    args = request()
    args["anchors"] = [anchor(**changes)]
    result = api().map_interval(**args)
    assert result["utc_interval_us"] is None
    assert result["null_reason"] == "UNTRUSTED_OR_UNBOUND_ANCHOR"


@pytest.mark.parametrize("kind,reason", [
    ("cross", "CROSS_DISCONTINUITY"), ("gap", "AMBIGUOUS_SEGMENT_MAPPING"),
    ("overlap", "AMBIGUOUS_SEGMENT_MAPPING"),
    ("duplicate", "AMBIGUOUS_SEGMENT_MAPPING"),
    ("sequence_gap", "AMBIGUOUS_SEGMENT_MAPPING"),
    ("outside", "AMBIGUOUS_SEGMENT_MAPPING"),
    ("wrong_epoch", "CROSS_DISCONTINUITY"),
])
def test_requires_single_contiguous_sequence_epoch(kind, reason):
    args = request()
    args["anchors"] = [anchor()]
    args["interval"]["end_tick"] = 990_000
    second = dict(args["segments"][0], media_sequence=401, pts_tick=9_900_000)
    args["segments"].append(second)
    if kind == "cross":
        second["discontinuity_epoch"] = 4
    elif kind == "gap":
        second["pts_tick"] += 1
    elif kind == "overlap":
        second["pts_tick"] -= 1
    elif kind == "duplicate":
        args["segments"].append(dict(second))
    elif kind == "sequence_gap":
        second["media_sequence"] = 402
    elif kind == "outside":
        args["interval"]["end_tick"] = 2_000_000
    elif kind == "wrong_epoch":
        args["interval"]["discontinuity_epoch"] = 2
    result = api().map_interval(**args)
    assert result["utc_interval_us"] is None
    assert result["null_reason"] == reason


@pytest.mark.parametrize("shift,expected", [(0, "RESOLVED"), (30_000, "RESOLVED"),
                                          (40_001, "UNRESOLVED")])
def test_all_anchor_constraints_must_have_a_common_offset(shift, expected):
    args = request()
    args["segments"].append(dict(args["segments"][0], media_sequence=401,
                                 pts_tick=9_900_000))
    args["anchors"] = [anchor(), anchor(media_sequence=401, pts_tick=9_900_000,
        program_date_time_utc_us=1_010_000_000 + shift, evidence_id="synthetic:401")]
    result = api().map_interval(**args)
    assert result["epoch_status"] == expected
    if expected == "UNRESOLVED":
        assert result["null_reason"] == "CONTRADICTORY_ANCHORS"
        assert result["utc_interval_us"] is None
    else:
        assert result["utc_interval_us"]["start_lower"] == 1_000_980_000 + shift
    args["anchors"].reverse()
    args["segments"].reverse()
    assert api().map_interval(**args) == result


@pytest.mark.parametrize("field", ["last_observed_utc_us", "asr_completed_utc_us"])
def test_unknown_observation_or_asr_completion_never_invents_availability(field):
    args = request()
    args["anchors"] = [anchor()]
    args[field] = None
    result = api().map_interval(**args)
    assert result["epoch_status"] == "RESOLVED"
    assert result["available_at_utc_us"] is None
    assert result["availability_null_reason"] == "MISSING_OBSERVATION_OR_ASR_COMPLETION"


@pytest.mark.parametrize("field", ["last_observed_utc_us", "asr_completed_utc_us",
                                   "start_tick", "ticks_per_second", "duration_ticks",
                                   "uncertainty_us", "program_date_time_utc_us",
                                   "media_sequence", "discontinuity_epoch"])
@pytest.mark.parametrize("bad", [True, 1.5, "1", -1])
def test_units_are_exact_nonnegative_integers(field, bad):
    args = request()
    args["anchors"] = [anchor()]
    if field in args:
        args[field] = bad
    elif field in args["interval"]:
        args["interval"][field] = bad
    elif field in args["segments"][0]:
        args["segments"][0][field] = bad
    else:
        args["anchors"][0][field] = bad
    with pytest.raises(ValueError):
        api().map_interval(**args)


@pytest.mark.parametrize("kind", ["zero_rate", "empty_interval", "zero_duration"])
def test_zero_length_or_invalid_rate_rejected(kind):
    args = request()
    args["anchors"] = [anchor()]
    if kind == "zero_rate":
        args["interval"]["ticks_per_second"] = 0
    elif kind == "empty_interval":
        args["interval"]["end_tick"] = args["interval"]["start_tick"]
    else:
        args["segments"][0]["duration_ticks"] = 0
    with pytest.raises(ValueError):
        api().map_interval(**args)


def test_outward_rounding_keeps_fractional_microseconds_and_does_not_mutate():
    import copy
    args = request()
    args["anchors"] = [anchor(uncertainty_us=0)]
    args["interval"].update(start_tick=1, end_tick=2)
    before = copy.deepcopy(args)
    result = api().map_interval(**args)
    assert result["utc_interval_us"] == {"start_lower": 1_000_000_011,
        "start_upper": 1_000_000_012, "end_lower": 1_000_000_022,
        "end_upper": 1_000_000_023}
    assert args == before


def test_uncertainty_exceeding_window_boundary_fails_closed_and_waits_for_asr():
    args = request()
    args["anchors"] = [anchor()]
    module = api()
    result = module.map_interval(**args)
    assert not module.eligible_in_window(result, window_start_utc_us=1_000_990_000,
        window_end_utc_us=1_002_020_000, cutoff_utc_us=1_040_000_000)
    assert not module.eligible_in_window(result, window_start_utc_us=1_000_980_000,
        window_end_utc_us=1_002_019_999, cutoff_utc_us=1_040_000_000)
    assert not module.eligible_in_window(result, window_start_utc_us=1_000_980_000,
        window_end_utc_us=1_002_020_000, cutoff_utc_us=1_039_999_999)
    assert module.eligible_in_window(result, window_start_utc_us=1_000_980_000,
        window_end_utc_us=1_002_020_000, cutoff_utc_us=1_040_000_000)
    args["anchors"] = []
    assert not module.eligible_in_window(module.map_interval(**args),
        window_start_utc_us=0, window_end_utc_us=2_000_000_000,
        cutoff_utc_us=2_000_000_000)


def test_metadata_only_assessment_never_converts_launcher_or_probe_pts_to_epoch():
    module = api()
    context = {"capture_started_unix": 1788965406, "stream_id": "320254509148"}
    probe = {"epoch_status": "UNRESOLVED", "media_epoch_utc": None,
             "observed_at_pt": "2026-09-09T08:59:58.608539-07:00",
             "probe": {"format": {"format_name": "mpegts", "start_time": "1.467000",
                                  "duration": "4178.194311"}}}
    result = module.assess_capture_metadata(context, probe)
    assert result["epoch_status"] == "UNRESOLVED"
    assert result["media_epoch_utc_us"] is None
    assert result["available_at_utc_us"] is None
    assert result["null_reason"] == "MISSING_VERIFIED_ANCHOR"
    assert result["capture_started_unix_metadata_only"] == 1788965406
    assert result["format_metadata"] == probe["probe"]["format"]
    probe["epoch_status"] = "RESOLVED"
    probe["media_epoch_utc"] = "2026-09-09T14:50:06Z"
    assert module.assess_capture_metadata(context, probe)["epoch_status"] == "UNRESOLVED"


@pytest.mark.parametrize("field", ["program_date_time_utc_us", "uncertainty_us", "pts_tick"])
def test_incomplete_anchor_retains_relative_evidence(field):
    args = request()
    args["anchors"] = [anchor()]
    del args["anchors"][0][field]
    result = api().map_interval(**args)
    assert result["clock"] == "RELATIVE_ONLY"
    assert result["interval"] == args["interval"]
    assert result["null_reason"] == "UNTRUSTED_OR_UNBOUND_ANCHOR"


def test_segment_end_is_exclusive_and_no_asr_is_ineligible():
    args = request()
    args["anchors"] = [anchor()]
    args["interval"].update(start_tick=810_000, end_tick=900_000)
    args["asr_completed_utc_us"] = None
    module = api()
    result = module.map_interval(**args)
    assert result["epoch_status"] == "RESOLVED"
    assert not module.eligible_in_window(result, window_start_utc_us=0,
        window_end_utc_us=2_000_000_000, cutoff_utc_us=2_000_000_000)
