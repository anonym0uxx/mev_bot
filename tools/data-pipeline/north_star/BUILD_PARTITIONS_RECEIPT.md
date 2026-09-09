# Stage 2 bounded development envelope Parquet receipt

## Release state

Implemented and exercised **development-only envelope storage**, not semantic canonical events and not full Stage 2 closure. Every row and the Arrow metadata retain `source_admission=unadmitted`, `training_eligible=false`, and `projection_scope=envelope_only_development`. The sample remains DEVELOPMENT/EXPOSED. No fits, admission, monetary decoding, collector changes, whole-corpus rebuild, or commits.

Owned repository changes only:
- `src/north_star/partitions.py`
- `tests/north_star/test_partitions.py`
- `north_star/BUILD_PARTITIONS_RECEIPT.md`

Read prerequisites: `raw_adapter.py`, `io.py`, `fs_integrity.py`, master Stage 2 and resource/restart discipline. Existing shared modules were not changed.

## API and bounds

```python
from north_star.partitions import write_partition
receipt = write_partition(events_path, output_path,
                          batch_rows=32, max_rows=100,
                          max_line_bytes=1048576, compression="zstd")
```

One explicitly supplied completed `events.jsonl` plus its adjacent successful `report.json` produces one partition; there is no directory/corpus scanner. Hard development ceilings are 100 input rows and 100 rows/batch, 8 MiB/line. Defaults are 32 rows/batch and 1 MiB/line. Reports/receipts are bounded at 1 MiB. Oversized input files are rejected before hashing. Processing and Parquet readback are batched; no pandas/whole-frame loader is used. One worker was exercised.

The 32-column schema preserves uint64 slots, recorder receive milliseconds, record/revision/line indices and account/transaction indices; signed int64 block times; nullable strings/lists and a `null_reasons` map. Float/string/bool coercion into integers is rejected. Literal digest case, source paths, signatures, duplicate key positions, empty lists versus null lists, and whitespace-bearing keys survive without normalization. Unknown or missing columns fail closed.

Receipt provenance pins exact events/report SHA256, source path/digest asserted by the adapter report, configuration plus its SHA256, schema plus its SHA256, writer file SHA256, PyArrow version, and canonical all-column content SHA256. Resume recomputes and validates input, receipt, output SHA/bytes, schema/metadata, row count, row-group bounds and all-column content digest; it never skips merely because a file exists.

## Publication and recovery

A unique same-directory `.pending` Parquet is closed and fsynced, then its schema/rows/content/SHA are validated. A hard link publishes without replacing any existing destination. A separately published no-clobber receipt is the commit marker, followed by readback verification. Observed input/report changes and active/unfinished markers fail closed. An adapter report must reconcile accepted/rejected rows and have consistent bounded-stop flags; `eof_observed=false` with `limit_reached=true` is a completed prefix, not an unfinished projection.

Data and receipt are **not one atomic transaction**. Receipt-publication interruption leaves an uncommitted data orphan; resume refuses it and requires manual recovery/new target. Receipt-only orphans are also refused. A stale unique `.pending` file is neither accepted as committed nor deleted; a new temp may complete independently. Concurrent no-clobber publication was tested on Windows.

The `fs_integrity` trusted-directory policy applies: symlinks/reparse/nonregular paths are rejected, but arbitrary hostile concurrent ancestor/hard-link mutation and undetectable ABA changes are not sandboxed. These checks validate the preserved projection and its successful report; they do not reopen/redecode the historical compressed capture or certify a live collector's current state. No crash/power-loss durability beyond the shared publication primitive is claimed.

## Actual TDD and regression results

Commands used from `D:/repos/mev_bot-north-star/tools/data-pipeline` with the installed Python/PyArrow environment:

```bash
PYTHONPATH=src python -m pytest tests/north_star/test_partitions.py -q
PYTHONPATH=src python -m pytest tests/north_star -q
```

Observed vertical red/green cycles:
- Initial writer absence: 1 failed, then 1 passed. Initial command without `PYTHONPATH=src` was a collection/import error, corrected before the genuine red assertion.
- Configuration/budgets: 10 failed / 1 passed, then 11 passed.
- Resume/orphans: 2 failed / 16 passed, then 18 passed.
- Input completion/integrity: 9 failed / 20 passed, then 29 passed.
- Strict envelope/content validation: 21 failed / 33 passed, then 54 passed.
- Oversized-input preflight: 1 failed / 56 passed, then green.
- Final focused run: **57 passed in 1.28s**.
- Full North Star regression: **750 passed in 6.07s** (existing 693 plus 57).

Coverage includes exact uint64/int64 limits, null preservation, literal/duplicate/empty key lists, malformed config, interruption, stale temp, both orphan forms, active/unfinished inputs, missing/changed reports, mutation before commit, corrupt bytes/schema/count/content, duplicate JSON keys, unknown/missing columns and atomic publication races.

`git diff --check` returned exit 0. Static literal checks found no shell/eval/pickle/credential-assignment patterns in either new Python file. Ruff is not installed. Independent fresh-context review is left to the parent agent; no independent-review approval is claimed.

## Preserved sample exercise

Input (read-only):
`D:/mev_bot-artifacts/north_star/development/raw_adapter/part0000_first100_v2/events.jsonl`

New output directory, asserted absent before creation:
`D:/mev_bot-artifacts/north_star/development/partitions/part0000_first100_v2_envelope_v1/`

Artifacts:
- `events.parquet`
- `events.parquet.receipt.json`

Actual readback compared **all 98 rows and all 32 columns** with the original JSON objects, converting only Arrow map representation back to dict. Equality passed; schema types/nullability and schema metadata matched. Row groups: `[32, 32, 32, 2]`. Exact resume returned the identical receipt without modifying output mtime. Source SHA remained unchanged. Input report accounts for 100 raw-prefix rows: 98 accepted, 2 rejected; accepted record types are 54 transaction, 42 account, 2 slot.

- Parquet bytes: `136808`
- Parquet SHA256: `eae768efdeaa923e4182dfa61e67d2d641868051e004a793e97e2db3d189d050`
- Events SHA256: `87d8f3210a9e392add7da328feff6b5ad6b20d7c85cd4a02d40f6b54e030586e`
- Report SHA256: `f03f3165b553678091b569dda6daa26bbbdc4060ad4d6a1ad87f3aed6b693a21`
- All-column content SHA256: `f4c5694cd7cbe13709d093509e1f3ded9ae11037d300a865cf2e3c61df739235`
- Configuration SHA256: `c384239ed6b88ba597ee6719817c79959a11091528b1e4dceeed58e1f363df36`
- Schema SHA256: `282a11a8becc2fe3bab43f70e2ff7a4619632e8da2dcabada04a2cef1690a5d4`
- Writer SHA256: `6a9ad827bf34dfe6e136a53761f41dd3055b14eedcdb4d62bbabde4bdf5b8449`
- PyArrow: `25.0.1`

Preflight available RAM: `253436559360` bytes; D: free: `355270959104` bytes. Combined write/readback/resume exercise elapsed `0.312717` seconds; measured peak working set `101236736` bytes. These are this small engineering run's measurements, not scale estimates.

## Independent-review bounds correction (current code)

Review `deleg_0c1ef71f` identified that `_inspect` hashed arbitrary replacement output and decoded every row before checking footer counts/group bounds. This correction changes only the three owned files listed above. No collector, shared module, existing sample artifact, original worktree, or live capture was modified; no commit or new real-data transformation was performed.

Current readback now enforces:
- A fixed **128 MiB output-byte ceiling**, checked against the stable-reader descriptor's initial size before `hashlib.file_digest` or opening Parquet. It applies to pending publication and resumed output independently of receipt claims. This is an additional development ceiling, not a claim that every permitted 100-row/8-MiB-line input fits it; larger valid output fails closed.
- Footer row count equal to the report's expected accepted count before batch iteration; row-group count at most expected rows, each group nonempty and no larger than `batch_rows`, and summed group rows equal to the expected count.
- Running decoded-row and per-batch row checks before batch validation/Python materialization; readback stops on the first over-budget batch. Final decoded count must still match.
- Exact `int` report rejection indices and record-type counts, rejecting Python/JSON bool and integral floats before output creation. Rejection indices are range checked; type counts must be nonnegative and no greater than accepted rows.

The existing explicit 100-row development ceiling, schema/exact-value preservation, unadmitted/training-ineligible metadata, content hashing, no-clobber publication, and orphan handling remain unchanged.

### Actual strict RED → GREEN evidence

Each new test group was run and observed failing for the reviewed behavior before its production change:

| Target (`-k`) | RED | GREEN |
|---|---|---|
| `oversized_output` | 2 failed: attempted forbidden output hash | 2 passed, 57 deselected (0.69s) |
| `bad_footer` | 4 failed: attempted forbidden batch decoding | 4 passed, 59 deselected (0.63s) |
| `running_row_cap` | 2 failed: attempted forbidden over-budget validation | 2 passed, 63 deselected (0.50s) |
| `requires_exact_integer` | 6 failed: malformed reports accepted | 6 passed, 65 deselected (0.17s) |

The oversized-output cases exercise both direct pending-style inspection and resume; both spy on hashing and Parquet opening. Actual rewritten synthetic Parquet fixtures test excess/fewer footer rows, oversized groups, and empty groups without decoding. Synthetic batch fault injection verifies running caps stop before validation/materialization or requesting another batch. All fixtures are temporary engineering data, not fabricated evidence of a real-data run.

Final commands, run through terminal from `D:/repos/mev_bot-north-star/tools/data-pipeline`:

```bash
PYTHONPATH=src python -m pytest tests/north_star/test_partitions.py -q
PYTHONPATH=src python -m pytest tests/north_star -q
```

Interpreter: `C:/Users/Alon/AppData/Local/hermes/hermes-agent/venv/Scripts/python.exe`.

After removing redundant post-decode footer checks:
- **71 passed in 1.90s** (targeted).
- **815 passed in 6.67s** (combined North Star suite, including concurrent sizing-worker tests present at execution).
- `git diff --check`: exit 0 (unrelated tracked `OPERATING_CONTRACT.md` emitted an LF/CRLF warning). The owned Python files are untracked, so their trailing whitespace was separately checked programmatically: none.
- Current writer SHA256: `cdb8d62e3c4f49c93ba19a2fe5e1b4402587ad890f97aa31248230107af57bf6`.

Read-only sample verification still returned exactly `136808` bytes and SHA256 `eae768efdeaa923e4182dfa61e67d2d641868051e004a793e97e2db3d189d050`. No sample resume/rewrite was run. The earlier sample writer hash/measurements above are historical, not claims about the patched writer. Because receipts pin writer bytes, the preserved old sample is expected to refuse resume under this revised writer's provenance; its receipt was deliberately not rewritten.

### Remaining bounds limitations

The fixed file-size ceiling and footer/row limits are not a hardened malicious-Parquet memory sandbox: PyArrow must parse the footer, and a compressed variable-width value/page may allocate during the first bounded batch decode before Python can examine it. No decoded-byte/page allocation budget or separate process memory limit is added here. The shared trusted-directory concurrency/ABA limitations and nontransactional data/receipt publication limits still apply. Independent re-review of this correction remains for the parent; Stage 2 semantic/admission closure is not claimed.

## Independent re-review and current-writer sample

Independent re-review `deleg_f1f1a0f5` task1 returned bounded_pass with no must-fix:71 focused tests plus10 independent boundary/type checks. Parent then exercised the revised writer on the same exposed98-event input into NEW `D:/mev_bot-artifacts/north_star/development/partitions/part0000_first100_v2_envelope_v2/events.parquet`; exact resume returned identical receipt. Parent independently read all98 rows and32 columns and verified equality with source JSONL, uint/list/map handling, unadmitted flags and SHA256 `eae768efdeaa923e4182dfa61e67d2d641868051e004a793e97e2db3d189d050`. Old v1 artifact/receipt remains untouched. This is successful small transformation evidence, not permission to scale beyond the explicit100-row cap.

## Remaining Stage 2 work

Semantic decoding, exact monetary units, source/rights admission, per-field availability semantics, event identity/finality correction policy and independently reconciled economic ledgers remain open. This receipt does not authorize training, model fits, corpus scaling, or closure of master stages 0–8.
