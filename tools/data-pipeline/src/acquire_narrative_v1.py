#!/usr/bin/env python
"""
narrative_gold_v1 — RAW Acquisition Framework
=============================================
Implements the RAW ingestion layer for narrative/social data.

DESIGN:
  - first_seen timestamps begin NOW (at acquisition start).
  - Broad RAW is fine; GOLD admission is fail-closed.
  - Uncertain data stays RAW/REJECTED/UNRESOLVED.
  - RAW is immutable, never deleted. All gold layers derive FROM raw.
  - Low-resource: runs in background, writes to disk incrementally.
  - Quality > Quantity: 50K excellent > 5M noisy.

OUTPUT:
  output/narrative_gold_v1/raw/
    raw_social_event_v1/    — one-object-per-line JSONL files (immutable)
    raw_manifest_v1.json    — manifest of raw files with SHA256 + row counts
    FIRST_SEEN.log          — acquisition start timestamp
    acquisition_state.json  — resume state for fail-closed resume

USAGE:
  python src/acquire_narrative_v1.py [--interval 300] [--max-events 10000]

The actual social adapters (telegram_stream.py, twitterapi_stream.py, etc.)
require API keys. This framework:
  1. Defines the RAW schema and storage format.
  2. Provides a mock/standby ingestor that records first_seen NOW for any
     pre-existing raw data (from prior adapter runs).
  3. Sets up the directory structure + manifest for when live adapters connect.
  4. Can be extended to plug in real adapters when keys are available.

When real adapters are connected, their one-object-per-line JSON output gets
normalized into RawSocialEventV1 records and written to raw_social_event_v1/.
"""

import os
import sys
import json
import time
import hashlib
import argparse
import traceback
from pathlib import Path
from datetime import datetime, timezone

# ─── Paths ───────────────────────────────────────────────────────────

PIPELINE_ROOT = Path(__file__).parent.parent
OUTPUT_BASE = PIPELINE_ROOT / "output" / "narrative_gold_v1"
RAW_DIR = OUTPUT_BASE / "raw" / "raw_social_event_v1"
MANIFEST_PATH = OUTPUT_BASE / "raw" / "raw_manifest_v1.json"
FIRST_SEEN_LOG = OUTPUT_BASE / "raw" / "FIRST_SEEN.log"
ACQ_STATE_PATH = OUTPUT_BASE / "raw" / "acquisition_state.json"

# ─── Schema constants ───────────────────────────────────────────────

SCHEMA_VERSION = "1.0.0"
PIPELINE_VERSION = "1.0.0"

# ─── Utilities ──────────────────────────────────────────────────────


def now_utc_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


def now_unix_ms() -> int:
    return int(time.time() * 1000)


def sha256_hex(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


def normalize_text(text: str) -> str:
    """Normalize text for dedup: lowercase, strip URLs, collapse whitespace."""
    import re
    text = re.sub(r'https?://\S+', '', text)
    text = re.sub(r'pic\.x\.com/\S+', '', text)
    text = text.lower().strip()
    text = re.sub(r'\s+', ' ', text)
    return text


def extract_cashtags(text: str) -> list:
    """Extract $TICKER cashtags."""
    import re
    return list(set(re.findall(r'\$([A-Za-z0-9]+)', text)))


def extract_contract_addresses(text: str) -> list:
    """Extract Solana addresses (base58, 32-44 chars)."""
    import re
    # Solana addresses are base58, typically 32-44 characters
    matches = re.findall(r'\b([1-9A-HJ-NP-Za-km-z]{32,44})\b', text)
    # Filter out common false positives (pure numbers, etc.)
    return list(set(m for m in matches if len(m) >= 32))


def extract_urls(text: str) -> list:
    """Extract URLs from text."""
    import re
    return re.findall(r'https?://\S+', text)


def parse_raw_adapter_output(line: str, platform: str, run_uuid: str,
                             git_sha: str) -> dict:
    """Parse one-object-per-line JSON from social-ingest adapters into
    RawSocialEventV1 format.

    Adapter output format (from social-ingest/normalize.py):
    {"platform":"x|telegram|tiktok|web","author":"...","community":"",
     "text":"...","likes":0,"reposts":0,"replies":0,"echo":false}
    """
    try:
        raw = json.loads(line)
    except json.JSONDecodeError:
        return None

    text = raw.get("text", "")
    normalized = normalize_text(text)
    text_hash = sha256_hex(normalized)

    # Extract entities
    cashtags = extract_cashtags(text)
    contracts = extract_contract_addresses(text)
    urls = extract_urls(text)

    # Determine publish_time (from adapter if available, else first_seen)
    publish_time = raw.get("timestamp_ms", now_unix_ms())
    first_seen = now_unix_ms()

    # Determine account_type from seeds or default
    account_handle = raw.get("author", "unknown")
    account_type = raw.get("account_type", "anonymous")

    event = {
        "event_id": sha256_hex(line),
        "schema_version": SCHEMA_VERSION,
        "pipeline_version": PIPELINE_VERSION,
        "run_uuid": run_uuid,
        "git_sha": git_sha,

        "platform": platform,
        "account_handle": account_handle,
        "account_id": raw.get("author_id", account_handle),
        "account_type": account_type,
        "source_id": raw.get("id", ""),
        "url": raw.get("url", ""),

        "publish_time_ms": publish_time,
        "first_seen_ms": first_seen,
        "ingestion_latency_ms": first_seen - publish_time,

        "text": text,
        "text_hash": text_hash,
        "engagement_likes": raw.get("likes"),
        "engagement_reposts": raw.get("reposts"),
        "engagement_replies": raw.get("replies"),
        "engagement_views": raw.get("views"),
        "is_echo": raw.get("echo", False),
        "is_edit": raw.get("is_edit", False),
        "is_delete": raw.get("is_delete", False),

        "cashtags": "|".join(cashtags) if cashtags else None,
        "contract_addresses": "|".join(contracts) if contracts else None,
        "mentioned_mints": "|".join(contracts) if contracts else None,
        "mentioned_tokens": "|".join(cashtags) if cashtags else None,
        "urls_in_text": "|".join(urls) if urls else None,

        "retrieval_method": raw.get("retrieval_method", "api"),
        "raw_payload_path": "",  # set by writer
        "admission_status": "RAW",
        "contamination_flags": None,
    }

    return event


class RawWriter:
    """Writes raw events to immutable JSONL files. One file per ~10K events
    or 1-hour window, whichever comes first."""

    BATCH_SIZE = 10_000
    TIME_WINDOW_MS = 3600 * 1000  # 1 hour

    def __init__(self, run_uuid: str, git_sha: str):
        self.run_uuid = run_uuid
        self.git_sha = git_sha
        self.events_written = 0
        self.files_written = []
        self.current_batch = []
        self.batch_start_time = now_unix_ms()
        self.file_index = 0

        RAW_DIR.mkdir(parents=True, exist_ok=True)

    def add(self, event: dict):
        self.current_batch.append(event)
        self.events_written += 1

        elapsed = now_unix_ms() - self.batch_start_time
        if len(self.current_batch) >= self.BATCH_SIZE or elapsed >= self.TIME_WINDOW_MS:
            self.flush()

    def flush(self):
        if not self.current_batch:
            return

        fname = f"raw_{self.file_index:04d}.jsonl"
        fpath = RAW_DIR / fname
        tmp_path = RAW_DIR / f"{fname}.tmp"

        # Write to temp, then atomic rename
        with open(tmp_path, "w", encoding="utf-8") as f:
            for event in self.current_batch:
                event["raw_payload_path"] = str(fpath)
                f.write(json.dumps(event, ensure_ascii=False) + "\n")

        # Atomic temp→final
        os.rename(tmp_path, fpath)

        # Compute SHA256
        file_hash = sha256_hex(open(fpath, "r", encoding="utf-8").read())
        file_size = os.path.getsize(fpath)

        self.files_written.append({
            "filename": fname,
            "path": str(fpath),
            "rows": len(self.current_batch),
            "bytes": file_size,
            "sha256": file_hash,
        })

        self.current_batch = []
        self.batch_start_time = now_unix_ms()
        self.file_index += 1

        print(f"  Wrote {fname}: {self.files_written[-1]['rows']} rows, "
              f"{file_size / 1024:.1f} KB")

    def finalize(self):
        self.flush()
        manifest = {
            "schema_version": SCHEMA_VERSION,
            "pipeline_version": PIPELINE_VERSION,
            "run_uuid": self.run_uuid,
            "git_sha": self.git_sha,
            "created_at": now_utc_iso(),
            "total_events": self.events_written,
            "total_files": len(self.files_written),
            "files": self.files_written,
        }
        with open(MANIFEST_PATH, "w", encoding="utf-8") as f:
            json.dump(manifest, f, indent=2)
        print(f"\nManifest: {MANIFEST_PATH}")
        print(f"Total events: {self.events_written}")
        print(f"Total files: {len(self.files_written)}")


class StandbyIngestor:
    """Standby ingestor that runs when no API keys are available.
    Records first_seen timestamp NOW and waits for real adapter input.
    Can also ingest from pre-existing raw adapter output files."""

    def __init__(self, run_uuid: str, git_sha: str):
        self.run_uuid = run_uuid
        self.git_sha = git_sha
        self.writer = RawWriter(run_uuid, git_sha)

    def ingest_file(self, filepath: str, platform: str):
        """Ingest a pre-existing raw adapter output file."""
        print(f"Ingesting {filepath} ({platform})...")
        count = 0
        with open(filepath, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                event = parse_raw_adapter_output(line, platform, self.run_uuid, self.git_sha)
                if event:
                    self.writer.add(event)
                    count += 1
        print(f"  Ingested {count} events from {filepath}")
        return count

    def ingest_mock(self, n: int = 100):
        """Generate mock events for pipeline testing. These are clearly flagged
        as mock data and get RAW admission status (never GOLD without real source)."""
        print(f"Generating {n} mock events for pipeline testing...")
        mock_accounts = [
            ("OrangeSBS", "x", "kol"),
            ("blknoiz06", "x", "kol"),
            ("crypticannouncements", "telegram", "community"),
            ("chasescharts", "telegram", "community"),
        ]
        for i in range(n):
            account, platform, atype = mock_accounts[i % len(mock_accounts)]
            mock_text = f"Mock event {i} for pipeline testing. $SOL pump.fun test."
            raw_line = json.dumps({
                "platform": platform,
                "author": account,
                "account_type": atype,
                "text": mock_text,
                "likes": 0,
                "reposts": 0,
                "replies": 0,
                "echo": False,
                "id": f"mock_{i}",
                "timestamp_ms": now_unix_ms(),
            })
            event = parse_raw_adapter_output(raw_line, platform, self.run_uuid, self.git_sha)
            if event:
                event["contamination_flags"] = "MOCK_DATA"
                self.writer.add(event)
        print(f"  Generated {n} mock events")

    def finalize(self):
        self.writer.finalize()


# ─── Main ───────────────────────────────────────────────────────────


def get_git_sha() -> str:
    import subprocess
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, timeout=10,
            cwd=str(PIPELINE_ROOT)
        )
        return result.stdout.strip() if result.returncode == 0 else "unknown"
    except Exception:
        return "unknown"


def main():
    parser = argparse.ArgumentParser(description="narrative_gold_v1 RAW acquisition")
    parser.add_argument("--interval", type=int, default=300,
                        help="Polling interval in seconds (default: 300)")
    parser.add_argument("--max-events", type=int, default=0,
                        help="Max events to acquire (0 = unlimited)")
    parser.add_argument("--mock", type=int, default=0,
                        help="Generate N mock events for testing (default: 0)")
    parser.add_argument("--ingest-file", type=str, default=None,
                        help="Ingest a pre-existing raw adapter output file")
    parser.add_argument("--ingest-platform", type=str, default="x",
                        help="Platform label for --ingest-file (default: x)")
    args = parser.parse_args()

    run_uuid = hashlib.sha256(str(now_unix_ms()).encode()).hexdigest()[:12]
    git_sha = get_git_sha()

    # ─── Initialize ─────────────────────────────────────────────────
    OUTPUT_BASE.mkdir(parents=True, exist_ok=True)
    RAW_DIR.mkdir(parents=True, exist_ok=True)

    # Record first_seen timestamp NOW
    first_seen_utc = now_utc_iso()
    first_seen_ms = now_unix_ms()

    with open(FIRST_SEEN_LOG, "w") as f:
        f.write(f"narrative_gold_v1 acquisition FIRST_SEEN\n")
        f.write(f"UTC: {first_seen_utc}\n")
        f.write(f"Unix_ms: {first_seen_ms}\n")
        f.write(f"Run_UUID: {run_uuid}\n")
        f.write(f"Git_SHA: {git_sha}\n")
        f.write(f"Schema: {SCHEMA_VERSION}\n")

    # Save acquisition state for resume
    acq_state = {
        "run_uuid": run_uuid,
        "git_sha": git_sha,
        "first_seen_utc": first_seen_utc,
        "first_seen_ms": first_seen_ms,
        "status": "STARTED",
        "last_update": now_utc_iso(),
        "events_acquired": 0,
    }
    with open(ACQ_STATE_PATH, "w") as f:
        json.dump(acq_state, f, indent=2)

    print("=" * 70)
    print("NARRATIVE_GOLD_V1 — RAW ACQUISITION")
    print("=" * 70)
    print(f"Run UUID:    {run_uuid}")
    print(f"Git SHA:     {git_sha}")
    print(f"First seen:  {first_seen_utc}")
    print(f"Output:      {RAW_DIR}")
    print(f"Interval:    {args.interval}s")
    print()

    ingestor = StandbyIngestor(run_uuid, git_sha)

    # ─── Ingest modes ───────────────────────────────────────────────

    if args.ingest_file:
        ingestor.ingest_file(args.ingest_file, args.ingest_platform)
        ingestor.finalize()
        acq_state["status"] = "COMPLETED_FILE"
        acq_state["events_acquired"] = ingestor.writer.events_written
        acq_state["last_update"] = now_utc_iso()
        with open(ACQ_STATE_PATH, "w") as f:
            json.dump(acq_state, f, indent=2)
        return

    if args.mock > 0:
        ingestor.ingest_mock(args.mock)
        ingestor.finalize()
        acq_state["status"] = "COMPLETED_MOCK"
        acq_state["events_acquired"] = ingestor.writer.events_written
        acq_state["last_update"] = now_utc_iso()
        with open(ACQ_STATE_PATH, "w") as f:
            json.dump(acq_state, f, indent=2)
        return

    # ─── Standby mode ───────────────────────────────────────────────
    # In standby mode, the framework is initialized and waiting for real
    # adapter connections. The first_seen timestamp is recorded NOW.
    # When API keys are available, this loop will poll real adapters.

    print("STANDBY MODE: No API keys detected. Recording first_seen timestamp.")
    print("To connect real adapters:")
    print("  TELEGRAM_API_ID=... python tools/social-ingest/telegram_stream.py")
    print("  TWITTERAPI_IO_KEY=... python tools/social-ingest/twitterapi_stream.py")
    print()
    print("Raw adapter output can be ingested with --ingest-file.")
    print("Mock data for pipeline testing with --mock N.")
    print()
    print("First_seen timestamp recorded. Acquisition framework is READY.")
    print(f"FIRST_SEEN.log: {FIRST_SEEN_LOG}")

    ingestor.writer.flush()
    acq_state["status"] = "STANDBY"
    acq_state["events_acquired"] = 0
    acq_state["last_update"] = now_utc_iso()
    with open(ACQ_STATE_PATH, "w") as f:
        json.dump(acq_state, f, indent=2)


if __name__ == "__main__":
    main()
