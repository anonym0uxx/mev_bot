# Phase 1 Quick Wins: Implementation Checklist
**Estimated effort:** 1-2 hours  
**Expected impact:** 30-40% false ban reduction, 1s faster exits

---

## Config Changes (canary.json)

### 1. Extend Observation Window
**File:** `config/canary.json`

```json
"entry": {
  "observation_window_s": 5,  // Changed from 3
  "min_trades_for_analysis": 15,  // NEW — don't analyze until 15+ trades
  // ... rest unchanged
}
```

---

### 2. Speed Up Position Scanner
**File:** `src/daemon/index.ts:174`

**Current:**
```typescript
setInterval(() => {
  this.scanOpenPositions();
}, 2000);
```

**Change to:**
```typescript
setInterval(() => {
  this.scanOpenPositions();
}, 500); // 4x faster for active exits
```

---

## Code Changes (daemon/index.ts)

### 3. Add Trade Count Gate (Analysis Loop)

**File:** `src/daemon/index.ts` — in `_analyzeTokenInner()` method (around line 473)

**Add this check BEFORE feature computation:**

```typescript
private _analyzeTokenInner(packet: CandidatePacket, config: PumpQuantConfig, health: SystemHealth): void {
  const mint = packet.mint;

  // NEW: Defer analysis until sufficient trade density
  const tradeCount = this.featureEngine.getTradeCount(mint);
  if (tradeCount < (config.entry.min_trades_for_analysis || 15)) {
    // Not enough data yet — skip analysis
    return;
  }

  // Compute features (existing code continues...)
  const features = this.featureEngine.computeFeatures(mint);
  // ... rest of function unchanged
}
```

---

### 4. Add `getTradeCount()` to FeatureEngine

**File:** `src/features/engine.ts`

**Add this public method:**

```typescript
/** Get current trade count for a token (for analysis gating) */
getTradeCount(mint: string): number {
  const state = this.tokenStates.get(mint);
  return state?.trades.length || 0;
}
```

---

### 5. Age-Adjusted Manipulation Thresholds

**File:** `src/daemon/index.ts` — in `_analyzeTokenInner()` (around line 509)

**Current:**
```typescript
// For non-position tokens: only hard-ban on creator_sell immediately.
if (manipAssessment.hardShock) {
  const isCreatorSell = manipAssessment.hardShockReason === 'creator_sell';
  if (isCreatorSell || tokenAgeSec > config.entry.observation_window_s) {
    this.stateMachine.transitionToBan(mint, `Manipulation shock: ${reason}`);
    return;
  }
}
```

**Change to:**
```typescript
// For non-position tokens: only hard-ban on creator_sell immediately.
// Other hard shocks: defer until token is 8s old (double observation window)
if (manipAssessment.hardShock) {
  const isCreatorSell = manipAssessment.hardShockReason === 'creator_sell';
  const minAgeForHardBan = config.entry.observation_window_s * 1.6; // 8s when window=5s
  
  if (isCreatorSell || tokenAgeSec > minAgeForHardBan) {
    this.stateMachine.transitionToBan(mint, `Manipulation shock: ${reason}`);
    return;
  }
  // Young token with non-creator hard shock: log but don't ban yet
  log.debug(`${mint}: Hard shock (${manipAssessment.hardShockReason}) deferred, age=${tokenAgeSec.toFixed(1)}s`);
}
```

---

## Testing Steps

### 1. Compile
```bash
cd /data/.openclaw/workspace/projects/pump-quant
npm run build
```

### 2. Backup Current Database
```bash
cp data/pump-quant.db data/pump-quant.db.backup-phase1
```

### 3. Restart Daemon (Paper Mode)
```bash
pkill -f "node dist/daemon"
PAPER_MODE=1 nohup bash run-daemon.sh > /dev/null 2>&1 &
```

### 4. Monitor Logs
```bash
tail -f data/bot.log | grep -E "BAN|ENTER_READY|Manipulation|Trade count"
```

### 5. Check Metrics (After 30 Minutes)
```bash
curl -s http://127.0.0.1:9420/api/health | jq
curl -s http://127.0.0.1:9420/api/positions | jq
```

**Look for:**
- Fewer "BAN: Manipulation shock" events in first 5-8s
- More tokens reaching ENTER_READY state
- Average hold duration increasing (target: >30s)

---

## Validation Queries

**Check ban rate before/after:**

```bash
# Total tokens discovered
node -e "const db = require('better-sqlite3')('data/pump-quant.db'); console.log('Total tokens:', db.prepare('SELECT COUNT(*) as c FROM token_state').get().c);"

# Tokens banned
node -e "const db = require('better-sqlite3')('data/pump-quant.db'); console.log('Banned:', db.prepare('SELECT COUNT(*) as c FROM token_state WHERE current_state = ?').get('BAN').c);"

# Ban reasons distribution
node -e "const db = require('better-sqlite3')('data/pump-quant.db'); db.prepare('SELECT ban_reason, COUNT(*) as c FROM token_state WHERE current_state = ? GROUP BY ban_reason ORDER BY c DESC').all('BAN').forEach(r => console.log(r.ban_reason, r.c));"
```

---

## Rollback Plan

**If false ban rate doesn't improve or system becomes unstable:**

```bash
# 1. Stop daemon
pkill -f "node dist/daemon"

# 2. Restore backup config
git checkout config/canary.json

# 3. Restore code changes
git checkout src/daemon/index.ts src/features/engine.ts

# 4. Rebuild and restart
npm run build
nohup bash run-daemon.sh > /dev/null 2>&1 &
```

---

## Success Criteria

**After 24 hours of paper trading:**

| Metric | Current (Baseline) | Phase 1 Target |
|--------|-------------------|----------------|
| Ban rate | ~95% | <70% |
| False ban rate | ~80% | <50% |
| Avg hold time | <15s | >30s |
| Tokens reaching ENTER_READY | ~5% | >20% |
| Position scanner exit latency | ~2s | <800ms |

**If targets met:** Proceed to Phase 2 (event-driven analysis).  
**If not:** Review logs, adjust thresholds, iterate.

---

## Notes

- All changes are **backward-compatible** (no schema changes)
- Config changes can be reverted without rebuild
- Trade count gate is **non-blocking** (returns early if insufficient data)
- Position scanner speedup has **no downside** (pure performance gain)

---

**Ready for implementation. Recommend starting with config changes only, then code changes after validation.**
