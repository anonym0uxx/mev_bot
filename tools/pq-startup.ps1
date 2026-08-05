# pq-startup.ps1 — Self-healing startup script for mev_bot on Windows
# Runs on every boot via Windows Task Scheduler "at logon" trigger
# Brings up: Docker/Firecrawl → wait → watchdog → daemon
# All services auto-restart on failure via their own restart policies
# Fully automated: NO manual steps needed after first reboot (WSL2 activation)

$ErrorActionPreference = "Stop"
$LOG_FILE = "$env:USERPROFILE\pq-startup.log"
$REPO = "D:\repos\mev_bot"
$FIRECRAWL_DIR = "D:\repos\firecrawl"

function Log([string]$msg) {
    $ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    "$ts | $msg" | Out-File -FilePath $LOG_FILE -Append -Encoding UTF8
}

Log "=== pq-startup BEGIN ==="

# ─── 1. Wait for Docker Desktop engine to be ready ─────────────────────────
# Docker Desktop auto-starts on logon (registry HKCU Run key).
# First boot after WSL2 activation takes longer (WSL kernel install).
# We wait up to 600s (10 min) for the first boot, 300s thereafter.
$dockerTries = 0
$dockerMaxTries = 120  # 120 × 5s = 600s
while ($dockerTries -lt $dockerMaxTries) {
    $dockerStatus = $null
    try { $dockerStatus = docker info 2>&1 | Select-String "Containers" } catch {}
    if ($dockerStatus) {
        Log "Docker is ready (after $($dockerTries * 5)s)"
        break
    }
    Log "Waiting for Docker... (attempt $($dockerTries + 1)/$dockerMaxTries)"
    Start-Sleep -Seconds 5
    $dockerTries++
    if ($dockerTries -eq 20) {
        # Try to start Docker Desktop app explicitly (belt-and-suspenders)
        $ddPath = "${env:ProgramFiles}\Docker\Docker\Docker Desktop.exe"
        if (Test-Path $ddPath) {
            Log "Starting Docker Desktop app explicitly"
            Start-Process $ddPath
        }
    }
}

if ($dockerTries -ge $dockerMaxTries) {
    Log "ERROR: Docker not ready after 600s — continuing without Firecrawl"
} else {
    # ─── 2. Start Firecrawl stack ────────────────────────────────────────
    # On first boot, images need to be pulled (Redis, RabbitMQ, Postgres,
    # Playwright, Firecrawl API/Worker). This can take 5-10 min.
    # docker compose up -d handles both pull and start in one command.
    Log "Starting Firecrawl stack (docker compose up -d)"
    Push-Location $FIRECRAWL_DIR
    
    # Check if images are already present to log appropriately
    $imagesCheck = docker images --format "{{.Repository}}:{{.Tag}}" 2>$null | Select-String "firecrawl|redis|rabbitmq|postgres|playwright"
    if ($imagesCheck) {
        Log "Firecrawl images already present — starting existing containers"
    } else {
        Log "First boot detected — pulling Firecrawl images (may take 5-10 min)"
    }
    
    # docker compose up -d pulls missing images then starts all services
    $composeOutput = docker compose up -d 2>&1
    $composeOutput | Out-File -FilePath $LOG_FILE -Append -Encoding UTF8
    Log "docker compose up -d completed (exit: $LASTEXITCODE)"
    Pop-Location

    # ─── 3. Wait for Firecrawl API to be healthy ────────────────────────
    # After first image pull, containers need time to initialize.
    # We wait up to 600s (10 min) for first boot, shorter for subsequent.
    $fcTries = 0
    $fcMaxTries = 120  # 120 × 5s = 600s
    while ($fcTries -lt $fcMaxTries) {
        try {
            $health = Invoke-RestMethod -Uri "http://127.0.0.1:3002/health" -TimeoutSec 5 -ErrorAction Stop
            Log "Firecrawl is healthy (after $($fcTries * 5)s)"
            break
        } catch {
            if ($fcTries % 6 -eq 0) {
                Log "Waiting for Firecrawl... (attempt $($fcTries + 1)/$fcMaxTries)"
            }
            Start-Sleep -Seconds 5
            $fcTries++
        }
    }
    if ($fcTries -ge $fcMaxTries) {
        Log "WARNING: Firecrawl not healthy after 600s — daemon will run without social intelligence"
    }
}

# ─── 4. Start the watchdog (which launches the daemon + Firecrawl bridge) ──
Log "Starting watchdog"
$launchScript = "$REPO\rust\launch_watchdog.sh"
$gitBash = "${env:ProgramFiles}\Git\bin\bash.exe"
if (Test-Path $gitBash) {
    # Set environment variables and launch watchdog via git-bash
    $env:PQ_CREDS_FILE = "$env:USERPROFILE\.hermes\creds\pump-quant.env"
    $env:PQ_LASERSTREAM_BIN = "$REPO\tools\stream-capture-rs\target\release\pq-stream-capture.exe"
    $env:PQ_FIRECRAWL_BIN = "$REPO\tools\firecrawl-bridge-rs\target\release\pq-firecrawl-bridge.exe"
    $env:RUSTFLAGS = "-C target-cpu=znver5"
    
    $daemonArgs = "--junction-cap 8192 --commitment processed --status-every-ticks 50 --brain-snapshot-every-ticks 5000 --tape-every-ticks 1000"
    $watchdogArgs = "--max-restarts 5 --health-timeout-secs 60 --backoff-cap-secs 30"
    
    Start-Process -FilePath $gitBash -ArgumentList "-c", "cd '$REPO/rust' && bash launch_watchdog.sh --daemon-args '$daemonArgs' -- $watchdogArgs" -WindowStyle Minimized
    Log "Watchdog launched"
} else {
    Log "ERROR: Git bash not found at expected path"
}

# ─── 5. Hermes cron jobs ────────────────────────────────────────────────────
Log "Hermes cron jobs are managed by Hermes scheduler — no action needed"

Log "=== pq-startup END ==="
