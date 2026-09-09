"""Passive scanner contracts exercised on tiny synthetic files only."""
import importlib.util
import json
from pathlib import Path
import pytest

MODULE = Path(__file__).parents[2] / 'src/north_star/capture_health.py'
SESSION = '20260909_144906_000490'
STREAM = '320254509148'


def load_health():
    assert MODULE.exists(), 'capture health implementation missing'
    spec = importlib.util.spec_from_file_location('ns_capture_health', MODULE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fixture_dirs(tmp_path):
    raw, media = tmp_path / 'raw', tmp_path / 'media'
    raw.mkdir()
    media.mkdir()
    (raw / f'pumpfun_laserstream_raw_v1_{SESSION}_part0000.ndjson.zst').write_bytes(b'raw')
    (raw / f'pumpfun_laserstream_events_v1_{SESSION}.ndjson.zst').write_bytes(b'ev')
    (media / f'megga_{STREAM}.mp4.part').write_bytes(b'media')
    return raw, media


def sample(module, raw, media, t=1788969000):
    return module.scan_sample(raw, media, session_id=SESSION, stream_id=STREAM,
                              channel='megga', observed_at_unix=t)


def test_exact_sessions_only_and_no_secret_metadata_output(tmp_path):
    raw, media = fixture_dirs(tmp_path)
    (raw / f'pumpfun_laserstream_raw_v1_{SESSION}9_part0000.ndjson.zst').write_bytes(b'x' * 100)
    (raw / 'other.log').write_text('https://secret.invalid/?token=DO_NOT_LOG')
    (media / 'CAPTURE_CONTEXT.json').write_text(json.dumps(dict(
        capture_started_unix=1788965406, stream_id=STREAM, laserstream_session=SESSION,
        note='DO_NOT_LOG', url='https://secret.invalid/?token=DO_NOT_LOG')))
    result = sample(load_health(), raw, media)
    assert result['raw']['bytes'] == 3
    assert result['raw']['file_count'] == 1
    assert result['events']['bytes'] == 2
    assert result['media']['bytes'] == 5
    assert result['observed_at_unix'] == 1788969000
    assert result['context']['metadata_timestamp_unix'] == 1788965406
    assert result['context']['media_epoch_unix'] is None
    assert result['context']['media_epoch_status'] == 'unknown_not_metadata_timestamp'
    assert 'DO_NOT_LOG' not in json.dumps(result)
    assert 'https://' not in json.dumps(result)
    assert result['laserstream_finalization'] == 'not_evidenced'
    assert result['media_finalization'] == 'partial_present'


def test_two_observations_measure_growth_and_deadline(tmp_path):
    raw, media = fixture_dirs(tmp_path)
    h = load_health()
    first = sample(h, raw, media, 1788969000)
    (raw / f'pumpfun_laserstream_raw_v1_{SESSION}_part0000.ndjson.zst').write_bytes(b'raw-plus')
    (media / f'megga_{STREAM}.mp4.part').write_bytes(b'media-growth')
    second = sample(h, raw, media, 1788969005)
    report = h.build_report(first, second, capture_minutes=120, warning_minutes=60)
    assert report['observations'] == [first, second]
    assert report['elapsed_seconds'] == 5
    assert report['growth']['raw']['delta_bytes'] == 5
    assert report['growth']['raw']['state'] == 'growth_observed'
    assert report['growth']['events']['state'] == 'no_growth_observed_not_proof_of_stall'
    assert report['growth']['media']['delta_bytes'] == 7
    assert report['deadline']['status'] == 'deadline_approaching'
    assert report['deadline']['expected_end_utc'] == '2026-09-09T16:49:06Z'
    assert report['deadline']['basis'] == 'session_id_generation_utc_not_verified_capture_start'
    assert report['collector_control_performed'] is False
    assert report['integrity_verified'] is False


def test_deadline_overdue_requires_attention_not_restart(tmp_path):
    raw, media = fixture_dirs(tmp_path)
    h = load_health()
    report = h.build_report(sample(h, raw, media, 1788972600), sample(h, raw, media, 1788973200))
    assert report['deadline']['status'] == 'deadline_reached_finalize_or_gap_check_required'


def test_shrinkage_or_disappearance_is_not_normal_growth(tmp_path):
    raw, media = fixture_dirs(tmp_path)
    h = load_health()
    first = sample(h, raw, media, 100)
    (raw / f'pumpfun_laserstream_raw_v1_{SESSION}_part0000.ndjson.zst').unlink()
    (raw / f'pumpfun_laserstream_raw_v1_{SESSION}_part0001.ndjson.zst').write_bytes(b'x' * 100)
    report = h.build_report(first, sample(h, raw, media, 101))
    assert report['growth']['raw']['state'] == 'file_disappeared_or_shrank'


def test_media_final_name_is_not_integrity_proof(tmp_path):
    raw, media = fixture_dirs(tmp_path)
    (media / f'megga_{STREAM}.mp4.part').rename(media / f'megga_{STREAM}.mp4')
    result = sample(load_health(), raw, media)
    assert result['media_finalization'] == 'final_name_present_unverified'
    assert result['context']['media_epoch_unix'] is None


@pytest.mark.parametrize('payload,status', [
    ('{', 'unreadable_or_invalid'),
    (json.dumps({'session_id': 'wrong', 'end_unix_ms': 10}), 'unreadable_or_invalid'),
    (json.dumps({'session_id': SESSION, 'start_unix_ms': 1, 'end_unix_ms': 2,
                 'raw_files': [], 'events_file': None, 'endpoint_host': 'DO_NOT_LOG'}),
     'manifest_present_unverified')])
def test_manifest_is_evidence_not_integrity_verification(tmp_path, payload, status):
    raw, media = fixture_dirs(tmp_path)
    (raw / f'pumpfun_laserstream_manifest_v1_{SESSION}.json').write_text(payload)
    result = sample(load_health(), raw, media)
    assert result['laserstream_finalization'] == status
    assert 'DO_NOT_LOG' not in json.dumps(result)


def test_missing_directories_and_bad_context_fail_closed(tmp_path):
    result = sample(load_health(), tmp_path / 'missing', tmp_path / 'absent')
    assert result['raw']['status'] == 'unavailable'
    assert result['media']['status'] == 'unavailable'
    assert result['context']['metadata_timestamp_unix'] is None


@pytest.mark.parametrize('field,value', [('session_id', '../bad'),
    ('session_id', SESSION + '*'), ('session_id', '20261399_144906_000490'),
    ('stream_id', '../x'), ('channel', 'megga/../secret')])
def test_rejects_unsafe_identifiers_before_io(tmp_path, field, value):
    kwargs = dict(session_id=SESSION, stream_id=STREAM, channel='megga')
    kwargs[field] = value
    with pytest.raises(ValueError):
        load_health().scan_sample(tmp_path, tmp_path, **kwargs)


@pytest.mark.parametrize('first_time,second_time', [(10, 10), (20, 10), (float('nan'), 20)])
def test_nonpositive_or_invalid_observation_window_rejected(tmp_path, first_time, second_time):
    raw, media = fixture_dirs(tmp_path)
    h = load_health()
    with pytest.raises(ValueError):
        h.build_report(sample(h, raw, media, first_time), sample(h, raw, media, second_time))


def test_different_session_observations_cannot_be_combined(tmp_path):
    raw, media = fixture_dirs(tmp_path)
    h = load_health()
    first = sample(h, raw, media, 100)
    second = sample(h, raw, media, 101)
    second['session_id'] = '20260909_144906_000491'
    with pytest.raises(ValueError):
        h.build_report(first, second)


def test_context_for_other_session_is_not_used(tmp_path):
    raw, media = fixture_dirs(tmp_path)
    (media / 'CAPTURE_CONTEXT.json').write_text(json.dumps(dict(
        capture_started_unix=1788965406, stream_id='999', laserstream_session=SESSION)))
    assert sample(load_health(), raw, media)['context']['metadata_timestamp_unix'] is None


def test_scanner_and_report_never_write_or_control_processes(tmp_path, monkeypatch):
    raw, media = fixture_dirs(tmp_path)
    h = load_health()
    import subprocess
    import os
    original_open = Path.open

    def read_only_open(self, mode='r', *args, **kwargs):
        assert not any(flag in mode for flag in ('w', 'a', '+', 'x'))
        return original_open(self, mode, *args, **kwargs)

    def forbidden(*args, **kwargs):
        pytest.fail('passive scan attempted a state change')

    monkeypatch.setattr(Path, 'open', read_only_open)
    for method in ('unlink', 'rename', 'replace', 'mkdir', 'write_text', 'write_bytes'):
        monkeypatch.setattr(Path, method, forbidden)
    monkeypatch.setattr(subprocess, 'Popen', forbidden)
    monkeypatch.setattr(os, 'system', forbidden)
    h.build_report(sample(h, raw, media, 100), sample(h, raw, media, 101))


def test_oversized_metadata_is_not_read_as_unbounded_content(tmp_path):
    raw, media = fixture_dirs(tmp_path)
    (media / 'CAPTURE_CONTEXT.json').write_bytes(b'x' * (1048576 + 1))
    assert sample(load_health(), raw, media)['context']['metadata_timestamp_unix'] is None
