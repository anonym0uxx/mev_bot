#!/bin/bash
# Monitor PumpPortal access and notify when it comes back
LOG="/data/.openclaw/workspace/projects/pump-quant/logs/overnight.log"
CHECK_INTERVAL=120  # 2 minutes

log() {
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" | tee -a "$LOG"
}

check_pumpportal() {
  cd /data/.openclaw/workspace/projects/pump-quant
  result=$(node -e "
const WebSocket = require('ws');
const ws = new WebSocket('wss://pumpportal.fun/api/data');
ws.on('open', () => { process.stdout.write('UP'); ws.close(); process.exit(0); });
ws.on('error', () => { process.stdout.write('DOWN'); process.exit(1); });
setTimeout(() => { process.stdout.write('TIMEOUT'); process.exit(2); }, 5000);
" 2>/dev/null)
  echo "$result"
}

check_wallet() {
  cd /data/.openclaw/workspace/projects/pump-quant
  set -a && . ./.env && set +a
  node -e "
const { Connection, PublicKey, LAMPORTS_PER_SOL } = require('@solana/web3.js');
const conn = new Connection(process.env.SOLANA_RPC_URL || 'https://api.mainnet-beta.solana.com', 'confirmed');
conn.getBalance(new PublicKey(process.env.PUMP_PORTAL_PUBLIC_KEY))
  .then(b => process.stdout.write((b/LAMPORTS_PER_SOL).toFixed(5)))
  .catch(() => process.stdout.write('error'));
" 2>/dev/null
}

log "=== PumpPortal Monitor Started ==="
was_down=true
last_wallet=""

while true; do
  status=$(check_pumpportal)
  wallet=$(check_wallet)
  
  if [ "$status" = "UP" ]; then
    if [ "$was_down" = "true" ]; then
      log "✅ PumpPortal RESTORED! Wallet: ${wallet} SOL"
      openclaw message send --channel telegram --target "telegram:5024153101" \
        --message "✅ PumpPortal WS is BACK! Wallet: ${wallet} SOL. Bot will resume trading automatically." 2>/dev/null
      was_down=false
    else
      log "✅ PumpPortal UP | Wallet: ${wallet} SOL"
    fi
  else
    log "⛔ PumpPortal ${status} | Wallet: ${wallet} SOL"
    if [ "$was_down" = "false" ]; then
      openclaw message send --channel telegram --target "telegram:5024153101" \
        --message "⚠️ PumpPortal went DOWN again. Status: ${status}" 2>/dev/null
    fi
    was_down=true
  fi
  
  # Check wallet milestones
  if [ "$wallet" != "$last_wallet" ] && [ "$wallet" != "error" ]; then
    # Convert to check for milestones using awk
    milestone=$(echo "$wallet" | awk '{
      if ($1 >= 1.2) print "1.2"
      else if ($1 >= 1.1) print "1.1"
      else if ($1 >= 1.0) print "1.0"
      else if ($1 >= 0.9) print "0.9"
      else if ($1 < 0.65) print "CRITICAL"
      else print ""
    }')
    
    if [ -n "$milestone" ] && [ "$milestone" != "CRITICAL" ] && [ "$milestone" != "$(cat /tmp/last_milestone 2>/dev/null)" ]; then
      echo "$milestone" > /tmp/last_milestone
      openclaw message send --channel telegram --target "telegram:5024153101" \
        --message "🎯 Wallet milestone: ${wallet} SOL (${milestone} SOL reached!)" 2>/dev/null
      log "MILESTONE: ${milestone} SOL"
    fi
    
    if [ "$milestone" = "CRITICAL" ]; then
      openclaw message send --channel telegram --target "telegram:5024153101" \
        --message "🚨 CRITICAL: Wallet dropped to ${wallet} SOL (< 0.65 SOL). Bot paused!" 2>/dev/null
      log "CRITICAL: Wallet below 0.65 SOL!"
    fi
    
    last_wallet="$wallet"
  fi
  
  sleep $CHECK_INTERVAL
done
