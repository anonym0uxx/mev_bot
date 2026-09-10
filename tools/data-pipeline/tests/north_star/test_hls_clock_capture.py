"""Synthetic HLS only; real capture is separately invoked and bounded."""
import importlib.util
from pathlib import Path

import pytest

MODULE = Path(__file__).parents[2] / 'src/north_star/hls_clock_capture.py'


@pytest.fixture(autouse=True)
def offline_only(monkeypatch):
    import socket
    def blocked(*args, **kwargs):
        raise AssertionError('live_network_forbidden_in_HLS_tests')
    monkeypatch.setattr(socket.socket, 'connect', blocked)
    monkeypatch.setattr(socket.socket, 'connect_ex', blocked)
    monkeypatch.setattr(socket, 'getaddrinfo', blocked)


PDT = '#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:01.000Z\n'
SEGMENT = '#EXTINF:1.000,\na.ts\n'


@pytest.mark.parametrize('body', [
    '#EXT-X-MEDIA-SEQUENCE:-1\n' + PDT + SEGMENT,
    '#EXT-X-DISCONTINUITY-SEQUENCE:-1\n' + PDT + SEGMENT,
    '#EXT-X-MEDIA-SEQUENCE:1\n#EXT-X-MEDIA-SEQUENCE:1\n' + PDT + SEGMENT,
    '#EXT-X-DISCONTINUITY-SEQUENCE:1\n#EXT-X-DISCONTINUITY-SEQUENCE:2\n' + PDT + SEGMENT,
    PDT + SEGMENT + '#EXT-X-MEDIA-SEQUENCE:7\n' + SEGMENT,
    PDT + SEGMENT + '#EXT-X-DISCONTINUITY-SEQUENCE:7\n' + SEGMENT,
    '#EXT-X-DISCONTINUITY\n#EXT-X-DISCONTINUITY-SEQUENCE:7\n' + PDT + SEGMENT,
    PDT + '#EXT-X-MEDIA-SEQUENCE:7\n' + SEGMENT,
    PDT + PDT + SEGMENT,
    PDT + '#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:02.000Z\n' + SEGMENT,
    PDT + SEGMENT + PDT + SEGMENT,
    PDT + SEGMENT + '#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:03.000Z\n' + SEGMENT,
    PDT + '#EXTINF:2.000,\n' + SEGMENT,
    PDT + SEGMENT + '#EXTINF:2.000,\n',
    PDT + SEGMENT + PDT,
    PDT + '#EXT-X-DISCONTINUITY\n' + SEGMENT,
    PDT + '#EXTINF:1.000,\n#EXT-X-DISCONTINUITY\na.ts\n',
    PDT + SEGMENT + '#EXT-X-DISCONTINUITY\n',
    PDT + 'a.ts\n',
    PDT + SEGMENT + 'orphan.ts\n',
    PDT + '#EXTINF:1.000,\n#EXT-X-ENDLIST\na.ts\n',
    PDT + SEGMENT + '#EXT-X-ENDLIST\n' + SEGMENT,
    PDT + SEGMENT + '#EXT-X-ENDLIST\n#EXT-X-ENDLIST\n',
    PDT + SEGMENT + '#EXTM3U\n',
    '#EXT-X-MEDIA-SEQUENCE:+1\n' + PDT + SEGMENT,
    '#EXT-X-MEDIA-SEQUENCE:1_000\n' + PDT + SEGMENT,
    '#EXT-X-MEDIA-SEQUENCE:18446744073709551616\n' + PDT + SEGMENT,
    PDT + '#EXTINF:1e0,\na.ts\n',
    PDT + '#EXTINF:1.000\na.ts\n',
    '#EXT-X-PROGRAM-DATE-TIME\n' + SEGMENT,
    '#EXT-X-STREAM-INF:BANDWIDTH=1\nvariant.m3u8\n' + PDT + SEGMENT,
    '#EXT-X-SKIP:SKIPPED-SEGMENTS=3\n' + PDT + SEGMENT,
    PDT + '#EXT-X-PART:DURATION=0.5,URI="a.part"\n' + SEGMENT,
    PDT + SEGMENT + '#EXT-X-PRELOAD-HINT:TYPE=PART,URI="b.part"\n',
    '#EXT-X-PART-INF:PART-TARGET=0.5\n' + PDT + SEGMENT,
    '#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES\n' + PDT + SEGMENT,
    PDT + SEGMENT + '#EXT-X-RENDITION-REPORT:URI="b.m3u8"\n',
    PDT + '#EXT-X-GAP\n' + SEGMENT,
    '#EXT-X-UNKNOWN-CLOCK-MUTATION:1\n' + PDT + SEGMENT,
])
def test_invalid_or_unsupported_playlist_cannot_invent_mapping(body):
    with pytest.raises(ValueError):
        load_clock().parse_playlist(('#EXTM3U\n' + body).encode())


def test_valid_consecutive_explicit_pdt_and_pdt_after_extinf():
    raw = ('#EXTM3U\n' + PDT + SEGMENT + '#EXTINF:1.000,\n'
           '#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:02.000Z\nb.ts\n').encode()
    parsed = load_clock().parse_playlist(raw)
    assert [a['program_date_time_ms'] for a in parsed['anchors']] == [1000, 2000]
    assert parsed['segment_content_identity'] == 'unverified_may_include_ad_or_slate'
    assert parsed['old_recording_utc_offset_ms'] is None


def load_clock():
    assert MODULE.exists(), 'HLS clock capture implementation missing'
    spec = importlib.util.spec_from_file_location('ns_hls_clock_capture', MODULE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_exact_clock_sequences_and_discontinuity_do_not_bridge_old_file():
    raw = b'''#EXTM3U
#EXT-X-MEDIA-SEQUENCE:81
#EXT-X-DISCONTINUITY-SEQUENCE:4
#EXT-X-PROGRAM-DATE-TIME:1970-01-01T02:00:01.123+02:00
#EXTINF:2.007,private-title
https://example.invalid/a.ts?token=PRIVATE
#EXTINF:0.333,
b.ts
#EXT-X-DISCONTINUITY
#EXTINF:1.000,
c.ts
#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:10.005Z
#EXTINF:0.001,
d.ts
'''
    result = load_clock().parse_playlist(raw)
    assert result['media_sequence'] == 81
    assert result['discontinuity_sequence'] == 4
    assert [s['media_sequence'] for s in result['segments']] == [81, 82, 83, 84]
    assert [s['discontinuity_sequence'] for s in result['segments']] == [4, 4, 5, 5]
    assert [s['duration_ms'] for s in result['segments']] == [2007, 333, 1000, 1]
    assert [s['program_date_time_ms'] for s in result['segments']] == [1123, 3130, None, 10005]
    assert [s['clock_basis'] for s in result['segments']] == ['explicit_pdt', 'extinf_from_pdt', 'unknown', 'explicit_pdt']
    assert result['old_recording_utc_offset_ms'] is None
    assert len(result['anchors']) == 2
    assert 'PRIVATE' not in str(result)


def test_safe_evidence_hashes_original_and_drops_all_uri_attributes():
    import hashlib
    raw = b'''#EXTM3U
#EXT-X-KEY:METHOD=AES-128,URI="https://secret.invalid/?token=PRIVATE"
#EXT-X-MAP:URI="relative?sig=PRIVATE"
#EXT-X-DATERANGE:ID="PRIVATE",X-URI="PRIVATE"
#EXT-X-MEDIA-SEQUENCE:9
#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:01.001Z
#EXTINF:0.999,PRIVATE
https://secret.invalid/a.ts?token=PRIVATE
'''
    clock = load_clock()
    safe = clock.safe_playlist(raw)
    assert 'PRIVATE' not in safe
    assert 'https://' not in safe
    assert '[REDACTED-URI]' in safe
    assert '#EXTINF:0.999,' in safe
    ev = clock.playlist_evidence(raw, 10, 20, 'media')
    assert ev['original_sha256'] == hashlib.sha256(raw).hexdigest()
    assert ev['retrieval_started_unix_ms'] == 10
    assert ev['retrieval_completed_unix_ms'] == 20
    assert ev['original_byte_count'] == len(raw)


def test_submillisecond_precision_and_missing_clock_fail_closed():
    import pytest
    clock = load_clock()
    for value in ['0.0001', '-1', 'NaN', 'Infinity']:
        with pytest.raises(ValueError):
            clock.exact_ms(value)
    for value in ['2026-09-09T18:00:00.1234Z', '2026-09-09T18:00:00']:
        with pytest.raises(ValueError):
            clock.utc_ms(value)
    result = clock.parse_playlist(b'#EXTM3U\n#EXTINF:1.000,\na.ts\n#EXT-X-ENDLIST\n')
    assert result['ended']
    assert result['anchors'] == []
    assert result['segments'][0]['program_date_time_ms'] is None
    with pytest.raises(ValueError):
        clock.parse_playlist(b'<html>login required</html>')


@pytest.mark.parametrize('precision', [2, 28, 80])
def test_duration_conversion_is_exact_independent_of_decimal_context(precision):
    from decimal import Inexact, Rounded, localcontext
    clock = load_clock()
    with localcontext() as ctx:
        ctx.prec = precision
        ctx.traps[Inexact] = ctx.traps[Rounded] = True
        assert clock.exact_ms('123456789012345678901234567890.123') == 123456789012345678901234567890123
        assert clock.exact_ms('1.123' + '0' * 40) == 1123
        with pytest.raises(ValueError):
            clock.exact_ms('1.0000000000000000000000000000000001')


@pytest.mark.parametrize('value', [
    '+1', '-0', '1e0', '1_000', ' 1', '1 ', '1\n', '.001', '1.',
    '\u0661', '\u0661.\u0660\u0660\u0661', '', None, 1, 1.0, True,
    '1' * 65, '0.' + '0' * 63,
])
def test_duration_rejects_nonliteral_or_oversized_input(value):
    with pytest.raises(ValueError):
        load_clock().exact_ms(value)


def test_duration_literal_size_boundary_is_exact():
    clock = load_clock()
    assert clock.exact_ms('9' * 64) == (10 ** 64 - 1) * 1000
    assert clock.exact_ms('0.' + '0' * 62) == 0
    assert clock.exact_ms('0.001') == 1
    assert clock.exact_ms('0001.1000') == 1100


@pytest.mark.parametrize('value', [
    '2026-09-09T18:00:00.1230000000000000000000000000000001Z',
    '2026-09-09T18:00:00.0000001Z',
    '2026-09-09T18:00:00+00:99', '2026-09-09T18:00:00-00:60',
    '2026-09-09T18:00:00+24:00', '2026-09-09T18:00:00-24:00',
    '2026-09-09T18:00:00-00:00',  # unknown-local-offset form outside subset
    '1969-12-31T23:59:59.999Z', '1970-01-01T00:00:00+00:01',
    '2026-09-09T24:00:00Z', '2026-09-09T18:60:00Z',
    '2016-12-31T23:59:60Z', '0000-01-01T00:00:00Z',
    '2026-02-29T00:00:00Z', '2026-13-01T00:00:00Z',
    '2026-01-00T00:00:00Z', '2026-04-31T00:00:00Z',
    '2026-09-09 18:00:00Z', '2026-09-09t18:00:00z',
    '20260909T180000Z', '2026-09-09T18:00Z',
    '2026-09-09T18:00:00+0000', '2026-09-09T18:00:00+00:00:01',
    '2026-09-09T18:00:00,123Z', '2026-09-09T18:00:00.Z',
    '2026-09-09T18:00:00.\u0661\u0662\u0663Z',
    '2026-09-09T18:00:00Z\n', '2026-09-09T18:00:00Z ',
    '2026-09-09T18:00:00.' + '0' * 44 + 'Z',
    None, 1, True,
])
def test_utc_rejects_inexact_malformed_or_unsupported_literals(value):
    with pytest.raises(ValueError):
        load_clock().utc_ms(value)


@pytest.mark.parametrize('precision', [2, 28, 80])
def test_utc_conversion_is_exact_independent_of_decimal_context(precision):
    from decimal import Inexact, Rounded, localcontext
    clock = load_clock()
    with localcontext() as ctx:
        ctx.prec = precision
        ctx.traps[Inexact] = ctx.traps[Rounded] = True
        assert clock.utc_ms('1970-01-01T00:00:01.123' + '0' * 35 + 'Z') == 1123
        assert clock.utc_ms('1970-01-01T00:00:01.' + '0' * 43 + 'Z') == 1000
        with pytest.raises(ValueError):
            clock.utc_ms('1970-01-01T00:00:01.123' + '0' * 30 + '1Z')


@pytest.mark.parametrize('value,expected', [
    ('1970-01-01T00:00:00Z', 0),
    ('1970-01-01T00:00:00.1+00:00', 100),
    ('1970-01-01T00:00:00.0100Z', 10),
    ('1970-01-01T00:00:00.001Z', 1),
    ('1969-12-31T23:00:00-01:00', 0),  # UTC epoch, not local year, matters
    ('1970-01-02T00:00:00+23:59', 60000),
    ('2000-02-29T00:00:00Z', 951782400000),
    ('9999-12-31T23:59:59.999Z', 253402300799999),
])
def test_utc_valid_fields_and_exact_epoch_boundaries(value, expected):
    assert load_clock().utc_ms(value) == expected


def test_utc_exhaustive_two_digit_offset_ranges():
    clock = load_clock()
    # Every syntactically two-digit offset, independently range checked.
    for sign in ('+', '-'):
        for hour in range(100):
            for minute in range(100):
                value = f'1970-01-02T00:00:00{sign}{hour:02d}:{minute:02d}'
                if hour > 23 or minute > 59 or (sign == '-' and hour == minute == 0):
                    with pytest.raises(ValueError):
                        clock.utc_ms(value)
                else:
                    offset = (hour * 60 + minute) * 60000
                    assert clock.utc_ms(value) == 86400000 + (-offset if sign == '+' else offset)


@pytest.mark.parametrize('body', [
    '#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:01.1230000000000000000000000000000001Z\n' + SEGMENT,
    '#EXT-X-PROGRAM-DATE-TIME:1970-01-02T00:00:00+00:99\n' + SEGMENT,
    PDT + '#EXTINF:1.0000000000000000000000000000000001,\na.ts\n',
    PDT + '#EXTINF:' + '9' * 65 + ',\na.ts\n',
])
def test_playlist_and_redaction_reject_invalid_clock_literals(body):
    clock = load_clock()
    raw = ('#EXTM3U\n' + body).encode()
    for parser in (clock.parse_playlist, clock.safe_playlist):
        with pytest.raises(ValueError):
            parser(raw)


def test_stream_identity_mismatch_and_offline_are_explicit():
    clock = load_clock()
    assert clock.stream_comparison('320254509148', True) == 'matching_live_stream'
    assert clock.stream_comparison('320254509149', True) == 'different_stream'
    assert clock.stream_comparison(None, False) == 'ended_or_offline'
    assert clock.stream_comparison(None, None) == 'unverified'


def test_bounded_reader_never_reads_beyond_total_budget():
    import io
    import pytest
    clock = load_clock()
    budget = clock.ReadBudget(10)
    assert budget.read(io.BytesIO(b'abc')) == b'abc'
    assert budget.consumed == 3
    data = io.BytesIO(b'x' * 30)
    with pytest.raises(ValueError, match='response_budget_exhausted'):
        budget.read(data)
    assert data.tell() == 7
    assert budget.consumed == 10


def test_capture_receipt_persists_only_safe_handles(tmp_path):
    import json
    clock = load_clock()
    result = dict(status='gap', gap_reason='offline', source_stream_id=None,
                  playlists=[], stream_comparison='ended_or_offline')
    handle = clock.persist_capture(tmp_path, result)
    saved = json.loads(Path(handle).read_text())
    assert saved['status'] == 'gap'
    assert saved['old_recording_utc_offset_ms'] is None
    assert saved['segments_downloaded'] == 0
    assert saved['collector_control_performed'] is False
    assert saved['continuous_capture'] is False


def test_snapshot_network_boundary_success_and_no_media_on_mismatch(tmp_path):
    import io
    import json
    clock = load_clock()
    calls = []

    class FakeYDL:
        stream_id = clock.EXPECTED_STREAM_ID
        def __enter__(self):
            return self
        def __exit__(self, *args):
            pass
        def extract_info(self, url, download, process):
            assert download is False and process is False
            return dict(id=self.stream_id, is_live=True, formats=[
                dict(url='https://example.invalid/playlist.m3u8?token=PRIVATE', vcodec='h264', height=1080)])
        def urlopen(self, url):
            calls.append(True)
            return io.BytesIO(b'#EXTM3U\n#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:01.000Z\n#EXTINF:1.000,\na.ts\n')

    factory = lambda state: FakeYDL()
    handle = clock.capture(tmp_path, ydl_factory=factory)
    saved = json.loads(Path(handle).read_text())
    assert saved['status'] == 'prospective_anchors_captured'
    assert saved['stream_comparison'] == 'matching_live_stream'
    assert len(saved['clock']['anchors']) == 1
    assert 'PRIVATE' not in Path(handle).read_text()
    assert len(calls) == 1
    FakeYDL.stream_id = '320254509149'
    handle = clock.capture(tmp_path, ydl_factory=factory)
    saved = json.loads(Path(handle).read_text())
    assert saved['status'] == 'gap'
    assert saved['stream_comparison'] == 'different_stream'
    assert len(calls) == 1


def test_snapshot_denial_never_retries_or_logs_exception_secrets(tmp_path, capsys):
    import json
    clock = load_clock()
    calls = []
    def denied(state):
        calls.append(True)
        raise PermissionError('https://secret.invalid/?token=PRIVATE')
    handle = clock.capture(tmp_path, ydl_factory=denied)
    saved = Path(handle).read_text()
    assert json.loads(saved)['status'] == 'gap'
    assert 'PRIVATE' not in saved
    assert 'PRIVATE' not in capsys.readouterr().out
    assert calls == [True]


def test_clock_does_not_certify_content_identity_and_marks_default_sequence():
    clock = load_clock()
    parsed = clock.parse_playlist(b'#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-DISCONTINUITY\n#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:01.000Z\n#EXTINF:2.000,\na.ts\n')
    assert parsed['media_sequence_tag_present'] is True
    assert parsed['discontinuity_sequence_tag_present'] is False
    assert parsed['segment_content_identity'] == 'unverified_may_include_ad_or_slate'


def transport_state(clock):
    import time
    return dict(budget=clock.ReadBudget(), requests_started=0,
                deadline=time.monotonic() + 60, playlists=[])


def mocked_http(monkeypatch, *, raw=b'#EXTM3U\n', status=200,
                location=None, final_url=None):
    import io
    import requests
    from urllib3.response import HTTPResponse
    calls, reads, responses = [], [], []

    class Body(io.BytesIO):
        def read(self, n=-1):
            reads.append(n)
            return super().read(n)

    body = Body(raw)

    def send(self, request, **kwargs):
        calls.append((request, kwargs))
        response = requests.Response()
        response.status_code = status
        response.url = final_url or request.url
        response.request = request
        response.raw = HTTPResponse(body, preload_content=False)
        if location:
            response.headers['Location'] = location
        responses.append(response)
        return response

    monkeypatch.setattr(requests.adapters.HTTPAdapter, 'send', send)
    return calls, reads, responses, body


def test_production_transport_blocks_segments_and_caps_real_api_responses(monkeypatch):
    clock = load_clock()
    raw = b'#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:1\n'
    calls, _, _, _ = mocked_http(monkeypatch, raw=raw)
    state = transport_state(clock)
    with clock.private_ydl(state) as ydl:
        with pytest.raises(PermissionError):
            ydl.urlopen('https://video.ttvnw.net/a.ts')
        assert calls == []
        with ydl.urlopen('https://usher.ttvnw.net/live.m3u8') as response:
            assert response.read() == raw
    assert state['budget'].consumed == len(raw)
    assert state['requests_started'] == 1
    assert len(state['playlists']) == 1


@pytest.mark.parametrize('status', [301, 302, 303, 307, 308])
@pytest.mark.parametrize('location', [
    'https://off-list.invalid/secret.ts?token=PRIVATE',
    'https://video.ttvnw.net/a.ts',
    'https://usher.ttvnw.net/other.m3u8',
    '/live.m3u8',
])
def test_redirects_fail_closed_before_any_body_or_second_hop(monkeypatch, status, location):
    clock = load_clock()
    calls, reads, _, body = mocked_http(monkeypatch, status=status, location=location,
                                      raw=b'x' * 4096)
    state = transport_state(clock)
    state['budget'] = clock.ReadBudget(8)
    with clock.private_ydl(state) as ydl:
        with pytest.raises(Exception):
            ydl.urlopen('https://usher.ttvnw.net/live.m3u8')
    assert len(calls) == 1
    assert reads == []
    assert body.closed
    assert state['requests_started'] == 1
    assert state['budget'].consumed == 0
    assert state['playlists'] == []
    assert state['http_error_status'] == status


@pytest.mark.parametrize('exhausted', ['bytes', 'requests', 'deadline'])
def test_exhausted_limits_prevent_http_dispatch(monkeypatch, exhausted):
    clock = load_clock()
    calls, reads, _, _ = mocked_http(monkeypatch)
    state = transport_state(clock)
    if exhausted == 'bytes':
        state['budget'].consumed = state['budget'].limit
    elif exhausted == 'requests':
        state['requests_started'] = 8
    else:
        state['deadline'] = 0
    with clock.private_ydl(state) as ydl:
        with pytest.raises((ValueError, TimeoutError)):
            ydl.urlopen('https://usher.ttvnw.net/live.m3u8')
    assert calls == reads == []


def test_changed_final_url_rejected_before_body(monkeypatch):
    clock = load_clock()
    calls, reads, _, body = mocked_http(monkeypatch, final_url='https://other.invalid/a.ts')
    state = transport_state(clock)
    with clock.private_ydl(state) as ydl:
        with pytest.raises(PermissionError):
            ydl.urlopen('https://usher.ttvnw.net/live.m3u8')
    assert len(calls) == 1
    assert reads == []
    assert body.closed


@pytest.mark.parametrize('status', [304, 401, 403, 429, 500])
def test_error_responses_are_closed_without_body_reads(monkeypatch, status):
    clock = load_clock()
    calls, reads, _, body = mocked_http(monkeypatch, status=status)
    state = transport_state(clock)
    with clock.private_ydl(state) as ydl:
        with pytest.raises(Exception):
            ydl.urlopen('https://usher.ttvnw.net/live.m3u8')
    assert len(calls) == 1
    assert reads == []
    assert body.closed
    assert state['http_error_status'] == status


@pytest.mark.parametrize('url', [
    'http://usher.ttvnw.net/live.m3u8',
    'https://ttvnw.net.evil.invalid/live.m3u8',
    'https://user:PRIVATE@usher.ttvnw.net/live.m3u8',
    'https://usher.ttvnw.net:444/live.m3u8',
    'https://usher.ttvnw.net/live.m3u8#PRIVATE',
    'https://gql.twitch.tv/not-gql',
])
def test_unapproved_origins_never_dispatch(monkeypatch, url):
    clock = load_clock()
    calls, reads, _, _ = mocked_http(monkeypatch)
    state = transport_state(clock)
    with clock.private_ydl(state) as ydl:
        with pytest.raises(PermissionError):
            ydl.urlopen(url)
    assert calls == reads == []
    assert state['requests_started'] == 0


def test_graphql_post_preserved_without_environment_auth(monkeypatch):
    import json
    from yt_dlp.networking import Request
    clock = load_clock()
    raw = json.dumps({'data': {'user': {'stream': {'id': clock.EXPECTED_STREAM_ID, 'type': 'live'}}}}).encode()
    calls, _, _, _ = mocked_http(monkeypatch, raw=raw)
    state = transport_state(clock)
    monkeypatch.setenv('HTTPS_PROXY', 'https://proxy.invalid')
    monkeypatch.setenv('NETRC', 'nonexistent-private-netrc')
    with clock.private_ydl(state) as ydl:
        request = Request('https://gql.twitch.tv/gql', data=b'{"query":"offline"}',
                          headers={'Client-ID': 'offline', 'Content-Type': 'application/json'})
        with ydl.urlopen(request) as response:
            assert response.read() == raw
    prepared, options = calls[0]
    assert prepared.method == 'POST'
    assert prepared.body == b'{"query":"offline"}'
    assert prepared.headers['Client-ID'] == 'offline'
    assert 'Authorization' not in prepared.headers
    assert 'Cookie' not in prepared.headers
    assert options['stream'] is True
    assert options['verify'] is True
    assert options['proxies'] == {}
    assert options['timeout'] == 10
    assert state['source_stream_id'] == clock.EXPECTED_STREAM_ID
    assert state['is_live'] is True
    assert state['budget'].consumed == len(raw)


def test_production_body_cap_closes_response_and_stops_future_dispatch(monkeypatch):
    clock = load_clock()
    calls, reads, _, body = mocked_http(monkeypatch, raw=b'x' * 100)
    state = transport_state(clock)
    state['budget'] = clock.ReadBudget(10)
    with clock.private_ydl(state) as ydl:
        for _ in range(2):
            with pytest.raises(ValueError, match='response_budget_exhausted'):
                ydl.urlopen('https://usher.ttvnw.net/live.m3u8')
    assert len(calls) == 1
    assert reads == [10]
    assert body.closed
    assert state['budget'].consumed == 10
    assert state['requests_started'] == 1


def test_malformed_playlist_produces_gap_not_success(tmp_path):
    import io
    import json
    clock = load_clock()
    class FakeYDL:
        def __enter__(self):
            return self
        def __exit__(self, *args):
            pass
        def extract_info(self, *args, **kwargs):
            return dict(id=clock.EXPECTED_STREAM_ID, is_live=True,
                        formats=[dict(url='https://usher.ttvnw.net/live.m3u8', vcodec='h264')])
        def urlopen(self, url):
            return io.BytesIO(('#EXTM3U\n' + PDT + SEGMENT + '#EXTINF:1.000,\n').encode())
    saved = json.loads(Path(clock.capture(tmp_path, ydl_factory=lambda state: FakeYDL())).read_text())
    assert saved['status'] == 'gap'
    assert saved['old_recording_utc_offset_ms'] is None
    assert 'clock' not in saved
