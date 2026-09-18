#!/usr/bin/env bash
# Read-only by default. No setup, mounts, cleanup, or implicit run.
set -euo pipefail
HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PYTHON="${PYTHON:-python3}"
exec "$PYTHON" -B "$HERE/launch_native.py" --dry-run "$@"
