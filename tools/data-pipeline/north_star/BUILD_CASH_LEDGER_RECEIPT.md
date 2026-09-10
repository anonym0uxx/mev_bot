# Exact native-cash reference ledger — engineering foundation only

## Outcome and release boundary

Implemented `src/north_star/cash_ledger.py` independently of the existing FIFO/consideration ledger. Added synthetic-only `tests/north_star/test_cash_ledger.py`. This receipt is the third owned file. No sibling implementation edits, commits, RPC, new source data, risk-policy numbers, fits, admission or Rust/live-exit changes were made.

**Not Stage 2 closure and not an actual economic claim.** Source-backed independently checked native endpoints, correct transfer/ownership classification, revision/finality integration and economic certification remain pending. Every wallet snapshot and transaction receipt permanently exposes `economics_eligible=False` and `training_eligible=False`, including caller-labeled `finalized` transactions. Mandatory evidence strings are preserved locators, not verified proof.

Prerequisites inspected:
- `src/north_star/accounting.py`: its `cash_delta_lamports` is cumulative trade consideration less separately charged costs, not actual wallet cash/bankroll; FIFO/PnL behavior was left untouched.
- Master `NORTH_STAR_WINDOWS_MASTER_V5.md`, Stage 2 (line 395), D10 (274–277), D12 (285–289), plus D13 test-only fixture limits.
- `north_star/BUILD_PLAN.md` S2.1–S2.3 (245–260): explicit identity/units, fees/rent, reconciliation and real-receipt gate.

## Implemented contract

- `CashLedger.open_wallet(wallet, lamports, evidence=..., unknown_reason=...)` records a known native checkpoint or an explicitly unknown opening. No implicit zero, budget or synthetic bankroll. Opening is the checkpoint immediately preceding that wallet's first transaction in this caller-defined accounting scope, and cannot reset an existing wallet.
- `CashTransaction` holds unique `tx_id`, strictly increasing exact integer `sequence`, caller-labeled status, a tuple of frozen `Movement` records, mandatory `TransactionFee`, and a tuple of frozen witnessed post-transaction `CashEndpoint` records. Tuple structure is immutable; frozen records are not mutation-proof. Local action identity is `(tx_id, action_id)`; duplicate transactions or same-transaction actions reject instead of silently replaying costs.
- Explicit movements: `EXTERNAL_DEPOSIT`, `EXTERNAL_WITHDRAWAL`, `TRADE_DEBIT`, `TRADE_CREDIT`, `RENT_DEPOSIT`, `RENT_REFUND`, `NONTRADING_INCOME`, `UNKNOWN_DEBIT`, `UNKNOWN_CREDIT`. No category is inferred from a wallet delta. Unknown movement needs a reason.
- Inputs and derived cash endpoints are u64 **lamports**, with exact `type(value) is int`; bool, float, text, negative, missing and overflow reject. Signed cumulative totals use Python integers. No SOL conversion, token/WSOL inference or model slippage.
- Mandatory fee amount/payer/evidence even for zero. `overlap="nonoverlap"` requires no embedded action and debits the payer separately. `overlap="overlap"` names exactly one same-payer trade action containing the entire fee: buy debit includes it; sell credit is net of it. The cash movement is applied once, the fee is expensed once, and only that transaction fee is removed from trade consideration. Partial, multiple, absent and unknown overlap declarations fail closed. Embedded venue costs remain part of trade consideration.
- `failed` transactions admit no actions and only the fee payer endpoint: known prior cash minus witnessed fee must exactly equal observed post-cash. Unknown prior cash or unexplained failed-tx differences reject, not invented failed-trade effects.
- Recoverable rent is tracked by `(wallet, rent_id)` as a recoverable asset movement, never fee expense, trade PnL or nontrading income. Refund cannot exceed prior same-wallet witnessed deposits; partial refunds supported. Unwitnessed opening rent/cross-wallet refund ownership is not guessed.
- Successful valid endpoint differences enter suspense, not inferred income/deposit/trade profit. Observed endpoints remain actual supplied cash. Unknown opening stays unknown even after a later known endpoint; it is never backsolved. Offsetting suspense amounts do not erase unresolved reasons.
- Detached snapshots and receipts preserve retained input witnesses and expected/observed/residual comparisons through defensive record copies, not through `frozen=True`. Malformed transaction rejection is atomic across wallet cash, category totals, rent, receipts, identity and sequence. A corrected transaction may reuse rejected identity/sequence.

For known opening, the tested native-cash identity is:

`observed cash = opening + external flow + trade consideration - fee expense + nontrading income - outstanding recoverable rent + suspense`

This is **not** realized PnL. Synthetic flat-cash test ends at its opening cash with nonzero external flow and offsetting trade consideration; no realized-PnL field is provided.

## Real verification output

TDD was exercised in successive RED/GREEN batches:
- Missing module: 1 failed, then 1 passed.
- Opening validation/unknown cash: 14 failed / 1 passed, then 15 passed.
- Movement posting: 1 failed / 15 passed, then 16 passed.
- Suspense/unknown history: 4 failed / 16 passed, then 20 passed.
- Embedded fees/rent: 3 failed / 22 passed, then 25 passed.
- Strict transaction/fee/identity/rent/atomic validation: 62 failed / 25 passed, then 87 passed.
- Additional invariant, multiwallet rollback, malformed-record and literal/finalized regressions; refactoring preserved passing behavior.

Original pre-review commands on native Windows Python (historical; these did not test CL-1 aliasing and do not establish state isolation):

- `python -m pytest tests/north_star/test_cash_ledger.py -q` → **94 passed in 0.06s**.
- `python -m pytest tests/north_star/test_cash_ledger.py tests/north_star/test_accounting.py -q --tb=short` → **152 passed in 0.14s**.
- `python -m pytest tests/north_star -q --tb=short` → **1861 passed in 11.65s** (includes concurrent sibling tests present at execution; not a claim to own or independently review those files).
- `git diff --check` → exit 0; unrelated `HELIUS_BUDGET_EVIDENCE.md` emitted an LF/CRLF warning.
- Owned Python files separately AST-parsed; zero trailing-whitespace lines and no direct `eval`/`exec`/`system` calls. Ruff was not installed.

## CL-1 correction and verified RED/GREEN

**Withdrawn:** the previous immutable-state claim based on frozen dataclasses. Independent review correctly found that `wallet("W").__dict__["cash_lamports"] = 1000` (or `object.__setattr__`) changed retained opening-100 state and allowed a fee-zero/action-empty endpoint-1000 transaction to falsely reconcile. Original caller transactions and returned receipts also aliased retained history, including nested fees, movements and endpoints. Ordinary-assignment `FrozenInstanceError` tests did not prove isolation.

The correction copies each validated transaction, movement, fee and endpoint on ingress. `wallet()` returns a fresh snapshot; `apply()` and every `receipts` read return fresh receipt/transaction/reconciliation record graphs. Exact validated immutable primitives and tuples of primitive reasons may be shared safely. The apply return copy is prepared before publishing state. No slots-only workaround, generic object traversal or sandbox machinery was added.

Native Windows Python verification, in successive strict test-before-fix batches:
- Wallet exploit RED: `python -m pytest tests/north_star/test_cash_ledger.py -k wallet_result_mutation -q --tb=short` → **2 failed, 94 deselected in 0.09s**, both because `receipt.reconciled` was incorrectly `True`, expected cash 1000 rather than 100. Wallet snapshot copy GREEN → **96 passed in 0.07s**.
- Original-input graph RED: `python -m pytest tests/north_star/test_cash_ledger.py -k transaction_graph_mutation -q --tb=line` → **24 failed, 96 deselected in 0.09s**, retained history changed. Ingress record copy GREEN → **120 passed in 0.09s**.
- Apply-return/receipts graph RED: `python -m pytest tests/north_star/test_cash_ledger.py -k 'transaction_graph_mutation or receipt_graph_mutation' -q --tb=no` → **72 failed, 24 passed, 96 deselected in 0.17s**. Egress record copies GREEN below.
- Final `python -m pytest tests/north_star/test_cash_ledger.py -q --tb=short` → **192 passed in 0.16s**.
- Final `python -m pytest tests/north_star/test_cash_ledger.py tests/north_star/test_accounting.py -q --tb=short` → **250 passed in 0.21s**.
- Owned Python AST parsing passed; zero trailing-whitespace lines. Scoped `git diff --check` exited 0, but these owned files are untracked, so that command does not inspect their contents; the direct AST/whitespace checks do.

Mutation regressions exercise both `__dict__` and `object.__setattr__` across original transaction inputs, wallet outputs, apply outputs and repeated receipts reads, including transaction replacement, fee/action/endpoint amounts and evidence, receipt flags and reconciliation fields. Subsequent valid cash conservation and unchanged history are checked. Late rent-refund rejection still rolls back rent, wallet totals and history and permits reuse of rejected identity/sequence; original multiwallet and accounting regressions also pass. The original exploit now retains expected cash 100, suspense 900 and unresolved history rather than false reconciliation.

SHA-256 of the corrected Python artifacts:
- `src/north_star/cash_ledger.py`: `8efc41acb861a4244541d427fcd05502201d236bbca4e54bee2eff6a47f2a2ed`
- `tests/north_star/test_cash_ledger.py`: `3136945498886cf9ef8d15c38477bf54136f0a628cfc5828188b89aac9566c0f`

Only the three owned files were edited for CL-1. `accounting.py` was read-only. No commits, network calls, real-data collection, trades or live-tree operations were performed. **Please independently re-review CL-1 against these hashes before accepting closure.** Mechanical synthetic success remains neither economic certification nor Stage 2 closure.

## Remaining limits

Isolation is a single-threaded trusted-caller API contract, not a hostile-Python security boundary. A caller may still bypass frozen guards to corrupt its own detached copy. Direct private-attribute access, class/module monkeypatching and mutation concurrent with validation/copying are out of scope. Record copying must be revisited if mutable leaf types or new nested fields are introduced. Reading all receipts copies the retained history, so time and allocation scale with that history; this remains a small reference ledger, not an unbounded production service.

Pure small in-memory, single-thread reference: no persistence, revision rollback, automatic finality resolution, counterparty/ownership verification, transfer-pair inference, wrapped-native decoding, reservations/spend authorization, inventory valuation, actual PnL or training exporter. Transaction sequence orders observations, not proof of complete on-chain coverage. Per-action interim liquidity is not a spend simulator. Caller-selected evidence/category accuracy is unverified. Reclassification/reorg must replay a corrected ordered stream into a fresh instance. Independent parent review/checkpoint and source-backed ledger acceptance remain outstanding.
