# pq-startup.ps1 — Self-healing startup script for mev_bot on Windows
# Runs on every boot via Windows Task Scheduler "at logon" trigger
# Brings up: Docker/Firecrawl → wait → watchdog → daemon
# All services auto-restart on failure via their own restart policies

$ErrorActionPreference = "Stop"
$LOG_FILE = "$env:USERPROFILE\pq-startup.log"
$REPO = "D:\repos\mev_bot"
$FIRECRAWL_DIR = "D:\repos\firecrawl"

function Log([string]$msg) {
    $ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    "$ts | $msg" | Out-File -FilePath $LOG_FILE -Append -Encoding UTF8
}

Log "=== pq-startup BEGIN ==="

# 1. Wait for Docker Desktop to be ready (it auto-starts as Windows service)
$dockerTries = 0
while ($dockerTries -lt 60) {
    $dockerStatus = $null
    try { $dockerStatus = docker info 2>&1 | Select-String "Containers" } catch {}
    if ($dockerStatus) {
        Log "Docker is ready"
        break
    }
    Log "Waiting for Docker... (attempt $($dockerTries + 1)/60)"
    Start-Sleep -Seconds 5
    $dockerTries++
    if ($dockerTries -eq 30) {
        # Try to start Docker Desktop app explicitly
        $ddPath = "${env:ProgramFiles}\Docker\Docker\Docker Desktop.exe"
        if (Test-Path $ddPath) {
            Log "Starting Docker Desktop app explicitly"
            Start-Process $ddPath
        }
    }
}

if ($dockerTries -ge 60) {
    Log "ERROR: Docker not ready after 300s — continuing without Firecrawl"
} else {
    # 2. Start Firecrawl stack
    Log "Starting Firecrawl stack"
    Push-Location $FIRECRAWL_DIR
    docker compose up -d 2>&1 | Out-File -FilePath $LOG_FILE -Append -Encoding UTF8
    Pop-Location

    # 3. Wait for Firecrawl API to be healthy
    $fcTries = 0
    while ($fcTries -lt 30) {
        try {
            $health = Invoke-RestMethod -Uri "http://127.0.0.1:3002/health" -TimeoutSec 5 -ErrorAction Stop
            Log "Firecrawl is healthy"
            break
        } catch {
            Log "Waiting for Firecrawl... (attempt $($fcTries + 1)/30)"
            Start-Sleep -Seconds 5
            $fcTries++
        }
    }
    if ($fcTries -ge 30) {
        Log "WARNING: Firecrawl not healthy after 150s — daemon will run without social intelligence"
    }
}

# 4. Start the watchdog (which launches the daemon)
Log "Starting watchdog"
$launchScript = "$REPO\rust\launch_watchdog.sh"
$gitBash = "${env:ProgramFiles}\Git\bin\bash.exe"
if (Test-Path $gitBash) {
    # Set environment variables and launch watchdog via git-bash
    $env:PQ_CREDS_FILE = "$env:USERPROFILE\.hermes\creds\pump-quant.env"
    $env:PQ_LASERSTREAM_BIN = "$REPO\\tools\\stream-capture-rs\\target\\release\\pq-stream-capture.exe"
    $env:PQ_FIRECRAWL_BIN = "$REPO\\tools\\firecrawl-bridge-rs\\target\\release\\pq-firecrawl-bridge.exe"
    $env:RUSTFLAGS = "-C target-cpu=znver5"
    
    $daemonArgs = "--junction-cap 8192 --commitment processed --status-every-ticks 50 --brain-snapshot-every-ticks 5000 --tape-every-ticks 1000"
    $watchdogArgs = "--max-restarts 5 --health-timeout-secs 60 --backoff-cap-secs 30"
    
    Start-Process -FilePath $gitBash -ArgumentList "-c", "cd '$REPO/rust' && bash launch_watchdog.sh --daemon-args '$daemonArgs' -- $watchdogArgs" -WindowStyle Minimized
    Log "Watchdog launched"
} else {
    Log "ERROR: Git bash not found at expected path"
}

# 5. Register Hermes cron jobs (they persist across reboots via Hermes scheduler)
Log "Hermes cron jobs are managed by Hermes scheduler — no action needed"

Log "=== pq-startup END ==="
