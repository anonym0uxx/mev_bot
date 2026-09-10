"""Prospective HLS clock evidence; never establishes an existing file's PTS epoch."""
from datetime import datetime, timezone
import re


# Local supported-subset bound, not a general HLS/RFC3339 length limit.
MAX_CLOCK_LITERAL_CHARS = 64


def exact_ms(value):
    """Unsigned ASCII decimal seconds, exactly representable in milliseconds.

    Inspect every fractional digit before conversion; Decimal arithmetic can
    round under the caller's context, even when the input was parsed exactly.
    """
    if (not isinstance(value, str) or len(value) > MAX_CLOCK_LITERAL_CHARS
            or not re.fullmatch(r'[0-9]+(?:\.[0-9]+)?', value)):
        raise ValueError('duration_not_exact_nonnegative_milliseconds')
    whole, _, fraction = value.partition('.')
    if any(digit != '0' for digit in fraction[3:]):
        raise ValueError('duration_not_exact_nonnegative_milliseconds')
    return int(whole) * 1000 + int(fraction[:3].ljust(3, '0'))


def utc_ms(value):
    """Exact nonnegative Unix ms from a bounded RFC3339 clock subset.

    Require uppercase T/Z, ASCII fields, a known offset, Gregorian years
    0001..9999 and seconds 00..59 (no leap-second table is available).
    Never let fromisoformat normalize offset minutes or truncate fractions.
    """
    if not isinstance(value, str) or len(value) > MAX_CLOCK_LITERAL_CHARS:
        raise ValueError('invalid_pdt')
    match = re.fullmatch(
        r'([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2})'
        r'(?:\.([0-9]+))?(Z|[+-][0-9]{2}:[0-9]{2})', value)
    if match is None:
        raise ValueError('invalid_pdt')
    fraction, offset = match[7] or '', match[8]
    if any(digit != '0' for digit in fraction[3:]):
        raise ValueError('pdt_not_exact_milliseconds')
    offset_ms = 0
    if offset != 'Z':
        hours, minutes = int(offset[1:3]), int(offset[4:6])
        if hours > 23 or minutes > 59 or offset == '-00:00':
            raise ValueError('invalid_or_unknown_pdt_offset')
        offset_ms = (hours * 60 + minutes) * 60000
        if offset[0] == '-':
            offset_ms = -offset_ms
    try:
        date = datetime(*(int(match[i]) for i in range(1, 7)))
    except ValueError:
        raise ValueError('invalid_pdt_calendar_or_time') from None
    delta = date - datetime(1970, 1, 1)
    result = (delta.days * 86400000 + delta.seconds * 1000
              + int(fraction[:3].ljust(3, '0')) - offset_ms)
    if result < 0:
        raise ValueError('pdt_before_unix_epoch')
    return result


def parse_playlist(raw):
    """Strict complete-segment clock subset, not a general HLS validator.

    Reject unimplemented EXT syntax rather than erase partial/delta semantics.
    Within a discontinuity, subsequent PDT must equal the EXTINF-derived clock;
    even forward jumps fail closed (no rounding/drift tolerance is asserted).
    """
    lines = raw.decode('utf-8-sig').splitlines()
    if not lines or lines[0].strip() != '#EXTM3U':
        raise ValueError('not_hls_playlist')
    media = discontinuity = sequence = current_discontinuity = 0
    clock = duration = explicit = None
    segments, anchors, seen = [], [], set()
    started = pending_discontinuity = ended = False
    uint_max = (1 << 64) - 1

    def uint(value):
        if not re.fullmatch(r'[0-9]{1,20}', value) or int(value) > uint_max:
            raise ValueError('invalid_unsigned_sequence_or_integer')
        return int(value)

    for text in lines[1:]:
        line = text.strip()
        if not line or (line.startswith('#') and not line.startswith('#EXT')):
            continue
        if ended:
            raise ValueError('content_after_endlist')
        tag, _, value = line.partition(':')
        if tag in ('#EXT-X-MEDIA-SEQUENCE', '#EXT-X-DISCONTINUITY-SEQUENCE',
                   '#EXT-X-VERSION', '#EXT-X-TARGETDURATION',
                   '#EXT-X-PLAYLIST-TYPE', '#EXT-X-INDEPENDENT-SEGMENTS'):
            if tag in seen or started:
                raise ValueError('duplicate_or_misplaced_header_tag')
            seen.add(tag)
            if tag == '#EXT-X-MEDIA-SEQUENCE':
                media = sequence = uint(value)
            elif tag == '#EXT-X-DISCONTINUITY-SEQUENCE':
                discontinuity = current_discontinuity = uint(value)
            elif tag in ('#EXT-X-VERSION', '#EXT-X-TARGETDURATION'):
                if uint(value) == 0:
                    raise ValueError('invalid_positive_header_integer')
            elif tag == '#EXT-X-PLAYLIST-TYPE':
                if value not in ('EVENT', 'VOD'):
                    raise ValueError('invalid_playlist_type')
            elif line != '#EXT-X-INDEPENDENT-SEGMENTS':
                raise ValueError('invalid_independent_segments_tag')
        elif line == '#EXT-X-DISCONTINUITY':
            if duration is not None or explicit is not None or pending_discontinuity:
                raise ValueError('misplaced_discontinuity')
            started = pending_discontinuity = True
            current_discontinuity += 1
            if current_discontinuity > uint_max:
                raise ValueError('discontinuity_sequence_overflow')
            clock = None
        elif tag == '#EXT-X-PROGRAM-DATE-TIME':
            if explicit is not None:
                raise ValueError('duplicate_pdt_for_segment')
            new_clock = utc_ms(value)
            if clock is not None and new_clock != clock:
                raise ValueError('contradictory_pdt_within_discontinuity')
            started = True
            explicit, clock = value, new_clock
        elif tag == '#EXTINF':
            if duration is not None:
                raise ValueError('duplicate_extinf')
            if not re.fullmatch(r'[0-9]+(?:\.[0-9]+)?,.*', value):
                raise ValueError('invalid_extinf_syntax')
            duration = exact_ms(value.split(',', 1)[0])
            started = True
        elif line == '#EXT-X-ENDLIST':
            if duration is not None or explicit is not None or pending_discontinuity:
                raise ValueError('endlist_before_segment_uri')
            ended = True
        elif line.startswith('#'):
            raise ValueError('unsupported_or_malformed_hls_tag')
        else:
            if duration is None:
                raise ValueError('segment_uri_without_extinf')
            if sequence > uint_max:
                raise ValueError('media_sequence_overflow')
            segment = dict(media_sequence=sequence, discontinuity_sequence=current_discontinuity,
                           duration_ms=duration, program_date_time_ms=clock,
                           clock_basis='explicit_pdt' if explicit else 'extinf_from_pdt' if clock is not None else 'unknown')
            if explicit:
                anchors.append(dict(segment, program_date_time=explicit))
            segments.append(segment)
            clock = clock + duration if clock is not None else None
            duration = explicit = None
            pending_discontinuity = False
            sequence += 1
    if duration is not None or explicit is not None or pending_discontinuity:
        raise ValueError('dangling_segment_tags')
    return dict(media_sequence=media, discontinuity_sequence=discontinuity,
                media_sequence_tag_present='#EXT-X-MEDIA-SEQUENCE' in seen,
                discontinuity_sequence_tag_present='#EXT-X-DISCONTINUITY-SEQUENCE' in seen,
                segment_content_identity='unverified_may_include_ad_or_slate',
                segments=segments, anchors=anchors, ended=ended,
                old_recording_utc_offset_ms=None)


EXPECTED_STREAM_ID = '320254509148'
MAX_BYTES = 2_000_000


def stream_comparison(source_id, is_live):
    if is_live is False:
        return 'ended_or_offline'
    if source_id is None:
        return 'unverified'
    if str(source_id) != EXPECTED_STREAM_ID:
        return 'different_stream'
    return 'matching_live_stream' if is_live else 'unverified'


def safe_playlist(raw):
    """Allowlist only clock syntax; redact ALL other tags/attributes and URIs."""
    safe = []
    for text in raw.decode('utf-8-sig').splitlines():
        line = text.strip()
        if not line:
            safe.append('')
        elif line in ('#EXTM3U', '#EXT-X-DISCONTINUITY', '#EXT-X-ENDLIST'):
            safe.append(line)
        elif re.fullmatch(r'#EXT-X-(?:MEDIA-SEQUENCE|DISCONTINUITY-SEQUENCE|VERSION|TARGETDURATION):\d+', line):
            safe.append(line)
        elif line.startswith('#EXT-X-PROGRAM-DATE-TIME:'):
            utc_ms(line.split(':', 1)[1])
            safe.append(line)
        elif line.startswith('#EXTINF:'):
            value = line.split(':', 1)[1].split(',', 1)[0]
            exact_ms(value)
            safe.append('#EXTINF:' + value + ',')
        elif line.startswith('#'):
            safe.append('# REDACTED-TAG')
        else:
            safe.append('[REDACTED-URI]')
    return '\n'.join(safe) + '\n'


def playlist_evidence(raw, started_ms, completed_ms, kind):
    import hashlib
    return dict(kind=kind, original_sha256=hashlib.sha256(raw).hexdigest(),
                original_byte_count=len(raw), retrieval_started_unix_ms=started_ms,
                retrieval_completed_unix_ms=completed_ms,
                safe_playlist=safe_playlist(raw))


class ReadBudget:
    """Aggregate decoded-body cap, including extractor JSON and master playlist."""
    def __init__(self, limit=MAX_BYTES):
        self.limit = limit
        self.consumed = 0

    def read(self, response, deadline=None):
        import time
        chunks = []
        while self.consumed < self.limit:
            if deadline is not None and time.monotonic() >= deadline:
                raise TimeoutError('capture_deadline')
            chunk = response.read(min(65536, self.limit - self.consumed))
            if not chunk:
                return b''.join(chunks)
            chunks.append(chunk)
            self.consumed += len(chunk)
        # Conservative boundary: do not read even one byte beyond the cap.
        raise ValueError('response_budget_exhausted')


def persist_capture(root, result):
    import json
    import uuid
    from pathlib import Path
    directory = Path(root) / (datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S%fZ') + '_' + uuid.uuid4().hex[:8])
    directory.mkdir(parents=True, exist_ok=False)
    receipt = dict(result, expected_stream_id=EXPECTED_STREAM_ID,
                   old_recording_utc_offset_ms=None, segments_downloaded=0,
                   collector_control_performed=False, continuous_capture=False,
                   provenance='public_twitch_hls_snapshot_not_media_pts',
                   clock_accuracy='local_wall_clock_and_publisher_pdt_not_independently_calibrated')
    receipt['playlists'] = []
    for index, entry in enumerate(result.get('playlists', [])):
        entry = dict(entry)
        safe = entry.pop('safe_playlist')
        handle = directory / f'playlist_{index:02d}.redacted.m3u8'
        handle.write_text(safe, encoding='utf-8', newline='\n')
        entry['redacted_playlist_path'] = str(handle)
        receipt['playlists'].append(entry)
    handle = directory / 'receipt.json'
    handle.write_text(json.dumps(receipt, indent=2) + '\n', encoding='utf-8')
    return str(handle)


def private_ydl(state):
    """No user config/cookies. Buffer only bounded public JSON or HLS responses."""
    import io
    import json
    import time
    from urllib.parse import urlsplit
    import yt_dlp
    from yt_dlp.networking import Response
    import requests

    def validate_url(url):
        parsed = urlsplit(url)
        host = parsed.hostname or ''
        permitted_host = host == 'gql.twitch.tv' or host.endswith('.twitch.tv') or host.endswith('.ttvnw.net')
        permitted_path = (host == 'gql.twitch.tv' and parsed.path == '/gql') or parsed.path.endswith('.m3u8')
        if (parsed.scheme != 'https' or not permitted_host or not permitted_path
                or parsed.username is not None or parsed.password is not None
                or parsed.port not in (None, 443) or parsed.fragment):
            raise PermissionError('non_metadata_or_playlist_request_blocked')
        return parsed

    def reject_before_body(response, *args, **kwargs):
        # requests consumes redirect bodies even with allow_redirects=False
        # while preparing Response.next. A response hook must abort BEFORE that.
        try:
            validate_url(response.url)
            if response.url != response.request.url:
                raise PermissionError('unexpected_response_destination')
            if not 200 <= response.status_code < 300:
                state['http_error_status'] = response.status_code
                raise PermissionError('http_status_rejected_without_body')
        except Exception:
            response.close()
            raise
        return response

    class SilentLogger:
        def debug(self, *args, **kwargs):
            pass
        info = warning = error = debug

    class BoundedYDL(yt_dlp.YoutubeDL):
        def urlopen(self, request):
            url = request if isinstance(request, str) else request.url
            parsed = validate_url(url)
            host = parsed.hostname or ''
            if state['requests_started'] >= 8 or time.monotonic() >= state['deadline']:
                raise TimeoutError('capture_request_or_time_limit')
            if state['budget'].consumed >= state['budget'].limit:
                raise ValueError('response_budget_exhausted')
            # Deliberately bypass yt-dlp's auto-redirecting request director.
            # Isolated session: no netrc, environment proxies, or saved cookies.
            with requests.Session() as session:
                session.trust_env = False
                session.mount('https://', requests.adapters.HTTPAdapter(max_retries=0))
                headers = dict(self.params.get('http_headers') or {})
                headers.update(getattr(request, 'headers', {}))
                prepared = session.prepare_request(requests.Request(
                    method=getattr(request, 'method', 'GET'), url=url,
                    data=getattr(request, 'data', None), headers=headers,
                    hooks={'response': reject_before_body}))
                validate_url(prepared.url)
                if state['requests_started'] >= 8 or time.monotonic() >= state['deadline']:
                    raise TimeoutError('capture_request_or_time_limit')
                state['requests_started'] += 1
                started = time.time_ns() // 1_000_000
                with session.send(prepared, allow_redirects=False, stream=True,
                                  timeout=10, verify=True, proxies={}) as response:
                    response.raw.decode_content = True
                    raw = state['budget'].read(response.raw, state['deadline'])
                    completed = time.time_ns() // 1_000_000
                    headers = dict(response.headers)
                    status = response.status_code
                    final_url = response.url
            if parsed.path.endswith('.m3u8'):
                kind = 'master' if b'#EXT-X-STREAM-INF:' in raw else 'media'
                state['playlists'].append(playlist_evidence(raw, started, completed, kind))
            elif host == 'gql.twitch.tv':
                data = json.loads(raw)
                for entry in data if isinstance(data, list) else [data]:
                    user = (entry.get('data') or {}).get('user')
                    if isinstance(user, dict) and 'stream' in user:
                        stream = user['stream']
                        if stream is None:
                            state['is_live'] = False
                        elif isinstance(stream, dict) and 'id' in stream:
                            source_id = str(stream['id'])
                            if source_id.isdecimal():
                                state['source_stream_id'] = source_id
                            state['is_live'] = stream.get('type') == 'live' if 'type' in stream else True
            # Bodies were decoded by the HTTP client; do not retain encoding headers.
            headers = {k: v for k, v in headers.items() if k.lower() not in ('content-encoding', 'content-length')}
            headers['Content-Length'] = str(len(raw))
            return Response(io.BytesIO(raw), final_url, headers, status=status)

    return BoundedYDL(dict(quiet=True, no_warnings=True, logger=SilentLogger(),
                          skip_download=True, noplaylist=True, cachedir=False,
                          socket_timeout=10, retries=0, extractor_retries=0,
                          fragment_retries=0, file_access_retries=0,
                          cookiefile=None, cookiesfrombrowser=None,
                          username=None, password=None, verbose=False,
                          writeinfojson=False, writethumbnail=False,
                          live_from_start=False, nocheckcertificate=False))


def capture(root, *, ydl_factory=None):
    """One snapshot, no retry loop, safe gap receipt on denial/offline/error."""
    import contextlib
    import os
    import time
    state = dict(budget=ReadBudget(), requests_started=0, deadline=time.monotonic() + 60,
                 playlists=[], source_stream_id=None, is_live=None)
    result = dict(status='gap', gap_reason=None, source_stream_id=None,
                  stream_comparison='unverified', capture_started_unix_ms=time.time_ns() // 1_000_000)
    try:
        with open(os.devnull, 'w') as sink, contextlib.redirect_stdout(sink), contextlib.redirect_stderr(sink):
            with (ydl_factory or private_ydl)(state) as ydl:
                info = ydl.extract_info('https://www.twitch.tv/megga', download=False, process=False)
                source_id = str(info.get('id', ''))
                if not source_id.isdecimal():
                    raise ValueError('unverified_source_stream_id')
                state['source_stream_id'] = source_id
                state['is_live'] = info.get('is_live')
                comparison = stream_comparison(source_id, state['is_live'])
                result['stream_comparison'] = comparison
                if comparison != 'matching_live_stream':
                    result['gap_reason'] = comparison
                else:
                    formats = [f for f in info.get('formats', []) if f.get('url') and f.get('vcodec') != 'none']
                    if not formats:
                        raise ValueError('no_video_playlist')
                    chosen = max(formats, key=lambda f: (f.get('height') or 0, f.get('tbr') or 0))
                    started = time.time_ns() // 1_000_000
                    with ydl.urlopen(chosen['url']) as response:
                        raw = response.read(MAX_BYTES)
                        if len(raw) >= MAX_BYTES:
                            raise ValueError('playlist_budget_exhausted')
                    completed = time.time_ns() // 1_000_000
                    # Production transport already collected the precise retrieval evidence.
                    if not state['playlists'] or state['playlists'][-1]['kind'] != 'media':
                        state['playlists'].append(playlist_evidence(raw, started, completed, 'media'))
                    result['clock'] = parse_playlist(raw)
                    if result['clock']['ended']:
                        result['gap_reason'] = 'playlist_ended'
                    elif not result['clock']['anchors']:
                        result['gap_reason'] = 'playlist_missing_explicit_pdt'
                    else:
                        result['status'] = 'prospective_anchors_captured'
    except Exception as exc:
        # NEVER persist exception text: yt-dlp errors may embed signed URLs.
        result['gap_reason'] = 'metadata_or_playlist_unavailable'
        result['error_type'] = type(exc).__name__
    result['source_stream_id'] = state['source_stream_id']
    result['stream_comparison'] = stream_comparison(state['source_stream_id'], state['is_live'])
    if result['stream_comparison'] in ('ended_or_offline', 'different_stream'):
        result['status'] = 'gap'
        result['gap_reason'] = result['stream_comparison']
    if 'http_error_status' in state:
        result['http_error_status'] = state['http_error_status']
    result.update(playlists=state['playlists'], network_body_bytes=state['budget'].consumed,
                  network_request_count=state['requests_started'], network_body_budget_bytes=MAX_BYTES,
                  request_socket_timeout_seconds=10, capture_soft_deadline_seconds=60,
                  capture_completed_unix_ms=time.time_ns() // 1_000_000)
    return persist_capture(root, result)


if __name__ == '__main__':
    import argparse
    import json
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output-root', default='D:/mev_bot-artifacts/north_star/development/hls_clock')
    args = parser.parse_args()
    print(json.dumps({'receipt': capture(args.output_root)}))
