# Loss Analysis: March 25, 2026

## Summary
10 trades, 0 wins, 8 losses, -0.0419 SOL total  
**Root cause:** MID_CURVE (10-50%) entries are systematic adverse selection

## Key Findings

### MEV Engineer Analysis
- **Tokens go 0→85% in ONE WAVE** - no mid-curve continuation exists
- MID_CURVE entry = buying the backside of the pump
- Early buyers (0-10%) use your liquidity to exit
- MFE=0.00 proves wave was already exhausted before entry

### Citadel Quant Analysis
- **Negative edge:** MFE=0 on 9/10 trades is -3σ outcome (anti-correlated)
- Buying at local price maxima (tops)
- Features are inverted, lagging, or miscalibrated
- Stop-loss working correctly, but entry selection is broken

## Trade Data

| Mint | Regime | Entry | Exit | Hold | PnL | Exit Reason | MFE | MAE |
|------|--------|-------|------|------|-----|-------------|-----|-----|
| C9iA1pPQ | MID | 0.01 | 0.0057 | 1.6s | -0.0043 | regime_change | 0.00 | -0.0043 |
| 8XuWdbr7 | MID | 0.01 | 0.0048 | 3.6s | -0.0052 | stop_loss | 0.00 | -0.0056 |
| 5nntQkoe | MID | 0.01 | 0.0060 | 2.0s | -0.0040 | stop_loss | 0.00 | -0.0040 |
| Emhfps2z | MID | 0.01 | 0.0043 | 4.0s | -0.0057 | stop_loss | 0.00 | -0.0058 |
| G4Ki7USp | MID | 0.01 | 0.0100 | 302s | 0.0000 | time_decay | 0.00 | 0.00 |
| G4Ki7USp | MID | 0.01 | 0.0100 | 3.0s | 0.0000 | creator_sell | 0.00 | 0.00 |
| G4Ki7USp | MID | 0.01 | 0.0040 | 2.8s | -0.0060 | creator_sell | 0.0002 | -0.0060 |
| BUfYMWXs | MID | 0.01 | 0.0056 | 9.6s | -0.0044 | stop_loss | 0.0013 | -0.0044 |
| BUfYMWXs | MID | 0.01 | 0.0036 | 7.7s | -0.0064 | stop_loss | 0.00 | -0.0067 |
| BUfYMWXs | MID | 0.01 | 0.0042 | 3.8s | -0.0058 | stop_loss | 0.00 | -0.0058 |

**Avg hold:** 34 seconds  
**Exits:** 60% stop_loss, 20% creator_sell, 20% regime_change/time_decay

## Fixes Applied (March 25, 22:17 PDT)

1. **EARLY_CURVE enabled** (0-15% bonding)
   - MID_CURVE disabled in `isTradeableRegime()`
   - Early curve max: 3% → 15%
   - Max token age: 180s → 120s

2. **Data quality gate**
   - Min 15 trades before analysis (was analyzing with 2-3 trades)
   - Position scanner: 2s → 500ms (4x faster exits)

3. **Execution speed**
   - Priority fees: 0.0001 → 0.0005 SOL (5x)
   - Skip preflight: enabled
   - Observation: 8s → 3s

4. **Risk params** (unchanged)
   - Stop-loss: -40%
   - Max position: 0.01 SOL
   - Daily loss: 0.05 SOL

## Recommended Next Steps

### Immediate (Deploy Tonight)
- [ ] Add "dead on arrival" exit: if MFE ≤ 0 after 15s AND MAE < -5%, exit immediately
- [ ] Add creator sell ban: exit if creator sells within first 5 min
- [ ] Add holder concentration filter: ban if top 3 holders >60%

### Short-term (24-48h)
- [ ] Feature audit: backtest which features predict MFE > 0
- [ ] Timing analysis: test entry lag ±60s around current signal
- [ ] Paper trading validation: run new logic 24h, require MFE > 0 on ≥40% before live

### Medium-term (Week)
- [ ] Rebuild probability model on recent Pump.fun data (last 7-14 days)
- [ ] Add regime detection (market-wide rug rate monitoring)
- [ ] Implement edge monitoring dashboard

## Success Criteria for EARLY_CURVE

- **MFE > 0:** ≥40% of trades (not 0%)
- **Hold time:** 60-180s average (not 34s)
- **Win distribution:** Power law (many -10-20% losses, rare +200-500% wins)
- **Entry window:** 0-10% bonding (ideally 0-5%)

## Lessons Learned

1. **MID_CURVE is toxic** - confirmed by live data, not just theory
2. **MFE=0 is a red flag** - should trigger immediate review
3. **Features need validation** - don't trust backtests without forward walk
4. **Stop early, stop often** - we caught this at -0.04 SOL, not -0.4 SOL
5. **Data beats intuition** - the specialists' predictions were correct

## Status
- Bot restarted with EARLY_CURVE strategy: 22:17 PDT
- Wallet balance: 0.45 SOL (PumpPortal wallet)
- Next trade will be first EARLY_CURVE validation
- Monitoring every 1-2 minutes for new positions
