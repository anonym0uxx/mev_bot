#!/usr/bin/env python
"""validate_laserstream.py — verify the raw capture against its manifest.

Checks presence + byte-size + SHA256 for every file the manifest claims (hash the
compressed bytes — no decompression). Reports missing/mismatched parts + the census.
"""
import json, os, hashlib

RAW_DIR = 'D:/mev_bot-artifacts/raw'
MANIFEST = 'D:/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/training-data/pumpfun_laserstream_manifest_v1_20260824_053543_000288.json'


def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(65536), b''):
            h.update(chunk)
    return h.hexdigest()


def main():
    m = json.load(open(MANIFEST))
    raw_files = m['raw_files']
    ev = m['events_file']

    missing, size_mismatch, hash_mismatch = [], [], []
    for i, rf in enumerate(raw_files):
        p = os.path.join(RAW_DIR, rf['filename'])
        if not os.path.exists(p):
            missing.append(rf['filename'])
            continue
        sz = os.path.getsize(p)
        if sz != rf['bytes']:
            size_mismatch.append((rf['filename'], sz, rf['bytes']))
            continue
        if sha256(p) != rf['sha256']:
            hash_mismatch.append(rf['filename'])
        if (i + 1) % 50 == 0:
            print(f'  verified {i+1}/{len(raw_files)} raw parts')

    # events file
    ep = os.path.join(RAW_DIR, ev['filename'])
    ev_ok = os.path.exists(ep) and os.path.getsize(ep) == ev['bytes']
    ev_hash_ok = ev_ok and sha256(ep) == ev['sha256']

    print(f'\n=== VALIDATION RESULT ===')
    print(f'raw parts: {len(raw_files)} claimed, {len(raw_files)-len(missing)} present')
    print(f'missing: {len(missing)}')
    for x in missing[:20]:
        print(f'  MISSING {x}')
    print(f'size mismatch: {len(size_mismatch)}')
    for fn, a, b in size_mismatch[:20]:
        print(f'  SIZE {fn}: ondisk={a} manifest={b}')
    print(f'hash mismatch (size ok): {len(hash_mismatch)}')
    for x in hash_mismatch[:20]:
        print(f'  HASH {x}')
    print(f'events file: present={os.path.exists(ep)} size={ev_ok} sha256={ev_hash_ok}')

    # census
    print(f'\n=== CENSUS (manifest) ===')
    print(f'total_raw_records: {m.get("total_raw_records"):,}')
    print(f'total_events: {m.get("total_events"):,}')
    for k, v in m.get('counts', {}).items():
        print(f'  {k}: {v:,}')
    q = m.get('quality', {})
    print(f'quality: dup={q.get("duplicates")} decode_fail={q.get("decode_failures")} unknown={q.get("unknown_events"):,} gaps={q.get("gaps")}')
    print(f'window: {m.get("start_unix_ms")} -> {m.get("end_unix_ms")} ({m.get("duration_minutes")} min) slots {m.get("start_slot")}-{m.get("end_slot")}')

    ok = not missing and not size_mismatch and not hash_mismatch and ev_ok and ev_hash_ok
    print(f'\nOVERALL: {"✓ VALID" if ok else "✗ INCOMPLETE/MISMATCH"}')


if __name__ == '__main__':
    main()