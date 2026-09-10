# Exact instruction-layout tracer — bounded development receipt

## Outcome and scope

Implemented `src/north_star/instruction_truth.py` and
`tests/north_star/test_instruction_truth.py`. This receipt is the third owned repo
file. Nothing was committed, deployed, collected, fetched from the network, fitted,
promoted, labeled as a trade, or admitted to training. Other workers' files were
not modified. Worktree inspected on `task/north-star-build`, HEAD
`96fb932794381d8eb4781c9887fa6420fa1abc2c`; local main unpushed count was zero.
Concurrent untracked narrative/HLS files were already present.

New artifact directory:
`D:/mev_bot-artifacts/north_star/development/instruction_truth_first10_v1/`

- `traced.ndjson`: all ten transactions, each with every outer and recorded inner instruction.
- `rejected.ndjson`: transaction-structure rejections (empty in this sample).
- `receipt.json`: selection, exact counts, source/transform/output SHA256.
- `verify.py`: independently implemented read-only checks against original raw bytes.
- `reference_hashes.json`: exact local official Rust reference paths and SHA256.

This is **layout classification**, not execution economics. `accepted` means an
exact supported binary/account layout only. Traced transactions may contain
rejected instructions and quarantined identities. It is not canonical transaction
acceptance. `executed_transfer_raw`, net received amount, owner, beneficial person,
trade, CPI caller identity and finality are never inferred. All output is
`training_eligible=false`, `source_admission=unadmitted`.

## Prerequisite inspection and reference evidence

First inspected `raw_adapter.py`, `transaction_truth.py` and
`BUILD_TRANSACTION_TRUTH_RECEIPT.md`, including the actual already-exposed source
path/hash. Read master V5 section 4.2: integer units, transfer != trade, separate
failed attempts, and no same-person attribution. Inspected recorder Rust
`tools/stream-capture-rs/grpc-server-only/src/raw_recorder.rs:176–184,223–240`:
`program_id_index` is an integer; accounts/data are base64 bytes; inner groups
contain outer `index`, instructions and nullable `stack_height`. Key order is
static + loaded writable + loaded readonly, not readonly before writable.

Searched both the worktree and original `D:/repos/mev_bot` for reference instruction
enums/layouts first; none were found there. Used **already-installed official
Rust Cargo sources**, without network or package installation, under:
`C:/Users/Alon/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.
Exact paths and full-file hashes are in `reference_hashes.json`.

Verified layouts before implementation:

| Program / instruction | Binary | Account positions | Local Rust evidence |
|---|---|---|---|
| System Transfer | LE u32 enum tag 2 + LE u64 lamports; exactly 12 bytes | 0 funding, 1 recipient | `solana-system-interface-2.0.0/src/instruction.rs:82–113,817–822` |
| SPL Token Transfer | u8 tag 3 + LE u64 amount; exactly 9 bytes | 0 source, 1 destination, 2 owner/delegate authority | `spl-token-interface-2.0.0/src/instruction.rs:93–107,600–603,964–983` |
| SPL Token TransferChecked | u8 tag 12 + LE u64 amount + u8 decimals; exactly 10 bytes | 0 source, 1 mint, 2 destination, 3 owner/delegate authority | same file `256–272,628–631,1233–1255` |
| Token-2022 Transfer / TransferChecked | same respective base encodings | same base positions | `spl-token-2022-interface-2.1.0/src/instruction.rs:134–148,322–338,901–904,929–932,1401–1421,1671–1693` |

SPL golden amount=1 vectors are printed in official tests at legacy lines
1485–1489 / 1553–1559 and 2022 lines 2127–2135 / 2229–2237. System tag is the
third serde enum variant, not guessed. Followed `new_with_bincode` through
`solana-instruction-2.3.3/src/lib.rs` to `bincode::serialize`; bincode 1.3.3
`src/lib.rs:106–113` uses fixint, `config/mod.rs:98–102` uses LittleEndian,
`config/int.rs:119–125,358–367` uses direct fixed integers and u32 enum indices,
and `ser/mod.rs:188–196` writes that enum index before fields.

Literal program IDs verified in each interface's `src/lib.rs`:
System `11111111111111111111111111111111`, SPL
`TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`, 2022
`TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`.

## Fail-closed contract

- Exact canonical base64, no whitespace/alternate pad bits or JSON-array account
  coercion. Program index must be an actual int in the u8/full-key domain; bool,
  float, negative, string and out-of-range indices reject.
- Referenced program/account identities must decode to exactly 32 bytes; full
  signatures to 64 bytes. Duplicate key domains reject. Unreferenced malformed
  keys quarantine transaction identity but do not manufacture an alias.
- Historical **33 literal ones remain unchanged** and are rejected when referenced.
  No encoder workaround, 32-one substitution, RPC lookup or canonical admission.
- Exact supported data lengths; missing base accounts reject; extra accounts,
  unknown programs/opcodes, Token-2022 extensions and multisig semantics remain
  unknown. This subset is intentionally narrower than permissive Rust unpacking.
- Amount is exact u64 integer instruction argument. Checked decimals are an
  argument, not independently fetched mint metadata. Owner/delegate authority is
  not proven token ownership or a beneficial person.
- Success/error fields must agree. Failed transactions retain decodable arguments
  but have `transaction_failed_no_committed_transfer`. Successful outer instructions
  get only `outer_instruction_succeeded`. **Even in successful transactions, inner
  outcomes remain `unknown_cpi_outcome`**: caught CPI failure is possible. Stack
  height does not establish caller identity. No executed transfer quantities.
- Invalid instruction is preserved as rejected beside other instructions. Invalid
  transaction structure/status rejects the transaction. Duplicate or invalid inner
  parent mappings reject rather than overwrite/attach to a guessed parent.
- Missing balances do not imply transfers or prevent binary classification. This
  module does not call endpoint reconciliation: the prior sample's index 19 remains
  unsuitable for balance reconciliation but its recorded instructions are traceable.

## Strict TDD and execution

All initial feature/sampler tests preceded the implementation. RED command:

```text
python -B -m pytest tests/north_star/test_instruction_truth.py -q -p no:cacheprovider --tb=line
55 failed in 0.27s
ModuleNotFoundError: No module named 'north_star.instruction_truth'
```

Implemented only after observing RED. First combined GREEN:

```text
python -B -m pytest tests/north_star/test_instruction_truth.py tests/north_star/test_transaction_truth.py tests/north_star/test_raw_adapter.py -q -p no:cacheprovider --tb=short
204 passed in 0.30s
```

Added additional guard characterization tests (no production changes): bad CPI
heights, duplicate groups/key domains, u64-max failed System transfer, both loaded
key groups, and output/source separation. Final combined GREEN:

```text
215 passed in 0.31s
```

Focused new suite contains 66 cases. Exact lengths, u64 maximum/zero, program
allowlist, 33-ones nonrepair, base64/index types, unknown semantics, CPI outcomes,
failed status, raw hashes, first10/first100 bounds, new-only outputs and compressed
hash mismatch are covered. `git diff --check` passed. Static scan found no eval,
exec, shell execution, pickle, subprocess, socket or HTTP code.

Broad concurrent-worker snapshot returned **6 failed, 1533 passed in 8.50s**:
`test_development_pipeline.py` (integration module absent at that instant) and five
`test_event_revisions.py` identity tests. These are outside owned files and were
not altered. This is a concurrent snapshot, not a claimed baseline/regression
comparison. No independent agent reviewer was available to this subagent; parent
independent review remains required. The artifact verification is a separate
implementation, not a separate reviewer or chain confirmation.

## Real bounded run and verified counts

Immutable original:
`D:/mev_bot-artifacts/north_star/capture/20260909_144906_000490/pumpfun_laserstream_raw_v1_20260909_144906_000490_part0000.ndjson.zst`

Source SHA256 before/after:
`85a02eb32405d8e0ddd96f463ceaea1f52951161298e9f1f027e854e4935543f`.
Only first ten transaction attempts inside the already-exposed first100 raw lines;
stopped at line **23**, selected indices `[7,8,9,11,12,14,15,16,19,22]`.
Full compressed hashing is not full decompression or source admission.

- Transactions: **10 traced, 0 structurally rejected**; **8 succeeded, 2 failed**.
- Every transaction identity quarantined: **10** (historical malformed keys).
- Instructions: **161 = 38 accepted layouts + 43 rejected + 80 unknown**.
- Accepted: **38 TransferChecked**, all inner instructions: **32 SPL Token,
  6 Token-2022**. All 38 retain unknown CPI outcomes, no executed quantity.
- Rejected: **17 invalid_program_id**, **26 invalid_instruction_account_key**.
- Unknown: **38 unsupported_program**, **42 unsupported_opcode**.
- Context outcomes across all layouts: **61 outer_instruction_succeeded**,
  **90 unknown_cpi_outcome**, **10 transaction_failed_no_committed_transfer**.
- System transfers accepted: **0**. Legacy malformed System ID is not repaired
  to obtain a positive sample. System/unchecked token positives are synthetic
  reference-layout engineering tests only, not real sample execution claims.

Read back exact outputs and ran:
`python -B D:/mev_bot-artifacts/north_star/development/instruction_truth_first10_v1/verify.py`
It returned `verified=true`, 10 transactions / 161 instructions / accepted38,
rejected43, unknown80, source unchanged. Verifier checks every original instruction,
parent position, raw line hash, key ordering, classification reason and all 38
amount/decimal/account role mappings using independent struct unpacking.

Exact hashes:

| File | SHA256 |
|---|---|
| instruction_truth.py | `4722fbf51acf79a899bf051d447b4328c0bc22af9a3b98ac096701761b84e535` |
| raw_adapter.py | `6f11413dc4be53bac1e776485c9b1800879a2d10bc0d6d52e43bba01c14f3ec6` |
| test_instruction_truth.py | `80ef14730276e38931966da533bce9f45859e367050e7aebbcff0af352d6edfa` |
| traced.ndjson | `9176bee0173f8844991884481b96f112368f31011d8bbdf481cd9aa381b9aecb` |
| rejected.ndjson | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| receipt.json | `d9efae07d23d01d3e200902378ef1b9f3f9e8bb54c6a23c9bb70a66a37d2e887` |
| verify.py | `244892d62f0bf6986cdc0038979a52e5b2489e694231dbef13761476539c1393` |

Remaining: independent review, explicit invocation outcome evidence, runtime
privilege checks, extensions/multisig, actual net flow/fee joins, finality and
identity/source admission. Do not convert these argument rows into fills or FIFO.
