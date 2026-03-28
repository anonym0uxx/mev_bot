#!/bin/bash
# Run parity test against historical data
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

export PARITY_DATA="${PARITY_DATA:-$PROJECT_DIR/data/mev_paper_trades.jsonl}"

echo "════════════════════════════════════════════════════════════"
echo "  Parity Test: Rust GateStack+Scorer vs TS Paper Trades"
echo "  Data: $PARITY_DATA"
echo "  Trades: $(wc -l < "$PARITY_DATA") lines"
echo "════════════════════════════════════════════════════════════"
echo ""

cd "$PROJECT_DIR/rust"
PATH=$HOME/.cargo/bin:$PATH \
  OPENSSL_DIR=/home/linuxbrew/.linuxbrew/opt/openssl@3 \
  PKG_CONFIG_PATH=/home/linuxbrew/.linuxbrew/opt/openssl@3/lib/pkgconfig \
  cargo run --bin parity-test --release 2>&1
