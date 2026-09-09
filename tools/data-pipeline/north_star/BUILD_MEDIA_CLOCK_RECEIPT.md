# Stage 3 minimal media clock validator — development receipt

## Outcome

Implemented pure `src/north_star/media_clock.py` with synthetic contract tests in
`tests/north_star/test_media_clock.py`. No service, collector action, network,
download, media-file read, ASR generation, transcript fabrication, or git commit.
No existing source evidence was modified.

**Historical Megga stream `320254509148` remains UNRESOLVED / RELATIVE_ONLY.**
No verified HLS PDT-to-PTS anchor, segment-sequence continuity evidence, or ASR
completion is established by the available capture context and probe metadata.
Historical UTC joins are not eligible.

Real metadata-only execution report:
`D:/mev_bot-artifacts/north_star/development/media_clock/MEGGA_CLOCK_ASSESSMENT.json`.
The original build receipts and source recording are untouched. `CAPTURE_CONTEXT.json`
was not present under build_receipts; the existing record was discovered at
`D:/mev_bot-artifacts/narrative/twitch_live/CAPTURE_CONTEXT.json`.

## Contract and limits

- `map_interval` consumes one source/session's explicitly supplied contiguous
  segment window. Every segment has integer `media_sequence`, `pts_tick`,
  `duration_ticks`, and `discontinuity_epoch`. All use the interval's explicit
  positive `ticks_per_second`. PTS is already unwrapped by the caller.
- Transcript `[start_tick, end_tick)` offsets are relative to explicit
  `origin_pts_tick`, **not** PTS zero, capture start, download time, stream title
  time, or health-probe observation time. Interval and units are retained on
  unresolved results for caller persistence; this pure module does not write.
- Each anchor binds exact segment-start PTS, sequence and epoch to integer Unix
  `program_date_time_utc_us`, nonnegative `uncertainty_us`, `verified is True`,
  and a nonempty `evidence_id`. A flag/reference is not authentication: upstream
  must verify evidence and source/session association. No playlist parser here.
- An uncertainty bound must include total clock error/drift for the supplied
  window. Mapping intersects all anchors' exact scaled-integer offset bounds;
  contradictory anchors fail closed, rather than choosing a convenient anchor.
  Endpoint UTC bounds round outward to integer microseconds. No float arithmetic.
- Missing/incomplete/untrusted/unbound anchors, gaps, duplicate sequences,
  overlapping PTS, extrapolation outside supplied coverage, and mixed epochs
  return relative-only with a reason. Bad numeric types/ranges raise ValueError;
  bool, string and float are not coerced. No cross-discontinuity interpolation.
  The caller must supply one epoch window; even an irrelevant mixed-epoch
  segment conservatively rejects the supplied window.
- `available_at_utc_us = max(last_observed_utc_us, asr_completed_utc_us)` only when
  both are known. `last_observed` means the last observation of media used for
  this transcript version, not an arbitrary later health probe. Availability
  inputs must be independently verified conservative UTC times. Missing either
  leaves availability null even when event-time mapping is resolved.
- `eligible_in_window` consumes an unmodified validator result, requires the
  full bounded interval to fit the supplied half-open window and ASR-derived
  availability to be at/before cutoff. Equality at window end is allowed for
  the exclusive transcript end. Uncertainty crossing either boundary rejects
  eligibility without discarding an otherwise valid bounded mapping.
- Timing validation is not full narrative admission: rights, identity, token
  links, causal dependency timing, partition/split rules and original evidence
  authenticity remain external. No runtime integration or complete Stage 3
  completion is claimed. This does not recover historical Megga epoch.

## Strict TDD execution evidence

Interpreter: `C:/Users/Alon/AppData/Local/hermes/hermes-agent/venv/Scripts/python.exe`
(Python 3.11 environment; pytest 9.1.1).
Tests disable bytecode and pytest cache writes. Each behavior batch was exercised
before implementation and rerun after its minimal implementation:

| Step | Observed RED | Observed GREEN |
|---|---|---|
| Missing anchor retention | 1 failed, missing module | 1 passed |
| Midstream offset / buffering | 1 failed, 1 passed | 2 passed |
| Anchor trust / exact binding | 6 failed, 2 passed | 8 passed |
| Sequence continuity / epoch / coverage | 7 failed, 8 passed | 15 passed |
| Consistent / contradictory anchors | 2 failed, 16 passed | 18 passed |
| Availability, units, boundary, metadata | 43 failed, 19 passed | 82 passed including availability regression |
| Incomplete anchors retain evidence | 3 failed, 63 passed | 86 passed including availability regression |

Final targeted execution:

```text
PYTHONDONTWRITEBYTECODE=1 python -m pytest tests/north_star/test_media_clock.py -q -p no:cacheprovider --tb=short
66 passed in 0.11s
```

Final combined regression:

```text
PYTHONDONTWRITEBYTECODE=1 python -m pytest tests/north_star/test_media_clock.py tests/north_star/test_availability.py -q -p no:cacheprovider --tb=short
86 passed in 0.13s
```

`git diff --check -- src/north_star/media_clock.py tests/north_star/test_media_clock.py`
exited 0; new files were additionally syntax-checked by edit tools and imported by
pytest. No full repository suite claim; other workers own concurrent changes.

## Real metadata exercise, not synthetic recovered timestamps

Executed `assess_capture_metadata` on the existing JSON bytes at
`2026-09-09T18:27:23.912929+00:00` (assessment execution time only):

| Input | SHA-256 | Bytes |
|---|---|---:|
| narrative/twitch_live/CAPTURE_CONTEXT.json | `76d3a4fb60472bc281e5069dca3b07b1f8e236a9c711d29ebaf68963bce5f74b` | 672 |
| north_star/build_receipts/MEGGA_MEDIA_PROBE.json | `dad3e1ed6cf901d2785d0dbbe9a379c99519751a8f1129ffa0613af27bb05363` | 1274 |

Returned:

```json
{
  "stream_id": "320254509148",
  "epoch_status": "UNRESOLVED",
  "clock": "RELATIVE_ONLY",
  "media_epoch_utc_us": null,
  "available_at_utc_us": null,
  "null_reason": "MISSING_VERIFIED_ANCHOR",
  "availability_null_reason": "MISSING_OBSERVATION_OR_ASR_COMPLETION",
  "capture_started_unix_metadata_only": 1788965406,
  "probe_observed_at_metadata_only": "2026-09-09T08:59:58.608539-07:00",
  "format_metadata": {
    "format_name": "mpegts",
    "start_time": "1.467000",
    "duration": "4178.194311"
  }
}
```

The `.mp4.part` name does not change the probe's MPEG-TS format evidence. Probe
relative start/duration are retained as original strings; neither establishes
UTC epoch or full continuity. `capture_started_unix` is metadata creation time,
not media epoch. No synthetic transcript interval was attached to this real
report. Source-byte reread during execution passed unchanged. Saved report was
then replayed through the validator and input hashes compared again:
`SAVED_REAL_REPORT_REPLAY_AND_SOURCE_HASHES: PASS`.

Independent review `deleg_509af810` returned BOUNDED_PASS with no must-fix within pure clock/metadata scope. It exercised 2,000 rational endpoint and 8,000 window/cutoff comparisons plus anchor/discontinuity rejection probes. Parent reran the clock+availability suite:86 passed. Missing original recording UTC anchor remains unresolved; no narrative admission follows from this code review.
