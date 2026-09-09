"""Passive exact-session filesystem observations, not a process supervisor.

Only stats capture payloads. Reads bounded JSON metadata using an output
allowlist; never reads logs, environment, network URLs or media payloads.
Import and API calls never write, launch, stop, compact or purge anything.
"""
import datetime as _dt
import json
import math
from pathlib import Path
import re
import stat
import time

_METADATA_LIMIT = 1_048_576


def _number(value):
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


def _identity(session_id, stream_id, channel):
    if not isinstance(session_id, str) or not re.fullmatch(r'[0-9]{8}_[0-9]{6}_[0-9]{6}', session_id):
        raise ValueError('invalid session identifier')
    try:
        start = _dt.datetime.strptime(session_id[:15], '%Y%m%d_%H%M%S').replace(tzinfo=_dt.timezone.utc)
    except ValueError:
        raise ValueError('invalid session timestamp') from None
    if not isinstance(stream_id, str) or not re.fullmatch(r'[0-9]{1,32}', stream_id):
        raise ValueError('invalid stream identifier')
    if not isinstance(channel, str) or not re.fullmatch(r'[A-Za-z0-9_]{1,25}', channel):
        raise ValueError('invalid channel identifier')
    return start


def _stats(directory, pattern):
    files, unavailable = [], False
    try:
        # Nonrecursive enumeration: never glob a session prefix or follow links.
        for path in directory.iterdir():
            if not pattern.fullmatch(path.name):
                continue
            try:
                info = path.lstat()
                if not stat.S_ISREG(info.st_mode) or path.is_symlink():
                    unavailable = True
                    continue
                files.append({'name': path.name, 'bytes': info.st_size,
                              'mtime_ns': info.st_mtime_ns})
            except OSError:
                unavailable = True
    except OSError:
        unavailable = True
    files.sort(key=lambda row: row['name'])
    return {'status': 'unavailable' if unavailable else ('observed' if files else 'missing'),
            'bytes': sum(row['bytes'] for row in files), 'file_count': len(files), 'files': files}


def _metadata(path):
    try:
        if path.is_symlink():
            return None, 'unreadable_or_invalid'
        with path.open('rb') as handle:
            payload = handle.read(_METADATA_LIMIT + 1)
        if len(payload) > _METADATA_LIMIT:
            return None, 'unreadable_or_invalid'
        value = json.loads(payload)
        if not isinstance(value, dict):
            return None, 'unreadable_or_invalid'
        return value, 'present'
    except FileNotFoundError:
        return None, 'not_evidenced'
    except (OSError, ValueError, UnicodeError):
        # No exception text: paths/content may contain credentials.
        return None, 'unreadable_or_invalid'


def scan_sample(raw_directory, media_directory, *, session_id, stream_id,
                channel, observed_at_unix=None):
    """Take a non-atomic passive sample of only the selected identities.

    observed_at_unix is injectable for deterministic tests; production defaults
    to the local clock. Manifest presence/final filenames never prove integrity.
    """
    _identity(session_id, stream_id, channel)
    observed = time.time() if observed_at_unix is None else observed_at_unix
    if not _number(observed):
        raise ValueError('invalid observation time')
    raw_directory, media_directory = Path(raw_directory), Path(media_directory)
    raw = _stats(raw_directory, re.compile(
        rf'pumpfun_laserstream_raw_v1_{session_id}(?:_part[0-9]+)?\.ndjson\.zst'))
    events = _stats(raw_directory, re.compile(
        rf'pumpfun_laserstream_events_v1_{session_id}\.ndjson\.zst'))
    media = _stats(media_directory, re.compile(rf'{channel}_{stream_id}\.mp4(?:\.part)?'))
    names = {row['name'] for row in media['files']}
    media_finalization = 'not_evidenced'
    if f'{channel}_{stream_id}.mp4.part' in names:
        media_finalization = 'partial_present'
    elif f'{channel}_{stream_id}.mp4' in names:
        media_finalization = 'final_name_present_unverified'
    manifest, finalization = _metadata(raw_directory / f'pumpfun_laserstream_manifest_v1_{session_id}.json')
    if manifest is not None:
        start, end = manifest.get('start_unix_ms'), manifest.get('end_unix_ms')
        finalization = ('manifest_present_unverified' if
            manifest.get('session_id') == session_id and _number(start) and _number(end)
            and end >= start and isinstance(manifest.get('raw_files'), list)
            and 'events_file' in manifest else 'unreadable_or_invalid')
    context, context_status = _metadata(media_directory / 'CAPTURE_CONTEXT.json')
    metadata_timestamp = None
    if context is not None:
        if context.get('stream_id') == stream_id and context.get('laserstream_session') == session_id:
            candidate = context.get('capture_started_unix')
            if _number(candidate):
                metadata_timestamp = candidate
            else:
                context_status = 'unreadable_or_invalid'
        else:
            context_status = 'identity_mismatch'
    return {'session_id': session_id, 'stream_id': stream_id, 'channel': channel,
            'observed_at_unix': observed, 'raw': raw, 'events': events, 'media': media,
            'laserstream_finalization': finalization, 'media_finalization': media_finalization,
            'context': {'status': context_status, 'metadata_timestamp_unix': metadata_timestamp,
                        'media_epoch_unix': None, 'media_epoch_status': 'unknown_not_metadata_timestamp'}}


def _growth(first, second):
    old = {row['name']: row['bytes'] for row in first['files']}
    new = {row['name']: row['bytes'] for row in second['files']}
    delta = second['bytes'] - first['bytes']
    if first['status'] == 'unavailable' or second['status'] == 'unavailable':
        state = 'observation_unavailable'
    elif any(name not in new or new[name] < size for name, size in old.items()):
        state = 'file_disappeared_or_shrank'
    elif delta > 0:
        state = 'growth_observed'
    elif not old and not new:
        state = 'missing'
    else:
        state = 'no_growth_observed_not_proof_of_stall'
    return {'delta_bytes': delta, 'state': state}


def build_report(first, second, *, capture_minutes=120, warning_minutes=15):
    """Summarize exactly two samples without inferring continuity or media UTC."""
    for key in ('session_id', 'stream_id', 'channel'):
        if first[key] != second[key]:
            raise ValueError('observation identities differ')
    start = _identity(first['session_id'], first['stream_id'], first['channel'])
    a, b = first['observed_at_unix'], second['observed_at_unix']
    if not _number(a) or not _number(b) or b <= a:
        raise ValueError('positive observation interval required')
    if not _number(capture_minutes) or capture_minutes == 0 or not _number(warning_minutes):
        raise ValueError('invalid capture or warning duration')
    end = start + _dt.timedelta(minutes=capture_minutes)
    remaining = end.timestamp() - b
    if remaining <= 0:
        status = 'deadline_reached_finalize_or_gap_check_required'
    elif remaining <= warning_minutes * 60:
        status = 'deadline_approaching'
    else:
        status = 'bounded_capture_deadline_pending'
    return {'schema_version': 'capture_health_v1', 'observations': [first, second],
            'elapsed_seconds': b - a,
            'growth': {key: _growth(first[key], second[key]) for key in ('raw', 'events', 'media')},
            'deadline': {'capture_minutes': capture_minutes, 'warning_minutes': warning_minutes,
                         'expected_end_utc': end.strftime('%Y-%m-%dT%H:%M:%SZ'),
                         'seconds_remaining': remaining, 'status': status,
                         'basis': 'session_id_generation_utc_not_verified_capture_start'},
            'collector_control_performed': False, 'integrity_verified': False,
            'continuity_verified': False, 'media_market_alignment_verified': False,
            'limitations': ['Non-atomic stat observations; buffered writes can hide short-window growth.',
                            'Manifest presence and final filename are not decoded/hash-verified closure.',
                            'Metadata timestamp is not media epoch; delay/ad/discontinuity mapping unknown.',
                            'No process owner or restart supervision installed; no deletion authority.']}
