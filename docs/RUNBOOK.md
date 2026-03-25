# Pump Quant Bot — Runbook

## First-Run Checklist

### Prerequisites
- [ ] Node.js 20+ installed
- [ ] Solana wallet created (dedicated hot wallet, NOT your main wallet)
- [ ] PumpPortal API key obtained
- [ ] Bitquery API key obtained
- [ ] VPS with stable internet connection

### Setup Steps

1. **Clone and install:**
   ```bash
   cd pump-quant
   npm install
   npm run build
   ```

2. **Configure environment:**
   ```bash
   cp .env.example .env
   # Edit .env:
   # - WALLET_PRIVATE_KEY: base58-encoded private key of dedicated hot wallet
   # - WALLET_PUBLIC_KEY: corresponding public key
   # - PUMP_PORTAL_API_KEY: your PumpPortal key
   # - BITQUERY_API_KEY: your Bitquery key
   # - SOLANA_RPC_URL: your RPC endpoint (mainnet)
   ```

3. **Fund hot wallet:**
   - Transfer ONLY the amount you're willing to risk
   - Recommended canary: 0.5 SOL
   - Keep bulk funds in cold storage

4. **Set quick_spend:**
   - Edit `config/canary.json`: `"quick_spend_sol": 0.02`
   - Or set via chat after boot: `set quick_spend 0.02`

5. **Run in paper mode first:**
   ```bash
   PAPER_MODE=true npm start
   ```
   - Observe for at least 24 hours
   - Check paper fills match expectations

6. **Switch to canary live:**
   ```bash
   CONFIG_PATH=config/canary.json npm start
   ```

## Paper Mode Operation

Paper mode uses the live PumpPortal feed but executes synthetic fills:
- All decision logic runs identically to live mode
- Fills simulate realistic slippage and latency
- All synthetic fills persisted to `orders` and `positions` tables with `is_paper=1`
- Monitor via `positions` and `pnl` commands

### Validation Metrics
- Net expectancy per trade > 0
- Hit rate > 50%
- Max drawdown < max_daily_loss_sol
- No unexpected forced exits
- Entry/exit decisions match manual analysis

## Canary Deployment

Canary mode (`config/canary.json`) settings:
- 1 position max
- 0.02 SOL quick_spend (tiny)
- 0.5 SOL bankroll
- Tighter entry filters (more edge required)
- Route promotion disabled
- No Mayhem, no Tokenized-Agent

### Canary Graduation
Promote from canary to default when:
- 50+ live trades completed
- Net expectancy positive
- Max drawdown within bounds
- No system failures
- Paper/live fill discrepancy < 20%

## Monitoring

### Health Check
Run `health` command to see all subsystems:
- ✅ market_feed — PumpPortal WebSocket connected
- ✅ friction_estimate — Slippage estimates fresh
- ✅ probability_layer — Feature computation running
- ✅ datastore — SQLite write path working
- ✅ execution_adapter — Can send transactions
- ✅ config_integrity — Config validates

### Auto-Pause Triggers
The bot will auto-pause (no new trades) if:
- Market feed stale > 15s
- Execution adapter stale > 60s
- Config integrity failure
- Daily loss limit reached

### Log Monitoring
```bash
# Follow daemon logs
tail -f data/pump-quant.log

# Check for errors
grep ERROR data/pump-quant.log | tail -20
```

## Common Issues

### "PumpPortal WebSocket disconnected"
- Check internet connection
- Check PumpPortal service status
- Bot will auto-reconnect with exponential backoff
- Trading auto-pauses until reconnected

### "Stale friction estimate"
- Friction estimates expire after 30s without trades
- Normal for low-activity tokens
- Bot won't enter with stale friction (fail-safe)

### "Max positions reached"
- Canary: 1 max position
- Wait for current position to close or manually sell
- Increase `max_positions` only after canary validation

### "Config validation failed"
- Check config JSON syntax
- Run: `node -e "require('./config/default.json')"`
- Verify against schema: all required fields present

### "WALLET_PRIVATE_KEY not set"
- Ensure `.env` file exists with key
- Key must be base58-encoded
- Restart daemon after changing .env

## Database Management

### Backup
```bash
# SQLite backup (safe with WAL mode)
cp data/pump-quant.db data/pump-quant.db.backup
```

### Query
```bash
# Open SQLite shell
sqlite3 data/pump-quant.db

# Recent positions
SELECT mint, symbol, status, realized_pnl_sol FROM positions ORDER BY opened_at DESC LIMIT 10;

# Daily PnL
SELECT date(closed_at/1000, 'unixepoch') as day, SUM(realized_pnl_sol) as pnl
FROM positions WHERE status='closed' GROUP BY day ORDER BY day DESC;

# Health events
SELECT * FROM health_events ORDER BY timestamp DESC LIMIT 20;
```

## Emergency Procedures

### Immediate Stop
```bash
# Via chat
pause

# Via signal
kill -SIGINT <daemon_pid>
```

### Force Exit All Positions
```bash
# Via chat - sell each position
sell <mint> 100
```

### Config Rollback
Config versions are persisted in `config_versions` table:
```sql
-- Find previous versions
SELECT version, source, description, datetime(timestamp/1000, 'unixepoch') FROM config_versions ORDER BY version DESC;
```

## Scheduled Learning Jobs

| Job | Cadence | What it does |
|-----|---------|-------------|
| Micro-calibration | Hourly | Adjusts slippage, landing-risk, route-health estimates |
| Daily replay | Daily (04:00 UTC) | Replays past 24h, computes attribution |
| Canary promotion | Daily (after replay) | Evaluates challenger configs |
| Deep retrain | Weekly (Sunday 06:00 UTC) | Regime review, weight adjustment proposals |
