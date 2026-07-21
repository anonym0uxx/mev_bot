#!/bin/bash
# Auto-commit and push any uncommitted changes
cd /data/.openclaw/workspace/projects/pump-quant
if [[ -n $(git status --porcelain) ]]; then
  git add -A
  git commit -m "auto: ${1:-periodic sync $(date -u +%Y-%m-%dT%H:%M:%SZ)}"
  git push origin main
  echo "Pushed to origin/main"
else
  echo "Nothing to commit"
fi
