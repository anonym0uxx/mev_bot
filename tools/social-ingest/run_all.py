#!/usr/bin/env python3
"""Unified runner: launch several `[S]` adapters and multiplex their normalized
NDJSON onto one stdout — the single fused attention stream the Rust core consumes.

    # everything, live, into the deterministic probe:
    python3 run_all.py --adapters telegram,x-firehose,x-amplifier,tiktok,web \
        | cargo run --quiet --manifest-path probe/Cargo.toml

    # prove the whole fan-in with zero keys:
    python3 run_all.py --selftest | cargo run --quiet --manifest-path probe/Cargo.toml

Each adapter is its own process (one vendor's clock/network stays isolated); the
runner only merges their line-oriented output, preserving the vendor-agnostic
schema. A crashing adapter is logged and dropped, never taking the stream down.
"""
import argparse
import subprocess
import sys
import threading

# adapter key -> (script, live args, selftest args)
ADAPTERS = {
    "telegram": ("telegram_stream.py", [], ["--selftest"]),
    "x-firehose": ("twitterapi_stream.py", ["--class", "firehose", "--watch", "5"], ["--selftest"]),
    "x-amplifier": ("twitterapi_stream.py", ["--class", "amplifier", "--watch", "10"], ["--selftest"]),
    "x-list": ("twitterapi_stream.py", ["--class", "list", "--watch", "10"], ["--selftest"]),
    "tiktok": ("tiktok_stream.py", ["--watch", "60"], ["--selftest"]),
    "web": ("firecrawl_stream.py", ["--watch", "120"], ["--selftest"]),
}

_write_lock = threading.Lock()


def pump(name: str, proc: subprocess.Popen):
    """Forward one adapter's stdout lines to the shared stdout, line-atomic."""
    assert proc.stdout is not None
    for line in proc.stdout:
        with _write_lock:
            sys.stdout.write(line)
            sys.stdout.flush()
    rc = proc.wait()
    sys.stderr.write(f"[run_all] adapter {name} exited ({rc})\n")


def main() -> int:
    ap = argparse.ArgumentParser(description="multiplex social adapters into one NDJSON stream")
    ap.add_argument(
        "--adapters",
        default="telegram,x-firehose",
        help="comma list: " + ",".join(ADAPTERS),
    )
    ap.add_argument("--selftest", action="store_true", help="run every adapter's selftest (no keys)")
    args = ap.parse_args()

    keys = list(ADAPTERS) if args.selftest else [a.strip() for a in args.adapters.split(",") if a.strip()]
    unknown = [k for k in keys if k not in ADAPTERS]
    if unknown:
        sys.stderr.write(f"unknown adapters: {unknown}; valid: {list(ADAPTERS)}\n")
        return 2

    procs, threads = [], []
    for k in keys:
        script, live_args, self_args = ADAPTERS[k]
        argv = [sys.executable, script] + (self_args if args.selftest else live_args)
        try:
            p = subprocess.Popen(argv, stdout=subprocess.PIPE, text=True, bufsize=1)
        except OSError as e:
            sys.stderr.write(f"[run_all] failed to start {k}: {e}\n")
            continue
        procs.append(p)
        t = threading.Thread(target=pump, args=(k, p), daemon=True)
        t.start()
        threads.append(t)
        sys.stderr.write(f"[run_all] started {k}: {' '.join(argv[1:])}\n")

    try:
        for t in threads:
            t.join()
    except KeyboardInterrupt:
        sys.stderr.write("[run_all] stopping\n")
        for p in procs:
            p.terminate()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
