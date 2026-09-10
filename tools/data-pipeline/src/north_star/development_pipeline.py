"""Composition of bounded reviewed helpers, permanently unadmitted development.

No recovery transform is imported: the reviewed-status-pending legacy recovery is
an immutable INPUT. No training/export, new source discovery or source deletion.
"""
from pathlib import Path
import hashlib
import json
import re

from north_star.fs_integrity import stable_reader
from north_star.splits import EvaluationBoundary
from north_star.reservation import ProspectiveReservation

ROOT = Path(__file__).resolve().parents[2]
_OUTPUT_FILES = frozenset((
    "identity.json", "raw/events.jsonl", "raw/report.json", "events.parquet",
    "events.parquet.receipt.json", "transactions/reconciled.ndjson",
    "transactions/rejected.ndjson", "transactions/receipt.json"))
RAW_SOURCE_ID = "urn:laserstream:session:20260909_144906_000490"
NARRATIVE_SOURCE_ID = "tools/data-pipeline/output/narrative_gold_v1.1/gold/FREEZE_MARKER.json"
POLICIES = {
    "EVAL_FREEZE.json": ("309d4b305b3e9dc7a2fd851997613041ca2c96f72a6fb6ab6dfac3252bb86ed3", "2e192cc90ccc331202f511667a58aefef8ed9326d11f1a502cd928191378aeaa"),
    "EVAL_RESERVATION_V3.json": ("a78b6efc2df16a69e52dc7fe33728fba2a2768572d62bc05cde978e375cbcb82", "8d018de9c530b97595d5a3ec9c69d92df497be16c882df12662398b6a8476fb8"),
}


def _bytes(path, ceiling=1024 * 1024):
    with stable_reader(path) as (source, info):
        if info.st_size > ceiling:
            raise ValueError("file byte ceiling exceeded")
        return source.read(ceiling + 1)


def exposure_gate(raw_source_id, narrative_source_id):
    """Exact exposed dataset identities, NOT provider rights or entity admission."""
    docs = {}
    for name, (file_pin, canonical_pin) in POLICIES.items():
        data = _bytes(ROOT / "north_star" / name)
        if hashlib.sha256(data).hexdigest() != file_pin:
            raise ValueError("policy file hash mismatch")
        document = json.loads(data)
        field = "policy_sha256" if name == "EVAL_FREEZE.json" else "reservation_sha256"
        if document.pop(field) != canonical_pin:
            raise ValueError("policy canonical pin mismatch")
        docs[name] = document
    base = EvaluationBoundary(docs["EVAL_FREEZE.json"], expected_sha256=POLICIES["EVAL_FREEZE.json"][1])
    reservation = ProspectiveReservation(docs["EVAL_RESERVATION_V3.json"],
        expected_sha256=POLICIES["EVAL_RESERVATION_V3.json"][1], base_boundary=base)
    if (raw_source_id, narrative_source_id) != (RAW_SOURCE_ID, NARRATIVE_SOURCE_ID):
        raise ValueError("excluded or unknown source identity")
    for source in (raw_source_id, narrative_source_id):
        if (base.document["source_assignments"].get(source) != "DEVELOPMENT"
                or source not in base.document["exposed_sources"]):
            raise ValueError("unknown or excluded source identity")
    return {"split": "DEVELOPMENT", "source_ids": [raw_source_id, narrative_source_id],
            "policy_pins": {name: {"file_sha256": pins[0], "canonical_sha256": pins[1]}
                            for name, pins in POLICIES.items()},
            "reserved_window": docs["EVAL_RESERVATION_V3.json"]["window"],
            "fit_authorized": False, "new_source_exposure_authorized": False,
            "reservation_routing": reservation.assign(source_id=raw_source_id,
                entity_ids=[], previously_exposed=True),
            "scope": "exact_existing_exposed_dataset_inputs_not_provider_or_entity_admission"}


# Literal binding of already-inspected identities to immutable bytes; no caller-
# supplied label can bless another input path, even if its content is identical.
RAW_PATH = "D:/mev_bot-artifacts/north_star/capture/20260909_144906_000490/pumpfun_laserstream_raw_v1_20260909_144906_000490_part0000.ndjson.zst"
RAW_SHA256 = "85a02eb32405d8e0ddd96f463ceaea1f52951161298e9f1f027e854e4935543f"
RECOVERY_PATH = "D:/mev_bot-artifacts/north_star/development/narrative_sources/first50_exact_v1/manifest.json"
RECOVERY_SHA256 = "65af14eb5a4707b7348fff0a81fe09728385e89a1ea056faef3dba8eb3287040"
_APPROVED_INPUTS = {(RAW_PATH, RAW_SHA256, RECOVERY_PATH, RECOVERY_SHA256)}

from importlib.metadata import version
import platform
from north_star import raw_adapter, partitions, transaction_truth
from north_star.fs_integrity import checked_path
from north_star.io import _publish_new
from north_star.splits import canonical_sha256


def default_config():
    return {"raw_path": RAW_PATH, "raw_sha256": RAW_SHA256,
            "recovery_manifest_path": RECOVERY_PATH, "recovery_manifest_sha256": RECOVERY_SHA256,
            "raw_source_id": RAW_SOURCE_ID, "narrative_source_id": NARRATIVE_SOURCE_ID,
            "raw_limit": 100, "transaction_limit": 10, "recovery_limit": 50,
            "batch_rows": 32, "source_commitment": None}


def _config(config):
    if type(config) is not dict or set(config) != set(default_config()):
        raise ValueError("invalid config keys")
    for key, bound in (("raw_limit", 100), ("transaction_limit", 10), ("recovery_limit", 50)):
        if type(config[key]) is not int or config[key] != bound:
            raise ValueError("invalid fixed bounded config: " + key)
    if type(config["batch_rows"]) is not int or not 1 <= config["batch_rows"] <= 100:
        raise ValueError("invalid config batch_rows")
    if config["source_commitment"] not in (None, "CONFIRMED"):
        raise ValueError("invalid config commitment")
    for key in ("raw_path", "raw_sha256", "recovery_manifest_path", "recovery_manifest_sha256",
                "raw_source_id", "narrative_source_id"):
        if type(config[key]) is not str:
            raise ValueError("invalid config literal")
    identity = tuple(config[key] for key in ("raw_path", "raw_sha256", "recovery_manifest_path", "recovery_manifest_sha256"))
    if identity not in _APPROVED_INPUTS:
        raise ValueError("not an approved input path/hash binding")
    return json.loads(json.dumps(config, allow_nan=False))


def _file(path, ceiling=128 * 1024 * 1024):
    with stable_reader(path) as (source, info):
        if info.st_size > ceiling:
            raise ValueError("file byte ceiling exceeded")
        return {"sha256": hashlib.file_digest(source, "sha256").hexdigest(), "bytes": info.st_size}


def _json(path, ceiling=1024 * 1024):
    return partitions._json(_bytes(path, ceiling))


def _relative(root, name):
    path = Path(name)
    if (not name or path.is_absolute() or path.drive or ".." in path.parts
            or "\\" in name or ":" in name or path.as_posix() != name):
        raise ValueError("unsafe manifest relative path")
    result = root / path
    checked_path(result)
    return result


def _verify_files(root, files):
    if type(files) is not dict or not files or len(files) > 200:
        raise ValueError("invalid manifest file count")
    for name, record in files.items():
        if _file(_relative(root, name)) != record:
            raise ValueError("file integrity/hash mismatch: " + name)


SOURCE_MANIFEST_PATH = str(Path(RAW_PATH).with_name("pumpfun_laserstream_manifest_v1_20260909_144906_000490.json"))
SOURCE_MANIFEST_SHA256 = "15e1c691b49b03fde0f4fdac4c613fc9941ada2e972b23f360109ce4376461e3"


def _source_binding(config):
    # Acquisition metadata, not matching filenames alone, proves the exact local
    # raw object belongs to this exposed session. Historical raw is not eligible.
    record = _file(SOURCE_MANIFEST_PATH, 1024 * 1024)
    if record["sha256"] != SOURCE_MANIFEST_SHA256:
        raise ValueError("source identity manifest hash mismatch")
    manifest = _json(SOURCE_MANIFEST_PATH)
    session = manifest.get("session_id")
    if session != "20260909_144906_000490" or "urn:laserstream:session:" + session != config["raw_source_id"]:
        raise ValueError("source identity/session mismatch")
    matches = [item for item in manifest["raw_files"] if item.get("filename") == Path(config["raw_path"]).name]
    if len(matches) != 1 or matches[0] != {"filename": Path(config["raw_path"]).name,
            "bytes": 40872244, "sha256": config["raw_sha256"]}:
        raise ValueError("raw object not bound to source manifest")
    return {"path": SOURCE_MANIFEST_PATH, **record, "session_id": session,
            "source_id": config["raw_source_id"], "raw_file": matches[0],
            "scope": "LOCAL_ACQUISITION_MANIFEST_NOT_PROVIDER_OR_CHAIN_CERTIFICATION"}


def _inputs(config):
    raw = _file(config["raw_path"])
    if raw["sha256"] != config["raw_sha256"]:
        raise ValueError("raw input hash mismatch")
    path = Path(config["recovery_manifest_path"])
    manifest_file = _file(path, 1024 * 1024)
    if manifest_file["sha256"] != config["recovery_manifest_sha256"]:
        raise ValueError("recovery input hash mismatch")
    manifest = _json(path)
    if (manifest["schema_version"] != "narrative_sources_v1" or manifest["admitted"] is not False
            or manifest["split"] != "development" or manifest["counts"]["claims"] != 50
            or manifest["counts"]["exact_prefix_spans"] != 50):
        raise ValueError("recovery input scope mismatch")
    _verify_files(path.parent, manifest["files"])
    if "recovered_sources.jsonl" not in manifest["files"]:
        raise ValueError("missing recovered input")
    recovered = _jsonl(path.parent / "recovered_sources.jsonl", 50)
    constants = {"admitted": False, "availability_verified": False, "available_at_ms": None,
                 "source_state": "UNKNOWN", "rights_state": "UNKNOWN", "temporal_state": "UNKNOWN",
                 "split": "development"}
    if len(recovered) != 50 or any(any(row.get(k) != v for k, v in constants.items()) for row in recovered):
        raise ValueError("recovery input count/state mismatch")
    # Existing evidence is referenced unchanged, never converted into context.
    return {"raw": {"path": config["raw_path"], **raw},
            "recovery": {"manifest_path": str(path), **manifest_file, "files": manifest["files"],
                         "mode": "VERIFIED_IMMUTABLE_EXISTING_INPUT_NO_FRESH_TRANSFORM",
                         "count": len(recovered)}}


def _jsonl(path, ceiling):
    rows = []
    with stable_reader(path) as (source, info):
        if info.st_size > 128 * 1024 * 1024:
            raise ValueError("JSONL byte ceiling")
        for line in iter(lambda: source.readline(8 * 1024 * 1024 + 1), b""):
            if len(line) > 8 * 1024 * 1024 or len(rows) >= ceiling or not line.endswith(b"\n"):
                raise ValueError("JSONL row/line bound")
            rows.append(partitions._json(line))
    return rows


def _code_identity():
    names = ("development_pipeline", "raw_adapter", "partitions", "transaction_truth",
             "io", "fs_integrity", "splits", "reservation", "dependencies")
    return {name + ".py": _file(Path(__file__).parent / (name + ".py"))["sha256"] for name in names}


def _checkpoint(stage):
    """Fault-injection seam; never changes configuration or authority."""


def _publish_json(path, document):
    _publish_new(path, (json.dumps(document, sort_keys=True, indent=2, allow_nan=False) + "\n").encode())


def _verify_completed_partition(out, batch_rows):
    """Read-only composition of the pinned partition validators; never heal.

    Rebuild provenance from bounded validated JSONL, not from the receipt.
    The shared writer has a create-missing branch and must not be called here.
    Private validators are covered by our code identity and dependency tests.
    """
    if type(batch_rows) is not int or not 1 <= batch_rows <= 100:
        raise ValueError("invalid partition batch bound")
    events_path, output_path = out / "raw/events.jsonl", out / "events.parquet"
    checked_path(output_path)
    checked_path(out / "events.parquet.receipt.json")
    config = {"batch_rows": batch_rows, "max_rows": 100,
              "max_line_bytes": 8 * 1024 * 1024, "compression": "zstd"}
    report, events_sha, report_sha = partitions._report(
        events_path, config["max_rows"], config["max_line_bytes"])
    content = hashlib.sha256()
    for event in partitions._events(events_path, report, config):
        content.update(partitions._encoded(event) + b"\n")
    provenance = {
        "events_sha256": events_sha, "report_sha256": report_sha, "config": config,
        "config_sha256": hashlib.sha256(partitions._encoded(config)).hexdigest(),
        "source_path": report["source_path"], "source_sha256": report["source_sha256"],
        "content_sha256": content.hexdigest(),
        "schema_sha256": hashlib.sha256(partitions.ENVELOPE_SCHEMA.serialize().to_pybytes()).hexdigest(),
        "schema_version": "development_envelope_partition_v1",
        "pyarrow_version": version("pyarrow"),
        "writer_sha256": _file(Path(partitions.__file__))["sha256"]}
    receipt = partitions._verify(output_path, provenance, report["accepted"], batch_rows)
    partitions._unchanged(events_path, provenance)
    return receipt


def _summarize(out, identity):
    report = _json(out / "raw/report.json")
    tx = _json(out / "transactions/receipt.json")
    events = _jsonl(out / "raw/events.jsonl", 100)
    window = identity["gate"]["reserved_window"]
    for event in events:
        clocks = [event["observed_at_unix_ms"]]
        if event["block_time_unix_s"] is not None:
            clocks.append(event["block_time_unix_s"] * 1000)
        if any(window["start_utc_ms"] <= clock < window["end_utc_ms"] for clock in clocks):
            raise ValueError("raw clock conflicts with reserved window")
    accepted = _jsonl(out / "transactions/reconciled.ndjson", 10)
    rejected = _jsonl(out / "transactions/rejected.ndjson", 10)
    if (_file(out / "raw/events.jsonl")["sha256"] != report["events_sha256"]
            or report["source_sha256"] != identity["config"]["raw_sha256"]
            or report["source_sha256_after"] != report["source_sha256"]
            or report["rows_read"] != report["accepted"] + report["rejected"]
            or len(events) != report["accepted"] or report["rows_read"] > 100):
        raise ValueError("raw conservation/integrity mismatch")
    for name, digest in tx["output_sha256"].items():
        if _file(_relative(out / "transactions", name))["sha256"] != digest:
            raise ValueError("transaction output integrity mismatch")
    if (tx["source_sha256"] != identity["config"]["raw_sha256"]
            or len(accepted) != tx["reconciled_count"] or len(rejected) != tx["rejected_count"]
            or len(accepted) + len(rejected) != tx["transactions_attempted"]
            or tx["transactions_attempted"] > 10 or tx["records_read"] > report["rows_read"]):
        raise ValueError("transaction conservation mismatch")
    # The transaction helper must reread raw bytes: envelope Parquet deliberately
    # lacks balance payloads. This is not a Parquet -> transaction decoder.
    partition = _verify_completed_partition(out, identity["config"]["batch_rows"])
    if partition["rows"] != len(events):
        raise ValueError("partition conservation mismatch")
    files = {name: _file(out / name) for name in sorted(_OUTPUT_FILES)}
    return {"schema_version": "bounded_development_pipeline_v1", "state": "COMPLETE_UNADMITTED_DEVELOPMENT",
        "identity": identity, "identity_sha256": canonical_sha256(identity), "files": files,
        "states": {"exposure": "DEVELOPMENT_EXPOSED", "source_admission": "UNADMITTED",
            "rights": "UNKNOWN", "narrative_clock": "UNKNOWN", "mint_context_join": "UNKNOWN",
            "canonical_transaction_admissions": 0, "training_eligible": False, "fit_authorized": False,
            "export_authorized": False, "recovery_review": "NOT_RELIED_ON_INPUT_ONLY"},
        "joins": {"source_context": None, "mint": None, "available_at_ms": None,
                  "reason": "NO_VERIFIED_CLOCK_AND_MINT_PROOF"},
        "conservation": {
            "raw_projection": {"input": report["rows_read"], "accepted": len(events), "rejected": report["rejected"]},
            "partition": {"input": len(events), "output": partition["rows"]},
            "transaction_truth": {"raw_lines_scanned": tx["records_read"], "attempted": tx["transactions_attempted"],
                "reconciled": len(accepted), "rejected": len(rejected),
                "token_endpoint_rows": tx["token_movement_rows"], "native_endpoint_rows": tx["native_movement_rows"]},
            "recovery_input": {"input": 50, "referenced": 50, "transformed": 0}},
        "transaction_identity_quarantined": tx["canonical_identity_quarantined_count"],
        "units": {"native_movements": "lamports", "token_movements": "raw_integer_token_units_with_decimals",
                  "recorder_observation": "unix_ms_not_verified_availability", "source_spans": "UTF8_BYTE"},
        "limitations": ["completed-run verification-only resume; all interrupted runs require a new target",
            "trusted local tree only; no hostile concurrent mutation, ABA or power-loss custody guarantee",
            "raw and transaction helpers independently decode bounded prefixes; no whole-file decoded coverage",
            "hashes prove local byte equality, not provider identity, rights, original custody or source admission",
            "50 recovery input rows are not 50 context joins; no timestamps or mint proof invented",
            "128 MiB per-file ceiling, not a hardened decompressor/Arrow memory sandbox"]}


def run_development(output_dir, *, config=None, resume=False):
    """New-only bounded run or read-only verification of an EXACT completed run.

    Any interruption (including between manifest and completion marker) refuses
    continuation. New-only child helper APIs cannot honestly guarantee partial
    stage resume, so retain all partial files and require a fresh output target.
    """
    config = _config(default_config() if config is None else config)
    gate = exposure_gate(config["raw_source_id"], config["narrative_source_id"])
    out = Path(output_dir).absolute()
    for source in (Path(config["raw_path"]).absolute(), Path(config["recovery_manifest_path"]).absolute()):
        if out.is_relative_to(source.parent) or source.is_relative_to(out):
            raise ValueError("output overlaps source directory")
    checked_path(out / "identity.json", allow_missing=True, create_parents=False) if out.exists() else None
    source_binding = _source_binding(config)
    inputs = _inputs(config)
    identity = {"config": config, "config_sha256": canonical_sha256(config), "code": _code_identity(),
                "inputs": inputs, "source_binding": source_binding, "gate": gate, "output_dir": str(out),
                "runtime": {"python": platform.python_version(),
                            **{name: version(name) for name in ("pyarrow", "zstandard", "base58")}}}
    if out.exists():
        if not resume:
            raise FileExistsError(out)
        if _json(out / "identity.json") != identity:
            raise ValueError("resume identity mismatch: exact config/code/input required")
        if not (out / "COMPLETE.json").exists():
            raise ValueError("incomplete run: preserve artifacts and use a new target")
        complete = _json(out / "COMPLETE.json")
        if complete != {"manifest": _file(out / "manifest.json"), "identity_sha256": canonical_sha256(identity)}:
            raise ValueError("completion integrity mismatch")
        manifest = _json(out / "manifest.json")
        if type(manifest.get("files")) is not dict or set(manifest["files"]) != _OUTPUT_FILES:
            raise ValueError("invalid completed manifest file inventory")
        for record in manifest["files"].values():
            if (type(record) is not dict or set(record) != {"bytes", "sha256"}
                    or type(record["bytes"]) is not int or not 0 <= record["bytes"] <= 128 * 1024 * 1024
                    or type(record["sha256"]) is not str
                    or re.fullmatch(r"[0-9a-f]{64}", record["sha256"]) is None):
                raise ValueError("invalid completed manifest file metadata")
        _verify_files(out, manifest["files"])
        if manifest != _summarize(out, identity):
            raise ValueError("manifest integrity mismatch")
        return manifest
    if resume:
        raise ValueError("resume requires existing completed target")
    checked_path(out / "identity.json", allow_missing=True, create_parents=True)
    _publish_json(out / "identity.json", identity)
    raw_adapter.project_bounded_file(config["raw_path"], output_dir=out / "raw", limit=100,
                                     source_commitment=config["source_commitment"])
    _checkpoint("raw")
    partitions.write_partition(out / "raw/events.jsonl", out / "events.parquet",
        batch_rows=config["batch_rows"], max_rows=100, max_line_bytes=8 * 1024 * 1024, compression="zstd")
    _checkpoint("partition")
    transaction_truth.sample_bounded_file(config["raw_path"], out / "transactions",
        expected_sha256=config["raw_sha256"], max_transactions=10)
    _checkpoint("transactions")
    manifest = _summarize(out, identity)
    if (_inputs(config) != inputs or _code_identity() != identity["code"]
            or _source_binding(config) != source_binding):
        raise ValueError("inputs or code changed during run")
    if exposure_gate(config["raw_source_id"], config["narrative_source_id"]) != gate:
        raise ValueError("policy changed during run")
    _publish_json(out / "manifest.json", manifest)
    _checkpoint("manifest")
    # This marker alone commits the unified run, and is deliberately written last.
    completion = {"manifest": _file(out / "manifest.json"), "identity_sha256": canonical_sha256(identity)}
    _publish_json(out / "COMPLETE.json", completion)
    if _json(out / "COMPLETE.json") != completion:
        raise ValueError("completion readback mismatch")
    if _json(out / "manifest.json") != manifest:
        raise ValueError("manifest readback mismatch")
    return manifest
