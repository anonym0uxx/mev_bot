#!/usr/bin/env python
"""capture_daemon.py — resident LaserStream capture loop (Windows-side supervisor).

Repeatedly:
  1. launch a bounded capture session in WSL (blocking; returns on session end/crash)
  2. compact any un-compacted events .ndjson.zst -> scalar Parquet
  3. purge raw .ndjson.zst older than the retention window (ring buffer)
  4. restart

Run as a background process (or under the mev_bot supervisor). Env-tunable:
  CAPTURE_SESSION_MINUTES      (default 300)
  CAPTURE_RAW_RETENTION_HOURS  (default 24)

Storage model (tiered): raw = bounded ring buffer (safety net only); events
scalar Parquet = the accumulating queryable product; both written under
tools/stream-capture-rs/grpc-server-only/training-data and output/events_parquet.
"""
import subprocess, sys, os, glob, time

REPO = "D:/repos/mev_bot"
TD = f"{REPO}/tools/stream-capture-rs/grpc-server-only/training-data"
OUT = f"{REPO}/tools/data-pipeline/output/events_parquet"
LAUNCHER = "/mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/run_capture.sh"
COMPACTOR = f"{REPO}/tools/data-pipeline/src/compact_events.py"

SESSION_MINUTES = int(os.environ.get("CAPTURE_SESSION_MINUTES", "300"))
RETENTION_HOURS = float(os.environ.get("CAPTURE_RAW_RETENTION_HOURS", "24"))


def launch_capture():
    """Run one bounded capture session in WSL (blocking)."""
    print(f"[daemon] launching {SESSION_MINUTES}min capture session", flush=True)
    return subprocess.run(
        ["wsl.exe", "-d", "Ubuntu", "--", "bash", LAUNCHER, str(SESSION_MINUTES)]
    )


def compact_new_sessions():
    """Compact every events .ndjson.zst without a matching .parquet."""
    os.makedirs(OUT, exist_ok=True)
    compacted = 0
    for ev in glob.glob(f"{TD}/pumpfun_laserstream_events_v1_*.ndjson.zst"):
        session = os.path.basename(ev).replace("pumpfun_laserstream_events_v1_", "").replace(".ndjson.zst", "")
        out_parquet = os.path.join(OUT, f"{session}.parquet")
        if os.path.exists(out_parquet):
            continue
        print(f"[daemon] compacting {session}", flush=True)
        r = subprocess.run([sys.executable, COMPACTOR, ev, out_parquet], capture_output=True, text=True)
        if r.returncode == 0:
            compacted += 1
        else:
            print(f"[daemon] compaction FAILED for {session}: {r.stderr.strip()[:400]}", flush=True)
    if compacted:
        print(f"[daemon] compacted {compacted} session(s)", flush=True)
    return compacted


def purge_old_raw():
    """Delete raw .ndjson.zst older than RETENTION_HOURS (ring buffer)."""
    now = time.time()
    cutoff = now - RETENTION_HOURS * 3600
    purged = 0
    for f in glob.glob(f"{TD}/pumpfun_laserstream_raw_v1_*.ndjson.zst"):
        try:
            if os.path.getmtime(f) < cutoff:
                os.remove(f)
                purged += 1
        except OSError as e:
            print(f"[daemon] purge failed {os.path.basename(f)}: {e}", flush=True)
    if purged:
        print(f"[daemon] purged {purged} raw part(s) older than {RETENTION_HOURS}h", flush=True)
    return purged


def main():
    print(f"[daemon] resident capture started (session={SESSION_MINUTES}min, "
          f"retention={RETENTION_HOURS}h)", flush=True)
    while True:
        launch_capture()
        compact_new_sessions()
        purge_old_raw()
        time.sleep(5)  # brief gap before restart


if __name__ == "__main__":
    main()
