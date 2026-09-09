#!/bin/bash
# run_capture.sh — launch pq-laserstream-grpc in WSL with creds read from file.
# Usage: run_capture.sh [duration_minutes]   (default 300)
#
# Creds are read INSIDE WSL (CRLF-safe). Values are never echoed. This replaces
# run_capture_300.sh, which hardcoded the API key (now a committed-secret leak).
set -e
CF=/mnt/c/Users/Alon/.hermes/creds/pump-quant.env
EP=$(grep -m1 '^LASERSTREAM_ENDPOINT=' "$CF" | cut -d= -f2- | tr -d '\r')
KEY=$(grep -m1 '^HELIUS_API_KEY=' "$CF" | cut -d= -f2- | tr -d '\r')
WAL=$(grep -m1 '^WALLET_ADDRESS=' "$CF" | cut -d= -f2- | tr -d '\r')
DUR="${1:-300}"
if [ -z "$KEY" ]; then echo "run_capture: HELIUS_API_KEY empty (creds read failed)" >&2; exit 2; fi
export LASERSTREAM_ENDPOINT="$EP" HELIUS_API_KEY="$KEY" WALLET_ADDRESS="$WAL"
export TRAINING_CAPTURE_DIR=/mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/training-data
cd /mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only
mkdir -p training-data
echo "run_capture: duration=${DUR}min key_len=${#KEY}"
exec target/release/pq-laserstream-grpc --training-capture --duration "$DUR"