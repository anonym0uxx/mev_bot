# Pump Quant — Live Momentum Trade Analysis & Profitability Briefing

## Current State
- **Wallet:** 1.4670 SOL (started ~1.50 SOL, lost ~0.033 SOL on real TX fees/tips)
- **Mode:** PAPER (paper accounting, but real buy/sell TXs fire on-chain for testing)
- **Engine:** Rust v5, PumpSwap + Raydium pool support

## Trade Data Summary (776 paper trades)

### Overall Stats
- **Trades:** 776
- **Win Rate:** 7.6% (59 wins / 717 losses)
- **Net PnL:** +0.243 SOL (positive despite 7.6% WR!)
- **Gross PnL:** +0.700 SOL
- **Total Fees:** 0.457 SOL (paper-estimated, not real)
- **Avg Win:** +0.01351 SOL (huge winners)
- **Avg Loss:** -0.00077 SOL (small losses)
- **Win/Loss Ratio:** 17.49x (wins are 17.5x bigger than losses)

### Exit Breakdown (CRITICAL)
| Exit Reason | Count | % | WR | Net PnL | Avg Hold |
|---|---|---|---|---|---|
| time_sl | 709 | 91.4% | 3% | -0.288 SOL | 10.8s |
| trailing_stop | 38 | 4.9% | 95% | +0.433 SOL | 62.1s |
| hard_sl | 26 | 3.4% | 0% | -0.110 SOL | 2.7s |
| max_hold | 2 | 0.3% | 100% | +0.209 SOL | 165.1s |
| drain_detected | 1 | 0.1% | 0% | -0.001 SOL | 1.2s |

### Key Insight
The strategy IS profitable (+0.243 SOL net) but has a MASSIVE time_sl problem:
- 91% of trades exit via time_sl (60s timeout) with only 3% WR
- The 5% that reach trailing_stop have 95% WR and generate +0.433 SOL
- **The time_sl is killing positions before they can develop momentum**

### Score Distribution
| Score Range | Count | WR | Net PnL |
|---|---|---|---|
| 20-29 | 20 | 5% | -0.002 |
| 30-39 | 18 | 0% | -0.009 |
| 40-49 | 52 | 8% | +0.184 |
| 50-59 | 429 | 4% | -0.237 |
| 60-69 | 105 | 18% | +0.221 |
| 70-79 | 152 | 11% | +0.087 |

### Pool Type Breakdown
| Pool | Count | WR | Net PnL |
|---|---|---|---|
| raydium_amm_v4 | 352 | 13% | +0.482 |
| pump_swap | 424 | 3% | -0.239 |

**Raydium trades are significantly more profitable than PumpSwap trades.**

### Time of Day (UTC)
Best hours: 4:00 UTC (+0.107), 5:00 UTC (+0.064), 7:00 UTC (+0.151), 17:00 UTC (+0.244)
Worst hours: 15:00 UTC (-0.058), 16:00 UTC (-0.210)
**16:00 UTC = 9:00 AM PDT has 309 trades but only 2% WR — worst hour.**

### Last 50 Trade Pattern
- Dominated by `7dpaUoCb` re-entries (same established token traded 20+ times)
- Most exits: time_sl at exactly 8.1s hold (observation window + a few ticks)
- gain=0bps on many trades (flat price, no momentum)
- hard_sl trades losing 180-520 bps (big instant drops)
- Trailing stop winners: 226-938 bps gains when they hit

### Streak Analysis
- Max win streak: 4
- Max loss streak: **169** (!)
- Avg win: 31,111 bps (very large, likely outlier-driven)
- Avg loss: -38 bps (tiny)

## Current Configuration

### Sizing
- Probe size: 0.03 SOL (all trades currently use probe)
- Scale-in available but rarely triggers
- Kelly sizing: DISABLED

### Entry
- Min graduation score: 30
- Observation window: 5s (from config, 6s in code)
- Time SL: 60,000ms (60s) — BUT paper trades show 8.1s holds, suggesting a different effective timeout

### Exit
- Hard SL: 10% (-1000 bps)
- Trailing stop: 15% from peak
- Time SL: 60s
- Dead zone detection (multiple flavors)
- Velocity/acceleration exits

## Key Questions for Architects

1. **Why is effective hold time only 8.1s when time_sl is 60s?**
   - Dead zone detection is probably exiting early (8s = dead_zone_reserve_flat_min_hold_ms)
   - Dead zone configs: ws_zero=8-10s, reserve_flat=8s, price_flat=12s
   
2. **Why is WR so low (7.6%) but strategy profitable?**
   - Massive win/loss asymmetry (17.49x)
   - The few trailing_stop winners carry the entire P&L
   - 91% of positions are killed by time stops before momentum develops
   
3. **How to reach 55% WR target?**
   - Need to either: (a) only enter trades likely to momentum, or (b) let positions run longer
   - Score filtering: 60+ score has 18% WR vs 4% for 50-59
   - Pool filtering: Raydium 13% WR vs PumpSwap 3% WR
   - Dead zone tuning: relax early exits to let positions develop

## Files for Reference
- Trade log: `/data/.openclaw/workspace/projects/pump-quant/data/momentum_paper_trades.jsonl`
- Config: `/data/.openclaw/workspace/projects/pump-quant/config/canary.json`
- Engine: `/data/.openclaw/workspace/projects/pump-quant/rust/pump-quant-core/src/momentum/mod.rs` (4930 lines)
- Kelly: `/data/.openclaw/workspace/projects/pump-quant/rust/pump-quant-core/src/engine/kelly_sizing.rs`
- Scoring: Graduation scoring is in `mod.rs` around lines 800-1000
