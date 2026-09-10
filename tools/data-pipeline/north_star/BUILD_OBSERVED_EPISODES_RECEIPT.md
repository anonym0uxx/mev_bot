# Bounded observed-episode assembly — development receipt

## Outcome and scope

**IMPLEMENTED_AND_TESTED transform; zero source-backed valid episodes. Stage 5 is NOT complete.**

Only `src/north_star/observed_episodes.py`, `tests/north_star/test_observed_episodes.py`, and this receipt were authored for this task. No shared modules, network, capture/raw files, relabeling, fitting, training, commits, or deployment changes. Other changes visible in the shared worktree were left alone.

Read master `NORTH_STAR_WINDOWS_MASTER_V5.md` stages 2/5 (lines 395/401), causal/source/parent requirements, and existing `accounting.py`, `actions.py`, `transaction_truth.py`, plus instruction-truth limitations. Arithmetic-only endpoints and instruction arguments cannot establish executed trades. This code adds no decision policy; Qwen's trader role is unchanged.

## API and contract

`assemble_observed_episodes(actions, *, cutoff_unix_ms, observation_end="open", max_actions=1000)` is an atomic pure transform. It raises on invalid input rather than silently sorting, dropping a gap, coercing numbers, or constructing partial output. Maximum configurable action count is 10,000; refs and parent lists are bounded at 128, identifiers at 2,048 characters. Empty input returns an empty list.

- Accepts only `proven_executed_venue_action_v1`, explicit observed origin, strict `recommended is False`, `executed_venue_fill is True`, verified identity/layout, confirmed transaction and finalized execution. Supports Solana pumpfun/pumpswap BUY/ADD/REDUCE/EXIT_ALL only.
- Exact canonical base58 wallet/mint/signature validation without repair. Strict u64 integers reject booleans, floats, strings, negative values, unknown inventory and overflow; decimals additionally capped at 255. Units are raw token integers and lamports.
- Requires action-bound hashed evidence references for causal context, execution, identity, layout, opening inventory, endpoint inventory, fees and ordering. Causal and opening-inventory evidence must be available by the decision cutoff. Other evidence must be available by the action's availability; action availability must be within the assembly cutoff. Arbitrary rationale fields are rejected, including nested evidence fields.
- **Proof references are an input contract, not an independent source verifier.** The caller must supply independently verified execution/layout, complete inventory and fee-allocation evidence. Passing software fixtures does not prove any real venue fill or confer source rights, canonical admission, or training eligibility.
- Strict per-wallet `(slot, transaction_index, instruction_index)` order; event times cannot move backward. Distinct wallets are not pooled. Duplicate action IDs and fee-allocation scope IDs reject. Mint decimals and wallet/mint inventory continuity must agree.
- Flat→position→flat boundaries: partial reductions and adds remain within one episode; post-flat reentry starts another. Known positive opening inventory is left-censored, not an invented BUY. Unknown opening inventory rejects. Unclosed positions remain open, or explicitly right-censored with `observation_end="censored"`; remaining inventory is retained.
- Deterministic episode ID and action/event/evidence IDs retained; parent IDs are the union of every input action and evidence parent for later split inheritance. Inputs are deep-copied. No split assignment, recommendation targets or rationale generation occurs.
- Cash flow follows existing accounting semantics: consideration already includes embedded venue fees; only independently allocated additional nonrecoverable fees are subtracted separately. Signed cash flow is not labeled PnL, and no opening cost basis or unknown fee is invented. Recoverable rent/transfers, fee-allocation derivation, FIFO PnL, relative-only sessions, multi-fill intra-instruction ordering, coverage certification and semantic adjudication remain outside this bounded contract.
- All episodes remain `training_eligible=false`, `imitation_eligible=false`.

## Strict TDD execution evidence

Behavioral batches were written and run red before implementation:

1. Missing assembler: 1 failed → 1 passed.
2. Partial exits/add/flat/reentry: 1 failed, 1 passed → 2 passed.
3. Proven-execution/identity/scope gate: 17 failed, 2 passed → 19 passed.
4. Units/fees/evidence/cutoff validation: 103 failed, 19 passed → 122 passed.
5. Chronology/duplicates/discontinuity/censoring/bounds: 16 failed, 123 passed → 139 passed.
6. Immutable first-ten report audit: 5 failed, 139 passed → 144 passed.
7. Episode identity/ancestry/nested evidence/decision inventory proof: 17 failed, 130 passed → 147 passed.

Final real executions:

```text
PYTHONPATH=src python -m pytest tests/north_star/test_observed_episodes.py -q
147 passed in 0.15s

PYTHONPATH=src python -m pytest tests/north_star/test_observed_episodes.py tests/north_star/test_accounting.py tests/north_star/test_actions.py tests/north_star/test_transaction_truth.py -q
284 passed in 0.30s

PYTHONPATH=src python -m pytest tests/north_star -q
2199 passed in 14.02s
```

`git diff --check` exited 0 (unrelated shared tracker CRLF warning). Fixture rows are synthetic software tests only, not manufactured training data. Unit tests use temporary fixture reports, not real data or network access.

Code SHA256: `7b7b678d235ad73503ac34401d1a0990dd3734252cb72e4f4135a294f3f204ec`

Test SHA256: `7c8bd16cfb7e7295125899587272464a5042b8128d52c6ea8ad8400210021e29`

## Real immutable-report attempt

Ran `audit_transaction_truth_report` on **only** the existing three files in:

`D:/mev_bot-artifacts/north_star/development/transaction_truth/`

The adapter never opens raw capture paths referenced inside those files. It checks file hashes, exact count types, selection indices, schemas and a strict first-ten bound before writing output. It calls the assembler on every reconciled record without modifying or upgrading any input identity/status.

Measured result:

- 10 original transaction attempts accounted for.
- All 9 identity-quarantined reconciliations refused episode construction with `identity_quarantined`.
- Original 1 `missing_token_counterpart` rejection retained, including a byte-identical copy of upstream `rejected.ndjson`.
- **0 valid episodes**, empty `episodes.ndjson` (zero bytes); no padding.
- All nine literal malformed 33-ones identities retained unchanged in complete original records.
- Source hashes unchanged before/after publication; all output hashes independently read back and verified.

New external artifact directory:

`D:/mev_bot-artifacts/north_star/development/observed_episodes/first10_rejection_audit_v1/`

| File | SHA256 |
|---|---|
| `rejection_audit.ndjson` | `5b6c8c0378a4789035de28cdad4d68d14e728bd97a81d7167677e272e21d8049` |
| `upstream_rejected.ndjson` | `ee65a263495ea95f26967dbfca2f9f486b1a4bc0af7b58520d55a084dfa8e8de` |
| `episodes.ndjson` | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `receipt.json` | `208f5f1fb4bba252c972d862ce29ada4d04f77811ba6895b9f0b07789fa990c4` |

Source hashes:

- `receipt.json`: `587d13fca51d2286f1b925fa14551c30ae7da6f37f18a3260ca23281104751be`
- `reconciled.ndjson`: `2e4ff8237cce0adf3f52e09c39d10fb2abf23a7c99ddfe99d5bedbda3faaff0b`
- `rejected.ndjson`: `ee65a263495ea95f26967dbfca2f9f486b1a4bc0af7b58520d55a084dfa8e8de`

Publication uses an absent output directory, exclusive file creation, fsync, readback, hash manifest, and receipt-last completion marker. All four Windows READONLY attributes were independently verified. Rerunning into an existing directory raises `FileExistsError` (fixture-tested). This is application-level immutable publication with read-only files, **not** OS/WORM protection against a privileged writer. A failed incomplete publication must be kept and a new path chosen, never overwritten.

Remaining source blocker: no eligible independently proven, canonically identified executed venue actions in this first-ten report. Source verification, real episode coverage, whole-session reconstruction, admission, protected split inheritance and Stage 5 completion remain unclaimed.
