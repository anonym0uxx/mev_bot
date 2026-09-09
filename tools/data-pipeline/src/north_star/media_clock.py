"""Pure single-source, single-discontinuity-epoch media clock validator.

Caller supplies a verified contiguous segment window and unwrapped PTS at one
explicit ticks_per_second rate. Anchors are PDT at the exact segment-start PTS;
evidence_id is a caller-verified evidence reference, NOT authenticity proof here.
Uncertainty must bound total clock error/drift over the supplied window. No PTS
wrap recovery, launcher-time inference, media parsing, network, ASR, or I/O.
UTC values are integer Unix microseconds; relative intervals are [start, end).
"""
from collections.abc import Mapping
from copy import deepcopy


def _integer(value, name, minimum=0):
    if type(value) is not int or value < minimum:
        raise ValueError(f"{name} must be an exact integer >= {minimum}")


def _fields(record, names):
    if not isinstance(record, Mapping):
        raise ValueError("clock record must be a mapping")
    for name in names:
        _integer(record.get(name), name)


def map_interval(*, interval, segments, anchors, last_observed_utc_us,
                 asr_completed_utc_us):
    """Retain relative evidence/reason on unresolved clocks; bad units raise.

    last_observed is the latest observation of all media used by this transcript
    version, not the capture launcher or an arbitrary later health probe. ASR
    completion is required even for historically recorded speech. Both UTC
    availability inputs must already be verified conservative clock values.
    """
    _fields(interval, ("origin_pts_tick", "start_tick", "end_tick",
                       "ticks_per_second", "discontinuity_epoch"))
    _integer(interval["ticks_per_second"], "ticks_per_second", 1)
    if interval["end_tick"] <= interval["start_tick"]:
        raise ValueError("relative interval must be nonempty and ordered")
    if not isinstance(segments, (list, tuple)) or not isinstance(anchors, (list, tuple)):
        raise ValueError("segments and anchors must be lists or tuples")
    for segment in segments:
        _fields(segment, ("media_sequence", "discontinuity_epoch", "pts_tick", "duration_ticks"))
        _integer(segment["duration_ticks"], "duration_ticks", 1)
    anchor_fields = ("media_sequence", "discontinuity_epoch", "pts_tick",
                     "program_date_time_utc_us", "uncertainty_us")
    incomplete_anchor = False
    for item in anchors:
        if not isinstance(item, Mapping):
            incomplete_anchor = True
            continue
        for name in anchor_fields:
            if item.get(name) is None:
                incomplete_anchor = True
            else:
                _integer(item[name], name)
    for value, name in ((last_observed_utc_us, "last_observed_utc_us"),
                        (asr_completed_utc_us, "asr_completed_utc_us")):
        if value is not None:
            _integer(value, name)
    known = last_observed_utc_us is not None and asr_completed_utc_us is not None
    result = {"interval": deepcopy(dict(interval)), "epoch_status": "UNRESOLVED",
              "clock": "RELATIVE_ONLY", "null_reason": "MISSING_VERIFIED_ANCHOR",
              "utc_interval_us": None,
              "available_at_utc_us": max(last_observed_utc_us, asr_completed_utc_us) if known else None,
              "availability_null_reason": None if known else "MISSING_OBSERVATION_OR_ASR_COMPLETION"}
    if not anchors:
        return result
    if incomplete_anchor:
        return dict(result, null_reason="UNTRUSTED_OR_UNBOUND_ANCHOR")
    for item in anchors:
        if (item.get("verified") is not True
                or not isinstance(item.get("evidence_id"), str)
                or not item["evidence_id"].strip()
                or not any(all(item.get(k) == s[k] for k in
                               ("media_sequence", "discontinuity_epoch", "pts_tick"))
                           for s in segments)):
            return dict(result, null_reason="UNTRUSTED_OR_UNBOUND_ANCHOR")
    if any(s["discontinuity_epoch"] != interval["discontinuity_epoch"] for s in segments):
        return dict(result, null_reason="CROSS_DISCONTINUITY")
    ordered = sorted(segments, key=lambda s: s["media_sequence"])
    if (not ordered
            or any(b["media_sequence"] != a["media_sequence"] + 1
                   or b["pts_tick"] != a["pts_tick"] + a["duration_ticks"]
                   for a, b in zip(ordered, ordered[1:]))
            or interval["origin_pts_tick"] + interval["start_tick"] < ordered[0]["pts_tick"]
            or interval["origin_pts_tick"] + interval["end_tick"]
            > ordered[-1]["pts_tick"] + ordered[-1]["duration_ticks"]):
        return dict(result, null_reason="AMBIGUOUS_SEGMENT_MAPPING")
    rate = interval["ticks_per_second"]
    # Scaled integer offsets retain fractional microseconds until final rounding.
    lower = max((a["program_date_time_utc_us"] - a["uncertainty_us"]) * rate
                - a["pts_tick"] * 1_000_000 for a in anchors)
    upper = min((a["program_date_time_utc_us"] + a["uncertainty_us"]) * rate
                - a["pts_tick"] * 1_000_000 for a in anchors)
    if lower > upper:
        return dict(result, null_reason="CONTRADICTORY_ANCHORS")
    bounds = {}
    for edge in ("start", "end"):
        position = (interval["origin_pts_tick"] + interval[edge + "_tick"]) * 1_000_000
        bounds[edge + "_lower"] = (position + lower) // rate
        bounds[edge + "_upper"] = -(-(position + upper) // rate)
    return dict(result, epoch_status="RESOLVED", clock="UTC_BOUNDED",
                null_reason=None, utc_interval_us=bounds)


def eligible_in_window(result, *, window_start_utc_us, window_end_utc_us, cutoff_utc_us):
    """Clock-only admission: entire [start,end) fits, ASR exists by cutoff.

    Consume an unmodified map_interval result, not arbitrary external records.
    A valid bounded mapping may still straddle a boundary and is then ineligible.
    This is not identity/rights/partition or complete narrative admission.
    """
    for value, name in ((window_start_utc_us, "window_start_utc_us"),
                        (window_end_utc_us, "window_end_utc_us"),
                        (cutoff_utc_us, "cutoff_utc_us")):
        _integer(value, name)
    if window_end_utc_us <= window_start_utc_us:
        raise ValueError("window must be nonempty and ordered")
    bounds = result.get("utc_interval_us")
    available = result.get("available_at_utc_us")
    return (result.get("epoch_status") == "RESOLVED"
            and result.get("clock") == "UTC_BOUNDED" and bounds is not None
            and available is not None and available <= cutoff_utc_us
            and bounds["start_lower"] >= window_start_utc_us
            and bounds["end_upper"] <= window_end_utc_us)


def assess_capture_metadata(context, probe_receipt):
    """Metadata-only assessment; neither record authenticates a PDT/PTS anchor.

    Does not invent a transcript interval, segments, epoch, or ASR completion.
    Actual anchors must be independently verified and passed to map_interval.
    """
    if not isinstance(context, Mapping) or not isinstance(probe_receipt, Mapping):
        raise ValueError("context and probe receipt must be mappings")
    return {"stream_id": context.get("stream_id"),
            "epoch_status": "UNRESOLVED", "clock": "RELATIVE_ONLY",
            "media_epoch_utc_us": None, "available_at_utc_us": None,
            "null_reason": "MISSING_VERIFIED_ANCHOR",
            "availability_null_reason": "MISSING_OBSERVATION_OR_ASR_COMPLETION",
            "capture_started_unix_metadata_only": context.get("capture_started_unix"),
            "probe_observed_at_metadata_only": probe_receipt.get("observed_at_pt"),
            "format_metadata": deepcopy(probe_receipt.get("probe", {}).get("format", {}))}
