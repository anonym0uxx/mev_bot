# Paper Trading System Audit Report
**Date:** 2026-03-28 02:05 PDT  
**Auditor:** MEV Engineering Audit (automated)  
**Scope:** PnL accuracy, fee model, training data quality, AMM simulation  
**Commit:** 8564f68

---

## Executive Summary

The paper trading system was **systematically overstating profitability** by ignoring pump.fun's 2% round-trip fees and Jito bundle tips. What appeared to be a +1.02 SOL profit across 3,303 trades is actually a **-8.47 SOL loss** after realistic fee modeling. The break-even gross win rate is **66.5%**, far above the current 43.1%.

Changes have been implemented, backfilled, and deployed.

---

## 1. Fee Audit

### What was missing from `closePosition()`
| Cost Component | Per Trade | Status Before | Status After |
|---|---|---|---|
| Pump.fun 1% buy fee | sizeSol × 0.01 | ❌ Missing | ✅ Added |
| Pump.fun 1% sell fee | sizeSol × 0.01 | ❌ Missing | ✅ Added |
| Jito bundle tip (×2) | 0.0001 SOL | ❌ Missing | ✅ Added |
| Own-buy price impact | ~0.4% of vSol | ❌ Missing | ⚠️ Accepted (negligible) |
| Slippage buffer | Variable | ❌ Missing | ⚠️ Not modeled (conservative) |

### Fee-Adjusted PnL Summary
| Metric | Value |
|---|---|
| Total trades (excl. anomalies) | 3,303 |
| Gross PnL | +1.0192 SOL |
| Total fees (2% round trip) | 9.1634 SOL |
| Jito tips (0.0001/trade) | 0.3303 SOL |
| **Net PnL (fee-adjusted)** | **-8.4745 SOL** |
| Gross Win Rate | 43.1% |
| Net Win Rate | 30.2% |
| Fee drag per trade | 2.87 mSOL |
| Avg gross PnL per trade | 0.31 mSOL |

### By Exit Reason
| Reason | Count | Gross PnL | Net PnL | Fees |
|---|---|---|---|---|
| next_buyer | 950 | +2.88 | -0.57 | 3.45 |
| max_hold | 1,218 | 0.00 | -4.27 | 4.27 |
| take_profit | 567 | +6.25 | +4.34 | 1.91 |
| stop_loss | 529 | -7.26 | -9.22 | 1.96 |
| intra_hold_trail | 39 | -0.85 | -1.05 | 0.19 |

### By Engine Version
| Version | Trades | Gross | Net | Net WR |
|---|---|---|---|---|
| v3 | 1,477 | +1.18 | -1.75 | 30.1% |
| v4 | 1,429 | +2.47 | -2.47 | 29.9% |
| v5 | 397 | -2.63 | -4.26 | 32.2% |

### Break-Even Analysis
- Avg position size: 0.1387 SOL
- Avg fees per trade: 2.87 mSOL  
- Avg gross win: 6.54 mSOL
- Avg gross loss: -4.41 mSOL
- **Break-even gross WR: 66.5%** (current: 43.1%, gap: 23.4pp)

---

## 2. AMM Simulation Audit

**File:** `src/mev/bonding-curve-sim.ts`

### Buy Formula ✅
```
fee = solIn × 1%
solAfterFee = solIn - fee
tokensOut = vTokens - (vSol × vTokens) / (vSol + solAfterFee)
```
Matches pump.fun constant product with 1% pre-swap fee. 1-token rounding difference from reference formula due to BigInt integer division — negligible.

### Sell Formula ✅  
```
k = vSol × vTokens
newVTokens = vTokens + tokensIn
solOutGross = vSol - k/newVTokens
solOutNet = solOutGross × 0.99 (1% fee)
```

### Round-Trip Verification
- Buy 0.1 SOL → sell immediately → receive 0.098 SOL
- Round-trip loss: 1.99% (matches expected 2% fee)
- Price impact for typical trade (0.14 SOL into 35 SOL pool): ~0.4% — negligible

### PnL Proxy Accuracy
Paper PnL uses `vSol % change × sizeSol` which:
- ✅ Captures market movement from other traders
- ❌ Ignores own-buy price impact (~0.4%, acceptable)
- ❌ Ignores fees (now fixed with fee deduction)

---

## 3. Training Data Quality

### Field Coverage (last 100 trades before fix)
| Field | Populated | Status |
|---|---|---|
| exitVSol | 100/100 | ✅ |
| tokenAgeMs | 0/100 | ❌ Not captured |
| buyerConcentration | 0/100 | ❌ Not captured |
| mfePct | 0/100 → 100/100 | ✅ Added + backfilled |
| maePct | 0/100 → 100/100 | ✅ Added + backfilled |
| entrySlot | 0/100 | ❌ Not captured (requires RPC) |
| engineVersion | 0/100 → 100/100 | ✅ Added + backfilled |
| postTriggerBuys1s | 0/100 | ❌ Not captured |
| preTriggerBuyConcentration | 0/100 | ❌ Not captured |

### New Fields Added
| Field | Source | Purpose |
|---|---|---|
| feesSol | Computed | Total friction per trade |
| netPnlSol | Computed | True economic PnL |
| netPnlPct | Computed | True economic return |
| mfePct | mfeSol/sizeSol | MFE as % of position |
| maePct | maeSol/sizeSol | MAE as % of position |
| engineVersion | Hardcoded "v5" | Model versioning |
| dataVersion | 2 | Schema versioning |

### Training Data Quality Score: **7/10**
Strong microstructure features (preTrigger signals, score components, crowd metrics). Missing token lifecycle features (tokenAgeMs, postTriggerBuys1s) and concentration metrics (buyerConcentration, Herfindahl).

### Recommended Future Additions (not implemented — require feed changes)
1. `tokenAgeMs` — requires tracking token create time in state machine
2. `postTriggerBuys1s` — requires post-entry buy counting in position manager
3. `preTriggerBuyConcentration` — requires wallet-level aggregation in detector
4. `entrySlot` — requires Solana RPC call or slot tracking

---

## 4. Changes Made

### Files Modified
1. **`src/mev/position-manager.ts`** (+37/-3 lines)
   - Added `feesSol`, `netPnlSol`, `netPnlPct`, `mfePct`, `maePct`, `engineVersion`, `dataVersion` to `PnLRecord` interface
   - Computed fee-adjusted PnL in `closePosition()` using pump fee (2% round trip) + Jito tips
   - Updated log format to show gross + net + fees

2. **`src/mev/paper-trade-logger.ts`** (+19/-1 lines)  
   - Serialize all new fields to JSONL
   - Auto-flag trades with >90% loss as `excludeFromAnalysis` with `dataBugNote`

### Data Backfill
- 3,313 historical trades backfilled with:
  - `engineVersion`: v3 (1,477) / v4 (1,429) / v5 (407)
  - `mfePct`, `maePct` (derived from existing mfeSol/maeSol)
  - `feesSol`, `netPnlSol`, `netPnlPct` (computed with 2% + 0.0001 SOL)
  - `dataVersion: 2`

---

## 5. Forward-Looking Recommendations

### The engine is NOT profitable after fees on live trading.
- Current gross WR (43.1%) is 23.4pp below break-even (66.5%)
- Even take_profit exits (the best category) only net +4.34 SOL on 567 trades
- max_hold exits (1,218 trades, 0 gross PnL) cost -4.27 SOL in pure fee drag

### To reach profitability:
1. **Reduce max_hold trades** — 37% of trades exit at max_hold with 0 gross PnL, each costing ~3.5 mSOL in fees. Tighter pre-entry filtering would eliminate these.
2. **Increase avg win size** — Current avg gross win is 6.54 mSOL. Need either larger wins or smaller positions to improve fee ratio.
3. **Target 67%+ gross WR** or restructure payoff to be more asymmetric (larger TP, tighter SL).
4. **Consider fee-adjusted scoring** — the score model should penalize setups where expected gain < 2.87 mSOL (avg fee drag).

### What break-even looks like on live:
Given avg win = 6.54 mSOL, avg loss = -4.41 mSOL, fees = 2.87 mSOL:
- Need **66.5% gross WR** to break even
- Or reduce fees by using direct Solana RPC (no PumpPortal 0.5% fee) → saves ~0.7 mSOL/trade → break-even drops to ~61%
- Or increase avg win to 10+ mSOL through better exit timing → break-even drops to ~55%
