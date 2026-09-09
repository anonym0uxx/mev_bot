# Stage 2 exact accounting foundation — execution receipt

Status: **implemented and synthetic-test verified; Stage 2 NOT CLOSED**.

## Scope and authority

Read master `master/NORTH_STAR_WINDOWS_MASTER_V5.md` §4.2 (lines 598–612),
D12, G08/G09, and Stage 2; read `BUILD_PLAN.md` S2.3 and
`00_HOLISTIC_CONTEXT.md`. This task implements a deterministic Python reference
library only. Synthetic fixtures are engineering evidence, not source truth,
human rationale, training examples, or admission evidence. No strategy parameters,
legacy-output repairs, collectors, production integration, fund movement, commits,
or training were performed.

Owned files, relative to `tools/data-pipeline/`:
- `src/north_star/accounting.py`
- `tests/north_star/test_accounting.py`
- `north_star/BUILD_ACCOUNTING_RECEIPT.md`

Other agents' concurrent untracked files were observed and left untouched.

## Implemented contract

- `Action`, `Transaction`, `Lot`, `Sale`, `Receipt`, and `Ledger` provide immutable
  input/output records and atomic in-memory posting. `allocate_lamports` apportions
  integer totals using cumulative floor; allocations telescope exactly to the total.
- Source lamports/raw token quantities are exact u64 integers (bool/float/string
  coercion is rejected); aggregates use arbitrary-precision Python integers. Token
  decimals must be an explicit u8 and cannot conflict for a mint within a ledger.
- FIFO uses original acquisition order and original quantity intervals. Partial
  exits and split/returned transfers retain those intervals: no repeated-rounding
  cost loss, no re-aged transfer lots, no inventory duplication. Multiple buys,
  adds, complete exits, zero-proceeds losses, residual inventory and re-entry work.
- BUY/SELL lamports mean observed consideration with embedded venue costs already
  reflected. Additional, nonoverlapping, nonrecoverable transaction fees/tips are
  supplied explicitly and expensed **once per transaction**, not capitalized into
  lot basis. Per-sale PnL excludes that separately visible transaction expense;
  whole-ledger realized PnL includes it. This reference decomposition is not a tax
  convention or approval of a downstream policy. No extra slippage is invented.
- `TRANSFER` requires a same-owner evidence reference and a different destination;
  source/destination wallet inventory remains separate. `TRANSFER_IN` and
  `TRANSFER_OUT` represent external inventory flows, not trading cash or revenue.
  Transferred basis is returned separately, including explicit unknown basis.
  Transfer fees, if supplied, remain expenses rather than transfer income.
- OPEN must precede any wallet/mint activity, even an earlier completed position.
  Unknown OPEN/TRANSFER_IN basis requires `None` plus an explicit reason. Confirmed
  subsequent sales retain quantities and proceeds but report unknown basis/PnL;
  aggregate realized PnL remains `None` thereafter. Missing-history oversells are
  rejected, never converted to free inventory. Known zero cost is distinct from
  unknown cost.
- Confirmed failed transactions must contain no actions; their explicit fee is
  booked without fabricating a fill or changing inventory. Pending/timeout/reverted
  statuses are rejected rather than treated as fills or zero-fee failures.
- Exact transaction identities are rejected on repeat, including conflicting
  contents. Action identities are unique within each transaction; identity is
  `(tx_id, action_id)`, so the same local action ID in different transactions is
  allowed. No whitespace normalization repairs identifiers.
- Caller-supplied sequence must be strictly increasing. Rejected transactions do
  not consume sequence/identity or publish partial lots, decimals, fees, or cash.
  Snapshots/receipts cannot mutate active state.

## Actual red → green execution

Working directory: `D:/repos/mev_bot-north-star/tools/data-pipeline`.
Command throughout:

```text
python -m pytest tests/north_star/test_accounting.py -q -p no:cacheprovider
```

`--tb=short` was added to some failure runs; results below are actual tool output.

1. Allocation test written before module existed:
   `1 failed in 0.04s` — `AssertionError: exact accounting module not implemented`.
   Implemented allocator: `1 passed in 0.01s`.
2. Invalid allocation-input tests written before validation:
   `8 failed, 1 passed in 0.06s` — missing rejection, TypeError and zero division.
   Added integer/nonempty/positive-weight validation: `9 passed in 0.01s`.
3. FIFO, transfer, unknown-basis, failure, identity, rollback and action-boundary
   tests written before the ledger:
   `25 failed, 9 passed in 0.14s` — missing `Ledger` API.
   Implemented the tested ledger contract: `34 passed in 0.03s`.
4. Added supplementary adversarial/characterization coverage of already-implemented
   boundaries (not claimed as new red cycles), u64 aggregate overflow, zero proceeds,
   cross-mint single fee, unknown transfers, immutable snapshots, literal IDs and
   seeded transfer/sale conservation paths: **`58 passed in 0.08s`**.

The conservation test exercises 40 seeded synthetic paths with exact assertions
at every step: disposed basis + active basis = original basis; disposed quantity
+ active quantity = original quantity; sale allocations = actual consideration;
at flat, cash delta = realized PnL for buy-funded inventory with no external flows.

Existing foundation regression (concurrent sibling work excluded explicitly):

```text
python -m pytest tests/north_star -q -p no:cacheprovider --ignore=tests/north_star/test_accounting.py --ignore=tests/north_star/test_actions.py --ignore=tests/north_star/test_dependencies.py --ignore=tests/north_star/test_jobs.py --ignore=tests/north_star/test_raw_adapter.py
491 passed in 1.52s
```

Final combined regression, with only the four concurrent sibling test modules
excluded (the command above without `--ignore=tests/north_star/test_accounting.py`):
**`549 passed in 1.65s`**. The three owned files remain untracked, with no commit.

AST parsing passed for module/tests; module contains zero float constants and its
only import is `dataclasses`. No network, filesystem, process or execution actions
exist in the library. `git diff --check` returned exit 0. Ruff was not installed.

## Explicit limitations / handoff gates

- No independently hand-reconciled **real** chain receipts: source-backed economics,
  rights/time/finality/decoder/venue semantics and corpus admission remain unverified.
  No independent reviewer was run in this subagent; parent review remains required.
- Caller must establish canonical chain/mint/account identities, sequence, ownership
  evidence validity/effective intervals, exact fills, fee completeness and absence
  of embedded/separate overlap. A reference string alone does not prove ownership.
  One instance has one chain/accounting scope; there is no adapter or cross-chain
  consolidation. Asset decimals changes are rejected pending explicit adapter work.
- No native cash deposits/withdrawals, detailed fee categories/payers, recoverable
  rent/refunds, fee rebates/nontrading income, Token-2022 withheld/burn semantics,
  migration/venue adapters, pending reservations, conservative liquidation values,
  flat-to-flat episode construction, or censoring ledger. Do not force these events
  into BUY/SELL or permanent transaction-fee expense.
- Unknown fees cannot be represented as verified economics; caller must quarantine
  them. External transfer-in basis is caller-supplied and FIFO-aged at observed
  receipt, not reconstructed to an unobserved historical acquisition. Internal
  transfers preserve original acquisition identity/order but this is not a full
  transfer-lineage evidence graph.
- This tracks aggregate observed cash **delta**, not spendable balances, capital
  adequacy, per-wallet native cash reconciliation or portfolio equity. Realized PnL
  alone must not be presented as whole-session profitability with open inventory.
- Active lots, seen identities and account/mint history remain in memory. Posting
  copies active lot lists for atomicity; no scale benchmark, durable checkpoint,
  restart dedup, concurrent mutation safety, reorg correction or Rust differential
  conformance is claimed. Callers must persist returned receipts if required.
