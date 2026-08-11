#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════
# launch_watchdog.sh — Production-grade launch script for pq-watchdog + daemon
# ═══════════════════════════════════════════════════════════════════════════
#
# Architecture (principal pump.fun memecoin quant desk):
#
#  ┌──────────────┐   spawns   ┌──────────────┐  ┌─────────────────────┐
#  │ launch_watch │──────────→│  pq-watchdog  │→│   pq-daemon (paper)  │
#  │  dog.sh      │           │  health-guard │  │  Engine::Paper mode  │
#  └──────────────┘           └──────────────┘  └─────────────────────┘
#                                                       │
#                                       ┌───────────────┼───────────────┐
#                                       ↓               ↓               ↓
#                                 ┌──────────┐   ┌──────────┐   ┌────────────┐
#                                 │LaserStream│  │Helius WS │   │PumpPortal  │
#                                 │ gRPC/WS  │   │(built-in)│   │  WS        │
#                                 │PRIMARY   │   │SECONDARY │   │SECONDARY   │
#                                 │(if avail)│   │slot+acct │   │create+trade│
#                                 └──────────┘   └──────────┘   └────────────┘
#                                       │
#                                       ↓
#                              ┌────────────────┐
#                              │pq-firecrawl-   │
#                              │bridge sidecar  │
#                              │(social intel)  │
#                              └────────────────┘
#
# Ingest lanes (quantitative correctness):
#
#  1. LaserStream (PRIMARY): gRPC binary (pq-laserstream-grpc.exe) provides
#     Geyser-fed transactionSubscribe with from_slot resume — lowest latency,
#     replay-capable. Falls back to pq-stream-capture.exe helius-ws if the
#     gRPC binary is unavailable (Windows host can't compile protobuf-src).
#     If NEITHER binary is available, the lane is cleanly disabled and the
#     daemon falls back to its built-in Helius WS + PumpPortal WS lanes.
#
#  2. Helius WS (SECONDARY, built-in): slotSubscribe (staleness heartbeat)
#     + per-mint accountSubscribe on bonding-curve PDAs. No transactionSubscribe
#     — the daemon uses this for slot timing and account-level diffs.
#
#  3. PumpPortal WS (SECONDARY, built-in): subscribeNewToken (create events)
#     + subscribeMigration (migration events) + per-mint subscribeTokenTrade.
#
#  4. Firecrawl bridge (SIDECAR): social intelligence from web scraping,
#     triggered by daemon events. Fail-safe: daemon runs without it.
#
# Daemon parameters (quantitatively tuned):
#
#  --junction-cap 8192        Ring buffer for event junction (2× default 4096).
#                             Higher cap = fewer overflow drops under burst.
#                             Quant: at ~400ms slot time and 50+ active mints,
#                             burst events can exceed 4096 in a single tick.
#
#  --commitment processed     Lowest latency commitment (~400ms slot time).
#                             Confirmed/finalized add 1-2s latency — too slow
#                             for memecoin trading where mcap moves in seconds.
#
#  --status-every-ticks 50    Write live_status.json every 50 ticks.
#                             At 250ms tick_period, that's ~12.5s between
#                             writes. The wall-clock heartbeat (15s) ensures
#                             a write even during event starvation.
#                             Watchdog health-timeout is 60s → 4.8× margin.
#                             Margin = timeout / status_interval = 60/12.5 ≈ 4.8×
#
#  --brain-snapshot-every-ticks 5000  Brain analysis snapshot frequency.
#                             At 250ms tick, ~21 minutes. Matches the refiner
#                             cadence (refiner_every_ticks=5000) so brain
#                             snapshots align with refiner promotions.
#
#  --tape-every-ticks 200     Tape flush frequency. At 250ms tick, ~50s.
#                             Balances I/O overhead vs tape freshness for
#                             the refiner's read-after-flush pattern.
#
# Watchdog parameters:
#
#  --max-restarts 10          Maximum daemon restarts before giving up.
#                             10 is sufficient for transient network issues
#                             but low enough to alert the operator if the
#                             daemon is fundamentally broken.
#
#  --health-timeout-secs 60   Kill daemon if live_status.json is older than
#                             60s. With status-every-ticks 50 + 15s wall-clock
#                             heartbeat, the daemon writes every ~12-15s →
#                             4× margin (60/15). Old value was 120s (too lax).
#
#  --backoff-cap-secs 30      Cap exponential backoff at 30s. Prevents
#                             long downtimes after consecutive crashes.
#
# Environment:
#
#  PQ_CREDS_FILE              Path to credentials file (User scope).
#                             Contains HELIUS_API_KEY, LASERSTREAM_ENDPOINT,
#                             HELIUS_WS_URL. Loaded by the daemon via
#                             dotenvy::from_path() — sets env vars in-process.
#                             The LaserStream subprocess inherits these via
#                             cmd.env() passthrough.
#
#  PQ_LASERSTREAM_BIN         Path to the LaserStream binary. The daemon
#                             searches: PQ_LASERSTREAM_BIN env → binary next
#                             to daemon exe → cwd. Set explicitly here.
#
#  PQ_LASERSTREAM_ARGS        Subcommand + flags for the LaserStream binary.
#                             For pq-laserstream-grpc.exe: unset (no args needed,
#                             reads LASERSTREAM_ENDPOINT env var).
#                             For pq-stream-capture.exe: "helius-ws --programs
#                             6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
#                             --commitment processed"
#                             (adds transactionSubscribe on pump.fun program).
#
#  PQ_FIRECRAWL_BIN           Path to the Firecrawl bridge binary.
#
# ═══════════════════════════════════════════════════════════════════════════

set -euo pipefail

# ─── Paths (Windows-native, NOT MSYS) ──────────────────────────────────────
# CRITICAL: PQ_LASERSTREAM_BIN must use Windows paths (D:/repos/...) because
# the daemon spawns the binary via std::process::Command::new() which uses
# Windows path resolution. MSYS paths (/d/repos/...) break the spawn.
REPO_ROOT="D:/repos/mev_bot"
RUST_DIR="$REPO_ROOT/rust"
DATA_DIR="$RUST_DIR/data"
WATCHDOG_BIN="$RUST_DIR/target/release/pq-watchdog.exe"

STREAM_CAPTURE_BIN="$REPO_ROOT/tools/stream-capture-rs/target/release/pq-stream-capture.exe"
# gRPC binary is compiled in WSL2 (Linux ELF). The daemon spawns it via wsl.exe
# with WSLENV forwarding credentials through the Windows→WSL2 boundary.
LASERSTREAM_GRPC_WSL="/mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/target/release/pq-laserstream-grpc"
# Native Windows fallback (if ever compiled natively on Windows):
LASERSTREAM_GRPC_BIN="$REPO_ROOT/tools/stream-capture-rs/grpc-server-only/target/release/pq-laserstream-grpc.exe"
LASERSTREAM_FALLBACK_BIN="$REPO_ROOT/tools/stream-capture-rs/target/release/pq-laserstream-grpc.exe"
FIRECRAWL_BIN="$REPO_ROOT/tools/firecrawl-bridge-rs/target/release/pq-firecrawl-bridge.exe"

# Pump.fun program ID for transactionSubscribe filter
PUMP_PROGRAM_ID="6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"

cd "$RUST_DIR"
mkdir -p "$DATA_DIR"

# ─── Pre-launch diagnostics ────────────────────────────────────────────────
echo "════════════════════════════════════════════════════════════════"
echo "  pq-watchdog launch — $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
echo "════════════════════════════════════════════════════════════════"
echo ""

# ─── 1. Credential verification ────────────────────────────────────────────
echo "── Credential check ──────────────────────────────────────────"
if [ -z "${PQ_CREDS_FILE:-}" ]; then
    echo "ERROR: PQ_CREDS_FILE not set. Expected at User scope."
    echo "Fix: hermes config set env.PQ_CREDS_FILE '\$HOME/.hermes/creds/pump-quant.env' --scope user"
    exit 1
fi
if [ ! -f "$PQ_CREDS_FILE" ]; then
    echo "ERROR: PQ_CREDS_FILE path does not exist: $PQ_CREDS_FILE"
    exit 1
fi
# Verify the creds file contains the required keys (names only, never values)
CREDS_OK=true
for key in HELIUS_API_KEY LASERSTREAM_ENDPOINT HELIUS_WS_URL; do
    if ! grep -q "^${key}=" "$PQ_CREDS_FILE" 2>/dev/null; then
        echo "  MISSING: $key"
        CREDS_OK=false
    fi
done
if [ "$CREDS_OK" = false ]; then
    echo "ERROR: PQ_CREDS_FILE is missing required keys."
    exit 1
fi
echo "  PQ_CREDS_FILE: OK (all required keys present)"
echo ""

# ─── 2. Binary verification ────────────────────────────────────────────────
echo "── Binary check ───────────────────────────────────────────────"

# Watchdog binary
if [ ! -f "$WATCHDOG_BIN" ]; then
    echo "ERROR: watchdog binary not found at $WATCHDOG_BIN"
    echo "Fix: cd $RUST_DIR && RUSTFLAGS='-C target-cpu=znver5' cargo build --release -p pump-quant-junction"
    exit 1
fi
echo "  pq-watchdog:     OK"

# Daemon binary (same target dir)
DAEMON_BIN="$RUST_DIR/target/release/pq-daemon.exe"
if [ ! -f "$DAEMON_BIN" ]; then
    echo "ERROR: daemon binary not found at $DAEMON_BIN"
    exit 1
fi
echo "  pq-daemon:       OK"

# Firecrawl bridge binary
if [ ! -f "$FIRECRAWL_BIN" ]; then
    echo "  pq-firecrawl-bridge: NOT FOUND (daemon will run without social intelligence)"
    export PQ_FIRECRAWL_BIN=""
else
    export PQ_FIRECRAWL_BIN="$FIRECRAWL_BIN"
    echo "  pq-firecrawl-bridge: OK"
fi

# LaserStream binary selection — try gRPC first, then stream-capture helius-ws
LS_BIN_SET=false
LS_BIN_PATH=""
LS_BIN_ARGS=""

# Check if the WSL2-compiled gRPC binary exists. We use `wsl.exe` to test
# because the binary lives inside the WSL2 filesystem (/mnt/d/...).
WSL_GRPC_EXISTS=$(wsl.exe -d Ubuntu -- bash -c 'test -x "/mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/target/release/pq-laserstream-grpc" && echo yes' 2>/dev/null || echo "no")

if [ "$WSL_GRPC_EXISTS" = "yes" ]; then
    # gRPC binary available in WSL2 — use as primary (lowest latency, replay-capable)
    # The daemon spawns wsl.exe, which runs the Linux binary inside WSL2.
    # Credentials are forwarded via WSLENV (set by the daemon's cmd.env()).
    LS_BIN_PATH="wsl.exe"
    LS_BIN_ARGS="-d Ubuntu -- $LASERSTREAM_GRPC_WSL"
    LS_BIN_SET=true
    echo "  pq-laserstream-grpc: OK (PRIMARY lane — gRPC via WSL2, replay-capable)"
elif [ -f "$LASERSTREAM_GRPC_BIN" ]; then
    # Native Windows gRPC binary (if ever compiled natively)
    LS_BIN_PATH="$LASERSTREAM_GRPC_BIN"
    LS_BIN_ARGS=""
    LS_BIN_SET=true
    echo "  pq-laserstream-grpc: OK (PRIMARY lane — native gRPC, replay-capable)"
elif [ -f "$STREAM_CAPTURE_BIN" ]; then
    # gRPC binary NOT available (can't compile on Windows). Use stream-capture
    # helius-ws as fallback. This adds transactionSubscribe on the pump.fun
    # program as a supplementary ingest lane. The daemon's parse_ndjson_line
    # will accept the helius-ws NDJSON format if the adapter is enabled.
    # NOTE: The helius-ws subcommand outputs a different NDJSON schema
    # ({"lane":"helius_ws","sub":"transaction","raw":{...}}) which the
    # daemon's LaserStream parser may not recognize. If the parser rejects
    # these lines, the daemon falls back to its built-in WS lanes — which
    # is the same behavior as not setting PQ_LASERSTREAM_BIN at all.
    #
    # For now: disable the LaserStream lane when only stream-capture is
    # available, because the NDJSON format mismatch means the data would
    # be silently dropped. The daemon's built-in Helius WS + PumpPortal WS
    # provide complete event coverage (create/trade/migration/slot/account).
    #
    # When pq-laserstream-grpc.exe is compiled (on a server or WSL2 with
    # Rust+protoc), set PQ_LASERSTREAM_BIN to its path and unset
    # PQ_LASERSTREAM_ARGS — the gRPC binary needs no subcommand.
    echo "  pq-laserstream-grpc: NOT FOUND (can't compile on Windows — protobuf-src)"
    echo "  pq-stream-capture:  available but NDJSON format incompatible with LaserStream parser"
    echo "  → LaserStream lane DISABLED — daemon uses Helius WS + PumpPortal WS"
    LS_BIN_SET=false
else
    echo "  pq-laserstream-grpc: NOT FOUND"
    echo "  pq-stream-capture:  NOT FOUND"
    echo "  → LaserStream lane DISABLED — daemon uses Helius WS + PumpPortal WS"
    LS_BIN_SET=false
fi

if [ "$LS_BIN_SET" = true ]; then
    export PQ_LASERSTREAM_BIN="$LS_BIN_PATH"
    if [ -n "$LS_BIN_ARGS" ]; then
        export PQ_LASERSTREAM_ARGS="$LS_BIN_ARGS"
    else
        unset PQ_LASERSTREAM_ARGS
    fi
else
    # Explicitly disable LaserStream lane — set PQ_LASERSTREAM_BIN to empty
    # so the daemon's std::env::var("PQ_LASERSTREAM_BIN") returns Ok("") which
    # is falsy in the Option chain (empty string → None after .filter).
    # Actually, std::env::var returns Ok("") for an empty env var, but the
    # daemon code checks .ok() which gives Some("") — this would cause
    # Command::new("") to fail. Instead, UNSET the variable entirely.
    unset PQ_LASERSTREAM_BIN
    unset PQ_LASERSTREAM_ARGS
fi

echo ""

# ─── 3. Process dedup check ────────────────────────────────────────────────
echo "── Process dedup check ────────────────────────────────────────"
check_dedup() {
    local wd_count dae_count sc_count fc_count
    wd_count=$(tasklist 2>/dev/null | grep -c "pq-watchdog" 2>/dev/null || echo 0)
    dae_count=$(tasklist 2>/dev/null | grep -c "pq-daemon" 2>/dev/null || echo 0)
    sc_count=$(tasklist 2>/dev/null | grep -c "pq-stream-capture" 2>/dev/null || echo 0)
    fc_count=$(tasklist 2>/dev/null | grep -c "pq-firecrawl-bridge" 2>/dev/null || echo 0)
    # Strip whitespace
    wd_count=$(echo "$wd_count" | tr -d '[:space:]')
    dae_count=$(echo "$dae_count" | tr -d '[:space:]')
    sc_count=$(echo "$sc_count" | tr -d '[:space:]')
    fc_count=$(echo "$fc_count" | tr -d '[:space:]')
    wd_count=${wd_count:-0}
    dae_count=${dae_count:-0}
    sc_count=${sc_count:-0}
    fc_count=${fc_count:-0}

    if [ "$wd_count" -gt 0 ] || [ "$dae_count" -gt 0 ] || [ "$sc_count" -gt 0 ] || [ "$fc_count" -gt 0 ]; then
        echo "DEDUP CHECK FAILED — existing pq processes found:"
        echo "  pq-watchdog:           $wd_count"
        echo "  pq-daemon:             $dae_count"
        echo "  pq-stream-capture:     $sc_count"
        echo "  pq-firecrawl-bridge:   $fc_count"
        echo ""
        echo "Refusing to launch. Kill existing processes first:"
        echo "  taskkill /F /IM pq-daemon.exe"
        echo "  taskkill /F /IM pq-watchdog.exe"
        echo "  taskkill /F /IM pq-stream-capture.exe"
        echo "  taskkill /F /IM pq-firecrawl-bridge.exe"
        echo "  rm -f $DATA_DIR/watchdog.pid"
        exit 1
    fi

    # Stale PID file check
    if [ -f "$DATA_DIR/watchdog.pid" ]; then
        local pid_in_file
        pid_in_file=$(cat "$DATA_DIR/watchdog.pid" 2>/dev/null)
        if tasklist /FI "PID eq $pid_in_file" /NH 2>/dev/null | grep -q "$pid_in_file"; then
            echo "DEDUP CHECK FAILED — watchdog PID file points to a live process (pid=$pid_in_file)"
            exit 1
        else
            echo "  Stale PID file (pid=$pid_in_file no longer alive) — cleaning up."
            rm -f "$DATA_DIR/watchdog.pid"
        fi
    fi

    echo "  No existing pq processes found — CLEAN"
}
check_dedup
echo ""

# ─── 4. Stale sentinel cleanup ──────────────────────────────────────────────
echo "── Sentinel cleanup ────────────────────────────────────────────"
for sentinel in EMERGENCY_STOP.sentinel DAEMON_STOP.sentinel STOP_WATCHDOG.sentinel \
                EMERGENCY_STOP DAEMON_STOP STOP_WATCHDOG WATCHDOG_GAVE_UP; do
    if [ -f "$DATA_DIR/$sentinel" ]; then
        echo "  Removing stale sentinel: $sentinel"
        rm -f "$DATA_DIR/$sentinel"
    fi
done
echo "  Sentinels cleaned"
echo ""

# ─── 5. Config verification ─────────────────────────────────────────────────
echo "── Config check ────────────────────────────────────────────────"
if [ -f "$DATA_DIR/CHAMPION_CONFIG.txt" ]; then
    CHAMP_LINES=$(wc -l < "$DATA_DIR/CHAMPION_CONFIG.txt" 2>/dev/null || echo 0)
    echo "  CHAMPION_CONFIG.txt: $CHAMP_LINES lines"
    # Verify critical params are present
    for param in entry_mode_leaves_enable brain_enable mcap_band_enable \
                 paper_tick_period_ms brain_min_sample; do
        if ! grep -q "^${param}=" "$DATA_DIR/CHAMPION_CONFIG.txt" 2>/dev/null; then
            echo "  WARNING: $param not in CHAMPION_CONFIG.txt (will use compiled default)"
        fi
    done
else
    echo "  CHAMPION_CONFIG.txt: NOT FOUND (daemon uses compiled defaults)"
    echo "  The refiner will create it after the first promotion cycle."
fi

# Verify CHAMPION_CONFIG params match the 3-phase anomaly fix
if grep -q "^entry_mode = " "$DATA_DIR/CHAMPION_CONFIG.txt" 2>/dev/null; then
    EM_VAL=$(grep "^entry_mode = " "$DATA_DIR/CHAMPION_CONFIG.txt" | head -1 | cut -d= -f2 | tr -d ' ')
    if [ "$EM_VAL" != "3400" ]; then
        echo "  WARNING: entry_mode = $EM_VAL (expected 3400 — Phase 1 cold-start prior)"
    fi
fi
echo ""

# ─── 6. Launch summary ──────────────────────────────────────────────────────
echo "── Launch configuration ───────────────────────────────────────"
echo "  Watchdog:    $WATCHDOG_BIN"
echo "  Daemon:      $DAEMON_BIN"
echo "  Data dir:    $DATA_DIR"
echo "  Creds:       $PQ_CREDS_FILE (PQ_CREDS_FILE env, User scope)"
if [ "$LS_BIN_SET" = true ]; then
    echo "  LaserStream: $PQ_LASERSTREAM_BIN $PQ_LASERSTREAM_ARGS"
    if [ -n "${PQ_LASERSTREAM_ARGS:-}" ]; then
        echo "    mode:      WSL2 gRPC (credentials forwarded via WSLENV)"
    fi
else
    echo "  LaserStream: DISABLED (gRPC binary not compiled — Helius WS + PumpPortal WS fallback)"
fi
if [ -n "${PQ_FIRECRAWL_BIN:-}" ] && [ "$PQ_FIRECRAWL_BIN" != "" ]; then
    echo "  Firecrawl:   $PQ_FIRECRAWL_BIN"
else
    echo "  Firecrawl:   DISABLED (binary not found)"
fi
echo ""
echo "  Watchdog params:"
echo "    --max-restarts 10"
echo "    --health-timeout-secs 60"
echo "    --backoff-cap-secs 30"
echo "  Daemon params (via --daemon-args):"
echo "    --junction-cap 8192"
echo "    --commitment processed"
echo "    --status-every-ticks 50"
echo "    --brain-snapshot-every-ticks 5000"
echo "    --tape-every-ticks 200"
echo "    --refiner-every-ticks 72000"
echo "    --strategy-label rev7-swing-v1"
echo ""
echo "  Health-check margin: 60s timeout / ~12.5s writes = 4.8×"
echo "  Wall-clock heartbeat: 15s (decoupled from tick throughput)"
echo ""

# ─── 7. Launch ──────────────────────────────────────────────────────────────
echo "── Launching watchdog ──────────────────────────────────────────"
echo ""

exec ./target/release/pq-watchdog \
    --max-restarts 10 \
    --health-timeout-secs 60 \
    --backoff-cap-secs 30 \
    --daemon-args "--junction-cap 8192 --commitment processed --status-every-ticks 50 --brain-snapshot-every-ticks 5000 --tape-every-ticks 200 --refiner-every-ticks 72000 --strategy-label rev7-swing-v1"
