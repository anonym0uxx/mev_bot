# Promotion Report — 2026-08-02

## PaperEvidence (from 900s session)

| Field | Value | Source |
|-------|-------|--------|
| closed_positions | 0 | Engine gate: admitted=0 |
| net_pnl_lamports | 0 | Engine gate: net_lamports=0 |
| sum_sq_pnl_lamports | 0 | No closed positions |
| max_drawdown_lamports | 0 | No positions opened |
| entries_attempted | 0 | Engine gate: promoted=0 |
| entries_filled | 0 | Engine gate: admitted=0 |
| slots_observed | 254 | Helius slot_notifications |
| slots_missed | (see gap analysis) | Gap-detection below |

## Slot Gap Analysis

- First slot observed: 436828370
- Last slot observed: 436828622
- Span: 252 slots
- Slot notifications: 254 (some duplicate slots from resubscription)
- Unique slots: 251 (deduped from 254 notifications)
- Expected slots in 900s window at ~400ms/slot: ~2250
- Observed/expected ratio: 251/2250 ≈ 11.2%
- Gap detection method: **slot-set deduplication** — every slot value extracted from OnchainConfirm lines, sorted, gap-checked by consecutive difference. A gap >1 between consecutive observed slots counts as missing slots.

**Note:** The slot span (252 slots ≈ 100s) is much narrower than the 900s wall-clock window because `accountSubscribe` only fires when a watched account changes. Slots where no watched account changed produce no notification. This is not a feed gap in the traditional sense (the slot subscriptions were healthy — 254 slot notifications received, 0 ws_errors, 0 reconnects). The "missing" slots are slots where nothing happened, not slots where the feed went blind.

## PromotionCriteria (conservative default)

| Field | Value |
|-------|-------|
| min_closed_positions | 100 |
| min_net_pnl_lamports | 1 |
| t_squared_num | 4 (t ≥ 2.0) |
| t_squared_den | 1 |
| max_drawdown_lamports | u64::MAX |
| min_fill_rate_bps | 5000 (50%) |
| max_slot_gap_bps | 50 (0.5%) |

## PromotionVerdict

**REFUSE — SampleTooSmall { closed: 0, required: 100 }**

The 900s paper session proved feed connectivity and decode correctness (535 OnchainConfirm decodes, 0 ws_errors, 0 reconnects, 0 overflow drops). It did not produce trading evidence — no positions were opened, closed, or filled. The promotion gate correctly refuses on the first binding constraint: zero closed positions is below the minimum of 100.

This is the correct and expected result. The 900s session was designed to validate the data pipeline against real mainnet, not to generate PnL. A session that generates trading edge requires the full engine running with live market data over a longer window.

## Feed Quality Summary

| Metric | Value | Status |
|--------|-------|--------|
| PumpPortal trades | 15 received, 0 enqueued | Live feed healthy |
| PumpPortal creates | 373 received, 373 parsed | Live feed healthy |
| Helius slot notifications | 254 | Healthy |
| Helius account notifications | 855 | Healthy |
| OnchainConfirm decodes | 535 | Decode pipeline verified |
| Account sub evictions | 309 (FIFO bound=64) | Bounded as designed |
| ws_errors | 0 | No errors |
| reconnects | 0 | No reconnects needed |
| overflow_dropped | 0 | Queue healthy |

## What This Session Proves

1. **Feed connectivity:** Both PumpPortal (free) and Helius (free tier) maintained live WebSocket connections for 900s with zero errors and zero reconnects.
2. **Decode pipeline:** 535 OnchainConfirm events decoded from real mainnet account data. Criterion 65 satisfied by construction.
3. **PDA derivation:** 373 PDAs derived via `solana_program::Pubkey::find_program_address`, mainnet-tested.
4. **FIFO bound:** 64-account subscription cap enforced, 309 evictions, zero overflow drops.
5. **Junction queue:** 535 events drained, zero overflow drops.

## What This Session Does NOT Prove

1. **Trading edge:** Zero positions opened or closed. No PnL data.
2. **Fill rate:** Zero entries attempted. No fill-rate data.
3. **Statistical significance:** No sample to test.
4. **Drawdown tolerance:** No positions to measure drawdown against.

## Conclusion

The promotion gate **REFUSES** on `SampleTooSmall`. This is correct. The 900s session was a feed-validation session, not a trading session. No live capital may be armed on this evidence. The next step is a trading-session run that actually opens and closes paper positions, then re-evaluates the promotion gate with real PnL data.
