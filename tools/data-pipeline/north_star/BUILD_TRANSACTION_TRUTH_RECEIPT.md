# Stage 2 transaction endpoint reconciliation foundation

## Result and boundary

Implemented `src/north_star/transaction_truth.py` with tests in
`tests/north_star/test_transaction_truth.py`. This is development-only **endpoint
balance arithmetic**, not an executed venue fill, trade classifier, ownership
cluster, inventory-cost ledger, source-admission gate or Stage 2 completion.

Only these two files and this receipt are owned/changed by this task. New output:
`D:/mev_bot-artifacts/north_star/development/transaction_truth/` containing
`reconciled.ndjson`, `rejected.ndjson`, `receipt.json`. Original source, Rust
collectors, live repository, other workers' files and configuration are untouched.
No commit, RPC, paid calls, rebuild, fit or training was performed.

## Contracts and source inspection

Read master `NORTH_STAR_WINDOWS_MASTER_V5.md` D03 and section 4.2 (lines 598–612),
and `BUILD_PLAN.md` S2.1–S2.3. Those require integer native units, explicit decimals,
separate failed-attempt costs and no transfer-to-trade relabeling.

Inspected preserved first100 raw lines and Rust
`tools/stream-capture-rs/grpc-server-only/src/raw_recorder.rs`: static message keys,
loaded writable then readonly keys, snake_case token `account_index`, `mint`,
`owner`, `program_id`, `ui_token_amount.amount` decimal string and `decimals`;
`pre_balances`/`post_balances` integer arrays; `fee`, `err_is_none`, `err_hex` and
full `signature_b58`/`signatures_b58` are explicit transaction metadata.

Legacy pitfalls inspected, not reused:
- Rust `normalizer.rs:422–469` uses generic instruction mint/trader positions and
  instruction arguments, including hardcoded fee_bps 100/0. These do not establish
  actual token-owner flow, actual fees or executed quantities.
- `BUILD_PLAN.md:485` records the legacy Python static + readonly + writable
  ordering defect. The new resolver uses static + writable + readonly and joins
  pre/post tokens by account index, never list zip.
- Rust `encoding.rs:9–28` initializes base58 digits to `[0]` then prepends every
  leading zero. All-zero 32-byte input therefore produces **33 literal `1`s**.
  This malformed key appears in every sampled transaction. It is preserved,
  never shortened, looked up or admitted as a canonical Solana key.

## Decoder semantics

- Validates raw JSON through the existing raw adapter: duplicate keys, scalar
  types, raw byte hash and full source provenance retained.
- Full signature must decode to 64 bytes; matching signature vector preserved.
- Complete ordered account-index domain includes both loaded groups. Duplicate
  keys, mismatched native arrays and bad token indices reject the transaction.
- Token mint/owner/program/account keys must be valid 32-byte base58 identities.
  Duplicate token indices, unknown/malformed owners, owner changes, mint/program
  changes and inconsistent decimals fail closed. Decimals must be integer u8;
  native/token amounts are u64; subtraction and sums use exact signed Python ints.
  UI amounts are not economic truth and are not used.
- Each observed token endpoint pair yields raw pre/post amounts, signed delta,
  exact account index/key, metadata owner, mint, program and decimals. Zero rows
  retained. SPL Token, Token-2022 and valid unknown programs remain distinguishable;
  no transfer-tax, quote-asset or fee assumptions are attached to them.
- Missing pre OR post token metadata rejects as `missing_token_counterpart`, even
  if a native endpoint is zero. Zero lamports alone is not sufficient evidence to
  synthesize token creation/closure balances. No account-snapshot/instruction
  proof join is implemented yet; creation/closure support is explicitly blocked.
- Native deltas are per account; observed total transaction fee is separate and
  marked already included in those deltas. Exact invariant:
  `sum(post_lamports) - sum(pre_lamports) + fee_lamports == 0`.
  No rent/tip/base/priority/venue-fee decomposition is invented.
- Failed transactions remain explicit with original error bytes and paid fee.
  Changed token balances on a failed transaction reject. Supported native
  endpoints are fee-only: index 0 delta equals `-fee`, all other deltas equal zero.
  Conserved but unexplained nonfee changes reject with
  `unsupported_failed_transaction_native_change`. Exceptional nonfee/durable-nonce
  effects lack explicit support here; rejection is not a claim they are impossible.
  No finality is inferred.
- Explicit loaded-readonly keys must have unchanged native and token endpoint
  amounts (`loaded_readonly_native_change` / `loaded_readonly_token_change`).
  Static writability is not inferred without message-header evidence.
- Malformed non-token keys permit only literal index-based arithmetic, with
  `canonical_identity_status=quarantined`, per-index validity and identity issue
  records. Token keys with this defect reject. Returned rows are always unadmitted
  and training-ineligible. `trader`, `side`, `quote_asset`, `executed_venue_fill`,
  `cost_breakdown` and `finality` remain null.
- Endpoint balances are not a transfer graph: transient intermediate accounts,
  withheld Token-2022 fees, beneficial owners and instruction-level netting remain
  unobserved. No largest-delta or signer-based trader inference is performed.

## Strict TDD / execution

Tests were written before implementation; the initial RED run failed with the
implementation unavailable. Windows execution required `PYTHONPATH=src`; without
it pytest could not reliably import the src-layout package. The corrected run
exposed 8 failures / 37 passes (received-time field and upstream integer-overflow
reason), then 45 passed after fixes. Sampler tests were added before sampler code:
**5 failed / 45 passed**, then the combined truth + raw-adapter suite returned:

```text
PYTHONPATH=src python -B -m pytest tests/north_star/test_transaction_truth.py tests/north_star/test_raw_adapter.py -q -p no:cacheprovider --tb=short
139 passed in 0.41s
```

The truth suite comprises 50 cases. Tests cover u64 max and exact one-unit deltas,
loaded writable/readonly mapping, mismatched arrays, owner change/absence,
decimal/mint/program inconsistency, missing endpoints, failed transactions,
Token-2022, unknown programs, identity quarantine, duplicate JSON/keys, input SHA,
new-only output, roundtrip hashes and the hard first100 / <=10 sampling bounds.

A broader concurrent-worker snapshot run returned **979 passed, 1 failed** in
7.17s. The failure was outside this task:
`test_reservation.py::test_checked_in_reservation_is_fixed_prospective_and_not_capture_evidence`
required `north_star/EVAL_RESERVATION_V3.md`, not yet present at that instant.
No reservation files were altered to mask it. A later snapshot after reservation
files arrived returned **1034 passed, 3 failed** in 7.20s; the three failures were
concurrent `test_media_clock.py::test_incomplete_anchor_retains_relative_evidence`
cases, also outside this task. The final isolated truth + raw-adapter rerun was
**139 passed** in 0.25s, and `git diff --check` passed.

## Bounded real sample and verification

Input:
`D:/mev_bot-artifacts/north_star/capture/20260909_144906_000490/pumpfun_laserstream_raw_v1_20260909_144906_000490_part0000.ndjson.zst`

Compressed SHA256 (stream-hashed before and after sampling):
`85a02eb32405d8e0ddd96f463ceaea1f52951161298e9f1f027e854e4935543f`

Selection: first ten transaction attempts in the already-exposed first100 raw
prefix; stopped at raw line 23. Record indices:
`[7, 8, 9, 11, 12, 14, 15, 16, 19, 22]`.

- **10 attempted; 9 arithmetic-reconciled; 1 rejected**.
- Rejection: index **19**, `missing_token_counterpart` (post-only token index 3).
- Reconciled: **7 succeeded, 2 failed**, **69 token endpoint rows**, **305 native
  endpoint rows** (includes zeros).
- **All 9 reconciled transactions are canonical-identity quarantined** because of
  the malformed recorder key above. Arithmetic acceptance is not identity or
  training admission; zero canonical transaction admissions are claimed.
- Output SHA256:
  - reconciled.ndjson: `2e4ff8237cce0adf3f52e09c39d10fb2abf23a7c99ddfe99d5bedbda3faaff0b`
  - rejected.ndjson: `ee65a263495ea95f26967dbfca2f9f486b1a4bc0af7b58520d55a084dfa8e8de`

Read back both exact output targets and verified their hashes/counts. An independent
read-only loop over the same raw prefix checked every one of the **69** token
rows against the original index-resolved key, matching pre/post owner and exact
integer subtraction; checked original signatures/raw byte hashes and per-tx native
fee residuals. This is an independent arithmetic implementation in this task,
not an independent human reviewer or chain RPC confirmation.

Concrete record 7, full signature:
`3k99d9ki6pauXRuaemdFCzqpoDyDCLzT3q9BVHxaprKzyiXCkz5yfNGHU54UzBTPJfW9MZek3pLhP5jkMBRNxnnS`

Observed fee: **49071 lamports**. Nonzero native `(account_index, delta_lamports)`:
`[(0,-1644733),(4,398),(5,1594070),(7,796),(9,398)]`.
Token `(account_index, delta_raw)`:
`[(1,-4324453),(2,4324453),(4,398),(5,1594070),(7,796),(9,398)]`.
These are not labeled as buys, sells, independent payments, costs or venue fills;
SOL and wrapped-token endpoint rows must not be summed as independent wealth.

## Independent-review fixes and preserved-sample replay

Read the complete review from `deleg_76e6afa0`, session
`20260909_112656_02d213`, assistant message `152958` in the read-only session DB.
The live log truncates its final JSON; the full report has **two P1 must-fix
findings**, not just failed native transfers. Both are addressed:

1. Failed native endpoints require fee-only index-0 debit and zero other deltas.
   Four conserved negative fixtures cover payer transfer, nonpayer transfer,
   wrong fee payer and payer credit. Before the fix: **4 failed / 50 deselected**,
   each `DID NOT RAISE TransactionTruthError`; after the fix the combined suites
   returned **143 passed**.
2. Known loaded-readonly native/token changes reject. Four negative fixtures
   cover native and token credits/debits, including balanced counterpart changes.
   Before the fix: **4 failed / 2 passed / 54 deselected**; unchanged-readonly
   positive controls passed. No static-key writability inference was added.

Final focused verification (Windows terminal venv Python):

```text
PYTHONPATH=src python -B -m pytest tests/north_star/test_transaction_truth.py tests/north_star/test_raw_adapter.py -q -p no:cacheprovider --tb=short
149 passed in 0.25s
```

The truth suite now has **60 cases**. A broader concurrent-worker snapshot:

```text
PYTHONPATH=src python -B -m pytest tests/north_star -q -p no:cacheprovider --tb=short
1 failed, 1091 passed in 7.56s
```

The remaining failure is outside ownership:
`test_reservation.py::test_checked_in_reservation_is_fixed_prospective_and_not_capture_evidence`
expects reservation pin
`95e14af3a84e34c0fbe77049dcabc991212514617bf35abc0ab4b9cb5f70255f`, but reads
`8d018de9c530b97595d5a3ec9c69d92df497be16c882df12662398b6a8476fb8`.
No reservation file was modified.

Replayed only the same ten original transaction attempts, stopping at line 23,
**in memory without writing a new sample**. All nine accepted output objects
match their preserved outputs exactly; index 19 still rejects for the original
`missing_token_counterpart` reason. Both failed records (12 and 15) are fee-only;
all explicitly loaded-readonly native/token endpoints are unchanged. All nine
accepted rows remain identity-quarantined; malformed literal keys are untouched.

Compressed source SHA256 verified before/after replay against the original pin.
All three existing sample files hashed identically before/after:

- `receipt.json`: `587d13fca51d2286f1b925fa14551c30ae7da6f37f18a3260ca23281104751be`
- `reconciled.ndjson`: `2e4ff8237cce0adf3f52e09c39d10fb2abf23a7c99ddfe99d5bedbda3faaff0b`
- `rejected.ndjson`: `ee65a263495ea95f26967dbfca2f9f486b1a4bc0af7b58520d55a084dfa8e8de`

Only this receipt, `transaction_truth.py` and `test_transaction_truth.py` were
edited for the review fixes. No collectors, original captures, identifiers,
fee assumptions, other workers' files or commits were changed. One fuzzy patch
initially matched the wrong native-output span; the tool's syntax check caught it,
and it was corrected before the successful test runs. Reusable review lesson:
read the full report rather than the truncated live summary, and test endpoint
constraints independently of aggregate conservation.

## Final independent re-review

`deleg_b9d9927d` returned boundedpass/no mustfix for endpoint reconciliation after failed-transaction and readonly guards. Parent independently ran all60 focused tests including temporary-file cases successfully. A new shared `tests/north_star/conftest.py` makes individual test targets independently runnable from the repository root; previously full-suite import side effects masked missing PYTHONPATH. This is test execution plumbing, not data logic. No whole-ledger/venue-fill admission is claimed.

## Remaining work / reusable lessons

Wire this explicitly limited development schema into the canonical pipeline only
after independent review. Source rights/time/finality and provenance admission,
recorder malformed-key remediation with original-byte evidence (no guessed repair),
creation/closure proofs, instruction/version/venue decoding and actual fee/rent
attribution remain open. Do not feed quarantined rows directly into FIFO as fills.
Do not broaden sample selection to obtain nicer acceptance statistics.

For future decoder work: inspect recorder encodings before validation; preserve
malformed identities while quarantining them; test src-layout imports using the
Windows venv terminal Python; keep endpoint arithmetic acceptance separate from
canonical identity admission and economic execution claims.
