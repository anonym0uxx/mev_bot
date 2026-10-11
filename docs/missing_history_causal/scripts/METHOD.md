# Pre-registered method (written BEFORE looking at any flagged-row outcome in this analysis)
Data: raw parts only (account/transaction/slot records); /tmp/mh_recon tsv/jsonl only used to read the producer's own outcome labels.
Producer replica: per curve account, ARRIVAL order = raw-file order (record_index). Flag = Rust rules: dv!=0&&dt!=0 and same-sign, or vtoken==0 (not i64-fit impossible here).
## Ordering key (from first principles)
Solana executes txs within a slot in tx_index order; an account's state after tx k is the write of tx k. LaserStream account
updates carry (slot, write_version, txn_signature). Pre-registered primary key:  K = (slot, tx_index(txn_signature_b58 via transaction records of same slot), write_version, record_index).
Updates with no signature (startup/snapshot) sort first within slot by write_version. Alternative key K2=(slot, write_version, record_index) reported as sensitivity. No key tuning after seeing results.
Reorder test: sort each curve account's snapshots by K, recompute prev/curr deltas with the same rule; count flagged rows (by arrival) that become valid/stay flagged/zero.
## Acceptance test for an alternative explanation (independent per-tx evidence)
Each pump tx carries a TradeEvent (inner instruction to 6EF8 with disc e445a52e51cb9a1d bddb7fd34ee661ee, mirrored in 'Program data' log) with that trade's
sol_amount, token_amount, is_buy and POST-trade virtual/real reserves. A consecutive pair (A,B) is "explained" only if B's tx TradeEvent: post-reserves == snapshot B reserves exactly
(vsol,vtok,rsol,rtok) AND pre-reserves reconstructed (post -/+ amounts) == snapshot A reserves exactly. A "recoverable lost step" requires: tx record(s) for the
missing step(s) exist, chaining their TradeEvents from A's reserves reproduces B's reserves exactly.
Classification of remaining flagged rows uses only: tx status, tx presence, signature->tx_index, write counts per (curve,slot) vs txs touching curve, complete flag (byte 48), data_len, slot status records.
Age: first_seen = first snapshot in capture; token age from create event in events file if present else 'unknown'.
