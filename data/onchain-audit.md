# On-Chain Wallet Audit — Full Parse
# Wallet: 7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC
# Date: 2026-04-02 23:25 PDT
# Range: Mar 24 → Apr 3 (10 days)

## Summary
- Total TXes: 866 (634 success, 232 failed)
- Swaps parsed: 459 (204 buys, 255 sells)
- Complete round trips: 167 (22 winners, 145 losers)
- Stuck tokens: 13 (0.219 SOL locked)
- Win rate: 13.2% (22/167)

## P&L
- Round trip P&L: -0.1627 SOL
- Stuck tokens: -0.2188 SOL
- TX fees (gas): -0.0176 SOL
- Failed TX gas: -0.0012 SOL
- **TOTAL: -0.4003 SOL**

## Key Patterns
- Massive cluster of -5.3% losses (AMM fee floor = buy+sell fees with 0 price movement)
- Winners avg +14.6% (22 trades)
- Losers avg -8.7% (145 trades)
- Best trade: 58WSMRURYYN +82.2%
- 13 stuck tokens = sell pipeline failures (0.219 SOL unrealized)
- Most holds: 0-2 seconds (immediate buy→sell, no time for price movement)
