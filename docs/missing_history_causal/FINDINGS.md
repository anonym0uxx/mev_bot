# Missing-history causal analysis (sessions 20260909_144906_000490, 20260909_173928_000596)
Status: analysis only. Rust fix, regressions, CI, handoff NOT done. Scripts in scripts/, input hashes in INPUT_HASHES.txt (/tmp/mh_recon outputs are the producer labels).

## Where the one-per-slot reduction happens (traced)
- Capture subscribes accounts by owner (both programs), CONFIRMED, no interslot-updates flag, no data slice (capture.rs L105-132). Recorder writes every received Account update verbatim (capture.rs L360, raw_recorder.rs write()).
- Raw records: max 1 write per (account,slot) across ALL pump-owned accounts (284,349 / 272,571 non-curve; curves 148,753 / 155,878). A probe on one part shows 14,749 account records, all 1 per (acct,slot).
- => reduction happens at/before the provider's confirmed-commitment account stream (Yellowstone-style per-slot account dedup), NOT in our recorder (writes everything it receives) and NOT in our normalizer. Not proven which provider layer; recorder-side loss is excluded only to the extent that the recorder code writes unconditionally and manifest reports 0 gaps/dups/decode failures.
- Snapshot signature == last OK writer tx in slot: 144,419 vs 4,323 mismatches (S1); 152,793 vs 3,084 (S2). Mismatches are NOT explained here (open).
- Transactions are all present: every flagged multi-event slot's skipped trades exist as TradeEvents in raw tx records.

## Open / unresolved (do not close)
- 12/23 adjacent-event flagged cases (+ user-cited 35 total) unexplained; 65/42 unmatched unexplained.
- vtoken==0 flagged: 45/60 have B all-zero reserves, B tx carries 0 events, and A complete=1 in 44/59. This is lifecycle-CONSISTENT (completed/closed) but identity/lifecycle evidence (migration event, pool create binding) NOT yet joined -> still unclassified.
- Virtual SOL: unexplained ~10% of steps (dvs != sol_amount). Rebuild validated only vtoken/real-sol/real-token. No quote parity claimed.
- Session 2 "every slot mixed" vs 1,540/1,546: the 6 same-direction flagged rows in S2 are the reconciliation; statement was wrong for S2.
- Unflagged multi-event snapshots (S1 44,260) are NOT proven complete; same-direction aggregation loses counts/participants/fees.
