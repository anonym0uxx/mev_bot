# Pump-Quant Restart Instructions

## Changes Applied (Ready to Deploy)

### Code Changes (Rebuilt)
1. **Entry expected return:** 0.15 → **0.12** (widens gate 20%)
2. **Adverse selection penalty:** reduced by 15% (0.85 scalar)
3. **Dynamic slippage:** increased by 20% (1.2 multiplier) for single-feed mode

### Config Changes (`config/default.json`)
```json
{
  "risk": {
    "quick_spend_sol": 0.01,           // Down from 0.05
    "max_position_size_sol": 0.05,     // NEW hard cap
    "max_daily_entries": 3,            // NEW circuit breaker
    "raw_stop_pct": 0.25,              // Up from 0.10 (hard stop -25%)
    "take_profit_pct": 0.40,           // NEW (+40% take profit)
    "max_daily_loss_sol": 0.15         // Up from 0.10
  },
  "entry": {
    "min_p_continuation": 0.68         // NEW explicit floor
  },
  "exit": {
    "max_hold_time_s": 900             // 15 minutes (was 300)
  }
}
```

## Restart Command

```bash
cd /data/.openclaw/workspace/projects/pump-quant
pkill -f "run-daemon.sh"
pkill -f "node dist/daemon"
sleep 2
rm -f data/pump-quant.db-shm data/pump-quant.db-wal
nohup bash run-daemon.sh > logs/supervisor.log 2>&1 &
sleep 3
curl -s http://127.0.0.1:9420/api/health | jq '.data.overall'
curl -s -X POST http://127.0.0.1:9420/api/control/resume
```

## Verification

After restart:
```bash
# Check health
curl -s http://127.0.0.1:9420/api/health | jq '.data | {overall, tradingAllowed}'

# Monitor entry decisions
tail -f logs/daemon.log | grep "Entry eval"

# Expect: EV should be less negative now (closer to 0)
# Target: 1-3 entries per 24h with P_continuation ≥ 0.68
```

## CoreCast Issue

CoreCast connects but streams close immediately. This appears to be a Bitquery API issue (query syntax or rate limit). The system will continue on PumpPortal-only mode with degraded data quality compensation applied.

**Fallback active:** single-feed adjustments (reduced adverse selection, increased slippage estimates).

**If CoreCast recovers:** revert these params:
- `E_return_continuation`: 0.12 → 0.15
- `adverse_selection_scalar`: 0.85 → 1.0
- `dynamic_slippage_multiplier`: 1.2 → 1.0
