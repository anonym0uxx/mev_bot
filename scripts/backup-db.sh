#!/bin/bash
# Daily SQLite backup — run via cron
BACKUP_DIR="/data/.openclaw/workspace/projects/pump-quant/data/backups"
DB_PATH="/data/.openclaw/workspace/projects/pump-quant/data/pump-quant.db"
mkdir -p "$BACKUP_DIR"
DATE=$(date +%Y-%m-%d)
DEST="$BACKUP_DIR/pump-quant-$DATE.db"
if [ ! -f "$DEST" ]; then
  cp "$DB_PATH" "$DEST"
  echo "[$(date)] Backup created: $DEST"
  # Keep last 7 days only
  find "$BACKUP_DIR" -name "*.db" -mtime +7 -delete
else
  echo "[$(date)] Backup already exists for today: $DEST"
fi
