"""Whole corrected capture, development-only JSONL envelopes; never admission.

Uses project_record, NOT either legacy <=100-row file/Parquet writer. Trusted
local immutable trees only; no hostile filesystem or power-loss guarantee.
Output publication is atomic no-clobber directory rename on Windows. Partial
staging directories carry no COMPLETE marker. Existing targets are never healed.
"""
import argparse
from collections import Counter
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import sys
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
import pyarrow as pa
import zstandard
from north_star import raw_adapter
from north_star.fs_integrity import checked_path, stable_reader
from north_star.partitions import ENVELOPE_SCHEMA

DEFAULT_SOURCE = Path("D:/mev_bot-artifacts/north_star/development/corrected_capture_validation_v1")
DEFAULT_OUTPUT = Path("D:/mev_bot-artifacts/north_star/aggregation/corrected_capture_envelopes_v1")
MANIFEST_NAME = "pumpfun_laserstream_manifest_v1_20260910_015052_000575.json"
MANIFEST_SHA256 = "0c80e5816948851de1d6f06c3279e732aa7d72c76707419ec14bc5050b0a758e"
RAW_PINS = [
    dict(filename="pumpfun_laserstream_raw_v1_20260910_015052_000575_part0000.ndjson.zst", bytes=33420886,
         sha256="034fd642be1cf852b422055199e1b630dd506e54ca0d1eb4e9a751f776543aba"),
    dict(filename="pumpfun_laserstream_raw_v1_20260910_015052_000575_part0001.ndjson.zst", bytes=32125909,
         sha256="237be5489a09ac722e04cd9f76c752f9cc2e4ac90bd1f474dc7a1e3deddfae2c"),
    dict(filename="pumpfun_laserstream_raw_v1_20260910_015052_000575_part0002.ndjson.zst", bytes=5365466,
         sha256="95e7e4058900937a144049cda0c88de0b10b5b051ccaa1a5c4e39c295aea6ea9"),
]
MAX_RECORD_BYTES = 8 * 1024 * 1024
MAX_OUTPUT_LINE_BYTES = 64 * 1024 * 1024


def encoded(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=True, allow_nan=False,
                      separators=(",", ":")).encode("utf-8")


def strict_json(raw):
    return json.loads(raw, object_pairs_hook=raw_adapter._object,
                      parse_constant=raw_adapter._nonfinite, parse_float=raw_adapter._float,
                      parse_int=raw_adapter._integer)


def sha(path):
    with stable_reader(path) as (f, _):
        return hashlib.file_digest(f, "sha256").hexdigest()


def write_json(path, value):
    with path.open("xb") as f:
        f.write(encoded(value) + b"\n")
        f.flush()
        os.fsync(f.fileno())


def pin_source(manifest_path, manifest_sha256, expected_raw_files, expected_records):
    if not re.fullmatch(r"[0-9a-f]{64}", manifest_sha256):
        raise ValueError("invalid manifest digest")
    if type(expected_records) is not int or expected_records < 0:
        raise ValueError("invalid producer record expectation")
    with stable_reader(manifest_path) as (f, info):
        if info.st_size > 1024 * 1024:
            raise ValueError("manifest byte bound")
        raw = f.read()
    if hashlib.sha256(raw).hexdigest() != manifest_sha256:
        raise ValueError("manifest hash mismatch")
    manifest = strict_json(raw)
    if manifest["raw_files"] != expected_raw_files or len(expected_raw_files) != 3:
        raise ValueError("explicit three-part pins mismatch")
    if (type(manifest["total_raw_records"]) is not int or
            manifest["total_raw_records"] != expected_records):
        raise ValueError("producer count declaration mismatch")
    if manifest["commitment"] not in ("CONFIRMED", "PROCESSED", "FINALIZED"):
        raise ValueError("invalid subscription commitment")
    names = set()
    for pin in [*manifest["raw_files"], manifest["events_file"]]:
        name = pin["filename"]
        if (type(name) is not str or Path(name).name != name or "/" in name or "\\" in name
                or name in (".", "..") or name in names):
            raise ValueError("unsafe/duplicate source filename")
        names.add(name)
        if (type(pin["bytes"]) is not int or pin["bytes"] < 0 or
                type(pin["sha256"]) is not str or not re.fullmatch(r"[0-9a-f]{64}", pin["sha256"])):
            raise ValueError("invalid payload pin")
        with stable_reader(manifest_path.parent / name) as (f, info):
            if info.st_size != pin["bytes"] or hashlib.file_digest(f, "sha256").hexdigest() != pin["sha256"]:
                raise ValueError("source size/hash mismatch: " + name)
    return manifest


def raw_lines(path):
    """Yield every physical line, retaining at most 8MiB plus a drain chunk.

    Oversized lines are drained/hash-accounted as ONE rejection, not split records.
    Offsets refer to decompressed bytes, never compressed seek offsets.
    """
    offset, index = 0, 0
    with stable_reader(path) as (compressed, _):
        with zstandard.ZstdDecompressor(max_window_size=128 * 1024 * 1024).stream_reader(
                compressed, read_across_frames=True, closefd=False) as stream:
            with io.BufferedReader(stream, buffer_size=65536) as reader:
                while True:
                    raw = reader.readline(MAX_RECORD_BYTES + 1)
                    if not raw:
                        break
                    digest = hashlib.sha256(raw)
                    length = len(raw)
                    oversized = length > MAX_RECORD_BYTES
                    ended = raw.endswith(b"\n")
                    if oversized:
                        raw = None
                        while not ended:
                            piece = reader.readline(65536)
                            if not piece:
                                break
                            digest.update(piece)
                            length += len(piece)
                            ended = piece.endswith(b"\n")
                    yield index, offset, length, digest.hexdigest(), raw
                    offset += length
                    index += 1


def pointer(part_index, pin, source, line):
    index, offset, length, digest, _ = line
    return dict(source_part_index=part_index, source_path=str(source), source_sha256=pin["sha256"],
                source_line_index=index, decompressed_byte_offset=offset, raw_byte_length=length,
                raw_record_sha256=digest,
                raw_record_hash_scope="exact_ndjson_bytes_including_line_terminator")


def validate_envelope(event):
    if set(event) != set(ENVELOPE_SCHEMA.names):
        raise ValueError("envelope schema columns changed")
    for field in ENVELOPE_SCHEMA:
        if event[field.name] is None and not field.nullable:
            raise ValueError("required envelope null")
    table = pa.Table.from_pylist([event], schema=ENVELOPE_SCHEMA)
    table.validate(full=True)
    restored = table.to_pylist()[0]
    restored["null_reasons"] = dict(restored["null_reasons"])
    if restored != event:
        raise ValueError("Arrow field/null roundtrip mismatch")


def project(line, ptr, commitment):
    if line[4] is None:
        return "rejected", dict(raw_pointer=ptr, reason="record_size_limit_exceeded",
                                training_eligible=False, source_admission="unadmitted")
    try:
        event = raw_adapter.project_record(line[4], source_path=ptr["source_path"],
                                          source_sha256=ptr["source_sha256"],
                                          source_commitment=commitment)
    except raw_adapter.RawProjectionError as exc:
        return "rejected", dict(raw_pointer=ptr, reason=exc.reason,
                                training_eligible=False, source_admission="unadmitted")
    event["source_line_index"] = ptr["source_line_index"]
    validate_envelope(event)
    return "accepted", dict(envelope=event, raw_pointer=ptr)


class Chunks:
    """One-row streaming writes; chunk_rows bounds files, not RAM buffering."""
    def __init__(self, root, part, kind, chunk_rows, inventory):
        self.root, self.part, self.kind = root, part, kind
        self.chunk_rows, self.inventory = chunk_rows, inventory
        self.count, self.number, self.handle, self.file = 0, 0, None, None

    def add(self, row):
        if self.handle is None:
            relative = f"part={self.part:04d}/{self.kind}/chunk{self.number:05d}.jsonl.gz"
            self.path = self.root / relative
            self.path.parent.mkdir(parents=True, exist_ok=True)
            self.file = self.path.open("xb")
            self.handle = gzip.GzipFile(filename="", mode="wb", fileobj=self.file, mtime=0, compresslevel=6)
            self.content = hashlib.sha256()
            self.count = 0
            self.relative = relative
        raw = encoded(row) + b"\n"
        if len(raw) > MAX_OUTPUT_LINE_BYTES:
            raise ValueError("output line infrastructure bound")
        self.handle.write(raw)
        self.content.update(raw)
        self.count += 1
        if self.count == self.chunk_rows:
            self.close()

    def close(self):
        if self.handle is not None:
            self.handle.close()
            self.file.flush()
            os.fsync(self.file.fileno())
            self.file.close()
            self.handle, self.file = None, None
            self.inventory.append(dict(path=self.relative, kind=self.kind, part=self.part,
                                       rows=self.count, bytes=self.path.stat().st_size,
                                       sha256=sha(self.path), content_sha256=self.content.hexdigest()))
            self.number += 1


def rows(root, report, part, kind):
    inventory = sorted((x for x in report["outputs"] if x["part"] == part and x["kind"] == kind),
                       key=lambda x: x["path"])
    for entry in inventory:
        path = root / entry["path"]
        if checked_path(path).st_size != entry["bytes"] or sha(path) != entry["sha256"]:
            raise ValueError("output compressed hash/size mismatch")
        count, digest = 0, hashlib.sha256()
        with stable_reader(path) as (f, _):
            with gzip.GzipFile(fileobj=f, mode="rb") as stream:
                while True:
                    line = stream.readline(MAX_OUTPUT_LINE_BYTES + 1)
                    if not line:
                        break
                    if len(line) > MAX_OUTPUT_LINE_BYTES or not line.endswith(b"\n"):
                        raise ValueError("output line bound/termination")
                    digest.update(line)
                    count += 1
                    yield strict_json(line)
        if count != entry["rows"] or digest.hexdigest() != entry["content_sha256"]:
            raise ValueError("output content/count mismatch")


def _verify(root, report):
    manifest_path = Path(report["manifest_path"])
    manifest = pin_source(manifest_path, report["manifest_sha256"], report["source_raw_pins"],
                          report["producer_records"])
    counts, kinds, reasons = Counter(), Counter(), Counter()
    chain = hashlib.sha256()
    actual_parts = []
    for part, pin in enumerate(manifest["raw_files"]):
        streams = {kind: iter(rows(root, report, part, kind)) for kind in ("accepted", "rejected", "ledger")}
        part_counts, byte_length, part_chain = Counter(), 0, hashlib.sha256()
        for line in raw_lines(manifest_path.parent / pin["filename"]):
            ptr = pointer(part, pin, manifest_path.parent / pin["filename"], line)
            kind, expected = project(line, ptr, manifest["commitment"])
            if next(streams[kind], None) != expected:
                raise ValueError("raw replay / envelope or rejection mismatch")
            if next(streams["ledger"], None) != dict(raw_pointer=ptr, disposition=kind):
                raise ValueError("raw pointer conservation mismatch")
            token = encoded(ptr) + b"\n"
            chain.update(token)
            part_chain.update(token)
            counts[kind] += 1
            part_counts[kind] += 1
            byte_length += line[2]
            if kind == "accepted":
                kinds[expected["envelope"]["record_type"]] += 1
            else:
                reasons[expected["reason"]] += 1
        for stream in streams.values():
            if next(stream, None) is not None:
                raise ValueError("extra output row")
        actual_parts.append(dict(source_part_index=part, rows_read=sum(part_counts.values()),
                                 accepted=part_counts["accepted"], rejected=part_counts["rejected"],
                                 decompressed_bytes=byte_length, eof_observed=True,
                                 pointer_sequence_sha256=part_chain.hexdigest()))
    total = sum(counts.values())
    if (total != manifest["total_raw_records"] or total != report["rows_read"] or
            counts["accepted"] != report["accepted"] or counts["rejected"] != report["rejected"] or
            dict(kinds) != report["record_types"] or dict(reasons) != report["rejection_reasons"] or
            actual_parts != report["raw_parts"] or chain.hexdigest() != report["pointer_sequence_sha256"]):
        raise ValueError("producer/output count or hash conservation mismatch")
    expected_inventory = {x["path"] for x in report["outputs"]}
    if len(expected_inventory) != len(report["outputs"]):
        raise ValueError("duplicate output inventory")
    actual_inventory = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file()}
    if actual_inventory - {"REPORT.json", "COMPLETE.json"} != expected_inventory:
        raise ValueError("exact output inventory mismatch")
    return report


def verify(output_dir):
    """Read-only completed-run verification with full raw replay; never repair."""
    root = Path(output_dir)
    with stable_reader(root / "COMPLETE.json") as (f, _):
        complete = strict_json(f.read(1024 * 1024 + 1))
    if complete != dict(complete=True, report_sha256=sha(root / "REPORT.json")):
        raise ValueError("completion receipt mismatch")
    with stable_reader(root / "REPORT.json") as (f, _):
        report = strict_json(f.read(8 * 1024 * 1024 + 1))
    if (report["complete"] is not True or report["training_eligible"] is not False or
            report["source_admission"] != "unadmitted" or report["producer_count_matches"] is not True):
        raise ValueError("invalid completion/development status")
    return _verify(root, report)


def aggregate(*, manifest_path, manifest_sha256, expected_raw_files, expected_records,
              output_dir, chunk_rows=10000):
    if os.name != "nt":
        raise RuntimeError("no-clobber directory publication implemented for Windows only")
    if type(chunk_rows) is not int or not 1 <= chunk_rows <= 10000:
        raise ValueError("invalid chunk rows")
    manifest_path, output = Path(manifest_path).absolute(), Path(output_dir).absolute()
    if (output.is_relative_to(manifest_path.parent) or manifest_path.is_relative_to(output)):
        raise ValueError("source/output overlap")
    # Check the target and ancestors without resolving away links/reparse points.
    if checked_path(output, allow_missing=True, create_parents=True) is not None:
        raise FileExistsError(output)
    manifest = pin_source(manifest_path, manifest_sha256, expected_raw_files, expected_records)
    stage = output.with_name(output.name + ".pending-" + uuid.uuid4().hex)
    stage.mkdir()
    inventory, parts, counts, kinds, reasons = [], [], Counter(), Counter(), Counter()
    chain, writers = hashlib.sha256(), []
    try:
        for part, pin in enumerate(manifest["raw_files"]):
            streams = {kind: Chunks(stage, part, kind, chunk_rows, inventory)
                       for kind in ("accepted", "rejected", "ledger")}
            writers.extend(streams.values())
            part_counts, byte_length, part_chain = Counter(), 0, hashlib.sha256()
            for line in raw_lines(manifest_path.parent / pin["filename"]):
                ptr = pointer(part, pin, manifest_path.parent / pin["filename"], line)
                kind, row = project(line, ptr, manifest["commitment"])
                streams[kind].add(row)
                streams["ledger"].add(dict(raw_pointer=ptr, disposition=kind))
                token = encoded(ptr) + b"\n"
                chain.update(token)
                part_chain.update(token)
                counts[kind] += 1
                part_counts[kind] += 1
                byte_length += line[2]
                if kind == "accepted":
                    kinds[row["envelope"]["record_type"]] += 1
                else:
                    reasons[row["reason"]] += 1
            for stream in streams.values():
                stream.close()
            parts.append(dict(source_part_index=part, rows_read=sum(part_counts.values()),
                              accepted=part_counts["accepted"], rejected=part_counts["rejected"],
                              decompressed_bytes=byte_length, eof_observed=True,
                              pointer_sequence_sha256=part_chain.hexdigest()))
            print(json.dumps(dict(part=part, **part_counts, rows_read=sum(part_counts.values()))), flush=True)
        if sum(counts.values()) != expected_records:
            raise ValueError(f"producer_count_mismatch: observed={sum(counts.values())} expected={expected_records}")
        report = dict(schema_version="corrected_capture_envelope_aggregation_v1", complete=True,
                      session_id=manifest["session_id"], projection_scope="envelope_only_development",
                      training_eligible=False, source_admission="unadmitted",
                      exposure="new_prospective_validation_development_source_not_original_EVALv2",
                      policy_acceptance_claimed=False, venue_truth_claimed=False, label_truth_claimed=False,
                      manifest_path=str(manifest_path), manifest_sha256=manifest_sha256,
                      source_raw_pins=expected_raw_files, all_four_payload_pins_verified_before_semantics=True,
                      source_commitment=manifest["commitment"], producer_records=expected_records,
                      producer_count_matches=True, rows_read=sum(counts.values()), accepted=counts["accepted"],
                      rejected=counts["rejected"], record_types=dict(kinds), rejection_reasons=dict(reasons),
                      raw_parts=parts, pointer_sequence_sha256=chain.hexdigest(), outputs=inventory,
                      full_source_decoded=True, original_sources_untouched=True,
                      raw_pointer_offset_scope="zero_based_decompressed_bytes_per_pinned_raw_part",
                      chunk_rows=chunk_rows, buffered_records=1, max_record_bytes=MAX_RECORD_BYTES,
                      schema_sha256=hashlib.sha256(ENVELOPE_SCHEMA.serialize().to_pybytes()).hexdigest(),
                      adapter_sha256=sha(Path(raw_adapter.__file__)), driver_sha256=sha(Path(__file__)),
                      schema_provider_sha256=sha(Path(raw_adapter.__file__).with_name("partitions.py")),
                      legacy_bounded_writer_called=False, pyarrow_version=pa.__version__,
                      zstandard_version=zstandard.__version__, verification="full_raw_replay_exact_envelopes_rejections_pointers")
        write_json(stage / "REPORT.json", report)
        _verify(stage, report)
        write_json(stage / "COMPLETE.json", dict(complete=True, report_sha256=sha(stage / "REPORT.json")))
        # On Windows os.rename refuses any existing destination, including empty dirs.
        os.rename(stage, output)
        return report
    except BaseException as exc:
        for writer in writers:
            try:
                writer.close()
            except Exception:
                pass
        (stage / "COMPLETE.json").unlink(missing_ok=True)
        if stage.exists():
            write_json(stage / "FAILED.json", dict(complete=False, error_type=type(exc).__name__,
                       error=str(exc), rows_read=sum(counts.values()), accepted=counts["accepted"],
                       rejected=counts["rejected"], producer_records=expected_records,
                       producer_count_matches=sum(counts.values()) == expected_records))
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify-only", action="store_true")
    args = parser.parse_args()
    if args.verify_only:
        report = verify(DEFAULT_OUTPUT)
    else:
        report = aggregate(manifest_path=DEFAULT_SOURCE / MANIFEST_NAME, manifest_sha256=MANIFEST_SHA256,
                           expected_raw_files=RAW_PINS, expected_records=107912, output_dir=DEFAULT_OUTPUT)
    print(json.dumps({key: report[key] for key in ("complete", "rows_read", "accepted", "rejected",
                    "record_types", "rejection_reasons", "pointer_sequence_sha256")}, sort_keys=True))


if __name__ == "__main__":
    main()
