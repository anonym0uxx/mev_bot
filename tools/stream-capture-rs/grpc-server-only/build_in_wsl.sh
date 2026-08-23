#!/bin/bash
# Build script for pq-laserstream-grpc in WSL2 (Linux native build).
# The protobuf-src crate requires a real make/C++ toolchain, which MSYS lacks.
# Run from Windows: wsl -d Ubuntu -- /path/to/this_script.sh
set -e
export PATH=/home/alon/.cargo/bin:/usr/bin:/bin:/usr/local/bin
unset $(export | sed 's/=.*//')
cd /mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only
cargo build --release 2>&1
