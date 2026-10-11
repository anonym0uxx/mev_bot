# Missing-history coverage measurement (offline replay, no launch)

Source: the two raw LaserStream captures under `/mnt/data/mev_bot-artifacts/north_star/capture/`
(2026-09-09, 120 min each; PT 07:49-09:49 and 10:39-12:39). Method: `missing_history_extract.py`
pulls pump.fun curve-account snapshots (owner 6EF8…, len 151) in capture order; the example
`missing_history_replay` runs the production `derive_market_trade_from_delta` +
`classify_delta_miss` per curve account exactly as the daemon does.

| | session A | session B |
|---|---|---|
| curve snapshots | 148,753 | 155,878 |
| curve accounts | 4,162 | 4,032 |
| derived trades | 142,808 | 149,324 |
| no_print | 4,664 | 4,883 |
| possible_trade (gate) | 1,281 | 1,671 |
| invalid_observation | 0 | 0 |
| accounts with >=1 gap | 231 (5.6%) | 288 (7.1%) |
| gap onward, share of observed curve-account time | 2.5% | 2.8% |
| first gap within 5 min of first sight | 94% | 93% |
| accounts with >=20 derived prints / of those gap-free | 849 / 628 | 884 / 617 |

Shapes of the possible_trade rows (A / B): same-sign down 737/979, same-sign up 499/632, zero token side 45/60.
An independent transaction record (events file, status=success) exists for 213/1,281 and 313/1,671 of them.

Not measured: held-position management downtime by venue/age (no held-position replay exists in the
captures; the time share above is a curve-account proxy, pump.fun only, PumpSwap not covered by
this producer), forgone PnL (none invented), real safety exclusions vs parser failures per refusal
name (needs a full engine replay of the tape; not done).
