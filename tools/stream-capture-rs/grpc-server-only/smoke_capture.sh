#!/bin/bash
# smoke_capture.sh — run a 1-minute training capture with creds read INSIDE WSL.
# The creds file is CRLF; strip \r. Values are never echoed (only key length).
set -e
CF=/mnt/c/Users/Alon/.hermes/creds/pump-quant.env
EP=$(grep -m1 '^LASERSTREAM_ENDPOINT=' "$CF" | cut -d= -f2- | tr -d '\r')
KEY=$(grep -m1 '^HELIUS_API_KEY=' "$CF" | cut -d= -f2- | tr -d '\r')
echo "endpoint=[$EP] key_len=${#KEY}"
export LASERSTREAM_ENDPOINT="$EP" HELIUS_API_KEY="$KEY"
cd /mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only
exec target/release/pq-laserstream-grpc --training-capture --duration 1