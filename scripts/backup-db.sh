#!/usr/bin/env bash
# Nightly backup of pump-quant.db
set -euo pipefail

DB="/data/.openclaw/workspace/projects/pump-quant/data/pump-quant.db"
BACKUP_DIR="/data/.openclaw/workspace/projects/pump-quant/data/backups"
DATE=$(date +%Y-%m-%d)
DEST="$BACKUP_DIR/pump-quant-$DATE.db"

mkdir -p "$BACKUP_DIR"

if [ ! -f "$DB" ]; then
  echo "[$(date)] ERROR: source DB not found: $DB" >&2
  exit 1
fi

cp "$DB" "$DEST"
echo "[$(date)] Backup created: $DEST"

# Keep only last 7 backups
ls -t "$BACKUP_DIR"/pump-quant-*.db 2>/dev/null | tail -n +8 | xargs -r rm --
