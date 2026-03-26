#!/bin/bash
# monitor-loop.sh — runs every 5 min via cron
# Checks bot health, runs loss analysis, writes state
# Apollo's main session handles subagent spawning for deeper analysis

cd /data/.openclaw/workspace/projects/pump-quant

HEALTH=$(curl -s http://127.0.0.1:9420/api/health 2>/dev/null | jq -r '.data.overall' 2>/dev/null)

if [ "$HEALTH" != "healthy" ]; then
  # Try restart
  pkill -f "bash run-daemon.sh" 2>/dev/null
  sleep 2
  nohup bash run-daemon.sh > logs/supervisor.log 2>&1 &
  echo "$(date): Daemon was $HEALTH — restarted" >> logs/monitor.log
fi

# Run analysis script
node scripts/analyze-losses.js 2>/dev/null >> logs/monitor.log

echo "$(date): health=$HEALTH" >> logs/monitor.log
