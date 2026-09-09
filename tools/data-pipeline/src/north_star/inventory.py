"""Streaming, read-only inventory integrity checks. Never deserializes sources."""
from pathlib import Path, PurePosixPath
import hashlib
import os
import re


def verify_manifest(root, rows):
    root = Path(root).resolve(strict=True)
    verified, missing, mismatched = [], [], []
    seen = set()
    for row in rows:
        name = row['path']
        p = PurePosixPath(name)
        if p.is_absolute() or '..' in p.parts or '\\' in name or ':' in name or name in seen:
            raise ValueError('Unsafe/duplicate inventory path')
        seen.add(name)
        if type(row.get('size')) is not int or row['size'] < 0 or not re.fullmatch('[0-9a-f]{64}', row.get('sha256', '')):
            raise ValueError('Invalid expected integrity metadata')
        path = root.joinpath(*p.parts)
        if not path.exists():
            missing.append(name)
            continue
        if not path.resolve().is_relative_to(root):
            raise ValueError('Inventory path escapes root')
        with path.open('rb') as source:
            before = os.fstat(source.fileno())
            digest = hashlib.file_digest(source, 'sha256').hexdigest()
            after = os.fstat(source.fileno())
        current = path.stat()
        stable = (before.st_ino, before.st_size, before.st_mtime_ns) == (after.st_ino, after.st_size, after.st_mtime_ns) == (current.st_ino, current.st_size, current.st_mtime_ns)
        result = {'path': name, 'size': after.st_size, 'sha256': digest}
        if stable and after.st_size == row['size'] and digest == row['sha256']:
            verified.append(result)
        else:
            mismatched.append(result)
    return {'expected_count': len(rows), 'verified_count': len(verified), 'missing': missing,
            'mismatched': mismatched, 'verified': verified, 'complete': not missing and not mismatched,
            'scope': 'manifest-covered byte integrity only; not rights or semantic admission'}
