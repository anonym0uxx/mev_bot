#!/bin/bash
# Run the Rust MEV daemon alongside the TypeScript daemon (paper mode)
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
RUST_DIR="$PROJECT_DIR/rust"

export PATH="$HOME/.cargo/bin:$PATH"
export OPENSSL_DIR="/home/linuxbrew/.linuxbrew/opt/openssl@3"
export PKG_CONFIG_PATH="/home/linuxbrew/.linuxbrew/opt/openssl@3/lib/pkgconfig"
export PAPER_MODE="${PAPER_MODE:-true}"
export CANARY_CONFIG="$PROJECT_DIR/config/canary.json"
export RUST_LOG="${RUST_LOG:-info}"

# Build in release mode
echo "Building Rust daemon (release)..."
cd "$RUST_DIR"
cargo build --release 2>&1

# Run
echo "Starting Rust MEV daemon (paper_mode=$PAPER_MODE)..."
exec "$RUST_DIR/target/release/pump-quant"
