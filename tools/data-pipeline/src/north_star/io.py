"""Bounded small-object publication with a separate validated commit receipt.

Not a bulk Parquet writer: adapters must stream their own partitions. A data file
without a matching receipt is uncommitted and never silently accepted.
Destinations must be caller-controlled trusted trees; see fs_integrity for the
link policy and bounded race guarantees. Data and receipt are not one atomic
transaction: interruption or an I/O failure can still leave uncommitted data.
"""
from pathlib import Path
import hashlib
import json
import os
import tempfile

from north_star.fs_integrity import checked_path, stable_reader


def _publish_new(path, data):
    path = Path(path)
    checked_path(path, allow_missing=True, create_parents=True)
    fd, tmp = tempfile.mkstemp(prefix=path.name + '.', suffix='.pending', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        checked_path(path, allow_missing=True)
        os.link(tmp, path)
    finally:
        os.unlink(tmp)


def verify_partition(path, provenance):
    path = Path(path)
    with stable_reader(path.with_name(path.name + '.receipt.json')) as (receipt_source, _):
        receipt = json.loads(receipt_source.read().decode('utf-8'))
        if receipt['provenance'] != provenance:
            raise ValueError('Partition provenance mismatch')
        with stable_reader(path) as (source, info):
            digest = hashlib.file_digest(source, 'sha256').hexdigest()
        if digest != receipt['sha256'] or info.st_size != receipt['bytes']:
            raise ValueError('Partition integrity mismatch')
    return receipt


def _validate_provenance(value):
    """Accept only lossless JSON values, not implicit key/tuple conversions."""
    if type(value) is dict:
        for key, item in value.items():
            if type(key) is not str:
                raise TypeError('Provenance keys must be strings')
            _validate_provenance(item)
    elif type(value) is list:
        for item in value:
            _validate_provenance(item)
    elif value is not None and type(value) not in (str, int, float, bool):
        raise TypeError('Unsupported provenance value')


def publish_bytes(path, data, provenance):
    path = Path(path)
    _validate_provenance(provenance)
    receipt = {'sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data), 'provenance': provenance}
    serialized = json.dumps(receipt, sort_keys=True, allow_nan=False).encode('utf-8')
    exists = checked_path(path, allow_missing=True, create_parents=True) is not None
    receipt_path = path.with_name(path.name + '.receipt.json')
    receipt_exists = checked_path(receipt_path, allow_missing=True) is not None
    if exists:
        previous = verify_partition(path, provenance)
        if previous != receipt:
            raise ValueError('Resume payload mismatch')
        return previous
    if receipt_exists:
        raise FileExistsError('Receipt already exists without partition')
    _publish_new(path, data)
    _publish_new(path.with_name(path.name + '.receipt.json'), serialized)
    return verify_partition(path, provenance)
