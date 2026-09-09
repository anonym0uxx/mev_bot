# Stage 0 source recovery — verified execution receipt

Scope: preserve deleted historical raw evidence; no decoding, labels, training or purge.

- Original live worktree `D:/repos/mev_bot` left untouched.
- Isolated implementation worktree `D:/repos/mev_bot-north-star`, branch `task/north-star-build`, based on origin/main `d17645623f764931b0b81866a0a2819103e0acdd`; main unpushed count 0 at kickoff.
- Recovery implementation: `src/north_star/recovery.py` streams exact Git blob IDs, validates bytes/SHA256, fsyncs and atomically publishes without overwriting existing targets. Existing differing targets refused; matching targets independently verified on resume.
- Test-first receipt: initial run 1 failed (missing implementation); first green 1 passed; added no-clobber/resume tests produced 2 failed, 2 passed; final 4 passed. Tests use synthetic temporary Git objects, not teaching data.
- Recovered **184 parts / 3,270,751,952 bytes** from the August 23 session. Each matched its original capture manifest size and SHA256 before publication.
- Separate readback: **184 unique files, zero size/hash failures**.
- Durable source directory: `D:/mev_bot-artifacts/recovered_raw/20260823_133256_000398/`.
- Per-file fsynced provenance ledger: `RECOVERY_RECEIPTS.jsonl` in that directory. Source root is outside every existing transient purge glob.
- August 23 session has one other part already in original raw store; full-session canonical registry still needs both roots linked.
- August 24 `part0368` is not recovered; remains an explicit source/coverage gap.

A subsequent exhaustive raw SHA256 read verified both original manifests across the two store roots: August 23 **185/185** and August 24 **368/369**, with zero mismatches among present files. Full ledger: `D:/mev_bot-artifacts/north_star/build_receipts/HISTORICAL_RAW_INTEGRITY.json`. The only absent part remains August 24 part0368.

This closes this recovery task, not Stage 0 or dataset admission. No destructive disk, git cleanup/reset, rewrite, collector restart or original data edits performed.
