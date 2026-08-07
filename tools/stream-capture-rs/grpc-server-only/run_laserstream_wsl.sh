#!/usr/bin/env bash
# run_laserstream_wsl.sh — Windows-side wrapper that launches the
# pq-laserstream-grpc Linux binary inside WSL2, forwarding credentials.
#
# The daemon spawns this script as PQ_LASERSTREAM_BIN. The script bridges
# the Windows→WSL2 boundary by passing env vars through the command line
# (WSL2 does not inherit Windows process env vars).
#
# Required env vars (set by the daemon):
#   HELIUS_API_KEY        — Helius API key for gRPC auth
#   LASERSTREAM_ENDPOINT  — Helius gRPC endpoint URL
#   HELIUS_WS_URL         — (optional) Helius WS URL for fallback
#
# The script passes these to WSL2 via `wsl --exec`, which forwards
# env vars when set with `WSLENV` (Windows env var passthrough).
#
# Usage by daemon:
#   PQ_LASERSTREAM_BIN = path/to/run_laserstream_wsl.sh
#   (no PQ_LASERSTREAM_ARGS needed)
#
# The daemon sets env vars on the child process via cmd.env();
# we use WSLENV to forward them through the WSL2 boundary.

set -euo pipefail

# ─── Locate the gRPC binary inside WSL2 ─────────────────────────────
GRPC_BIN="/mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/target/release/pq-laserstream-grpc"

if [ ! -x "$GRPC_BIN" ]; then
    echo "[run_laserstream_wsl] ERROR: gRPC binary not found at $GRPC_BIN" >&2
    exit 4
fi

# ─── Forward credentials via WSLENV ────────────────────────────────
# WSLENV is a Windows env var that tells wsl.exe which vars to forward.
# Format: VARNAME (forward as-is) or VARNAME/u (forward and convert path)
# The daemon sets these on the child process; we echo them into WSLENV.
export WSLENV="HELIUS_API_KEY/u:LASERSTREAM_ENDPOINT/u:HELIUS_WS_URL/u"

# ─── Launch inside WSL2 ────────────────────────────────────────────
# Use `wsl -d Ubuntu --exec` so the binary runs with forwarded env vars.
exec wsl.exe -d Ubuntu -- "$GRPC_BIN"
