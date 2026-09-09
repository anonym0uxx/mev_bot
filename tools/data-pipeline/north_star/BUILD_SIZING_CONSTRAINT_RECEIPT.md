# Offline sizing constraint build receipt

## Delivered scope

- `src/north_star/sizing_constraints.py`: pure `evaluate_entry_size(size_lamports=None, *, available_cash_lamports=None, pending_reservations_lamports=None)` evidence-producing predicate.
- `tests/north_star/test_sizing_constraints.py`: 51 passing tests built through successive RED/GREEN behavior slices.
- This receipt. No commits, collectors, order submission, runtime/config edits, or Rust exit changes were performed by this task. Other agents' working-tree changes were left untouched.

Branch inspected: `task/north-star-build`; HEAD inspected: `0b4fe0c94979f7594d84c55ce3b1400ef7a0c062`.

## Source and semantics

Source: `north_star/OPERATOR_SCOPE_UPDATE.json`, `sizing.max_entry_lamports`, `sizing.cap_lift_requires_explicit_operator_change`, and the update's explicitly partial authorization status.

Raw-file SHA-256: `ca1632456b13c93d398a4043a82c5a26d004ffdc1c3c4a15747f08da6dcdff27`.

Every result includes that reference and pinned hash. The test hashes the actual source bytes and checks the exact native-int cap and authorization fields. The predicate does not read files at runtime; its metadata identifies the reviewed snapshot, not a claim of runtime source freshness. Source changes require reviewed repinning; a cap lift requires an explicit operator change.

- Hard **per-order entry** cap is exact native integer `250000000` lamports, from operator literal `.25` SOL. No caller cap override exists.
- Reject unset/None size, bool, float (including NaN/infinity), string/container, int subclass, negative size, and any size above the cap; no coercion or clipping.
- Zero satisfies this nonnegative upper-bound predicate only. It is not permission to execute a zero-size order.
- Optional cash/reservations use None for unknown. Supplied known values must also be exact native nonnegative integers. Malformed supplied values reject even when the other input is unknown.
- Cash means caller-supplied cash **before** the supplied pending reservation total. If cash alone is known, enforce that upper bound but leave net spendable cash unknown. If both are known, deduct reservations with integer arithmetic; reservations exceeding cash explicitly reject. Do not supply already-netted cash with the same reservations, which would double-count them.
- Pending reservations alone cannot establish affordability; no cash or bankroll is inferred. Missing optional facts do not invalidate the independent cap check, and result flags disclose which bounds were actually checked.
- Starting bankroll remains undecided (operator range 2–3 SOL); selected bankroll, maximum drawdown, and credits remaining remain None. Cash observations do not select a bankroll.
- **Cumulative position cap is unspecified**, not equated with the entry cap. ADD semantics remain unapproved. No split-order circumvention recommendation or approval is provided.
- Passing `size_constraints_satisfied` is sizing-only evidence. `live_orders_authorized`, `add_semantics_approved`, and `full_operating_contract_approved` remain false. This is one Stage 1 operating value, **not full stage closure or an economic gate**. Fees, snapshot freshness, aggregate exposure, and other execution gates remain outside this predicate. Existing deterministic Rust exits are unchanged.

## Executed strict TDD evidence

Command, from `tools/data-pipeline`:

```text
PYTHONDONTWRITEBYTECODE=1 python -m pytest tests/north_star/test_sizing_constraints.py -q -p no:cacheprovider
```

Each behavior slice was added and run before its implementation; successive observed results:

| Slice | RED | GREEN |
|---|---|---|
| Boundary, pinned provenance, limited authority | 1 failed: offline sizing predicate missing | 1 passed |
| Over-cap rejection | 3 failed, 1 passed: previously accepted sizes above cap | 4 passed |
| Exact native integer / omitted-size rejection | 14 failed, 4 passed: invalid values accepted, misclassified, or unsupported | 18 passed |
| Known cash bound | 3 failed, 18 passed: missing cash keyword API | 21 passed |
| Known pending reservations | 7 failed, 21 passed: missing reservations keyword API | 28 passed |
| Malformed optional numeric rejection | 23 failed, 28 passed: absent strict optional validation | 51 passed |

Final targeted run: **51 passed in 0.04s** (exit 0).

Full regression command:

```text
PYTHONDONTWRITEBYTECODE=1 python -m pytest tests/ -q -p no:cacheprovider
```

Observed: **4 failed, 803 passed in 6.79s**. All failures were in the separately owned, concurrently edited `tests/north_star/test_partitions.py::test_bad_footer_refused_before_decode` variants `more_rows`, `fewer_rows`, `large_group`, `empty_group`; message: `bad footer must be rejected before decoding batches`. They are not sizing failures and were not modified here. This receipt does not claim the full suite passed.

Regression isolation command:

```text
PYTHONDONTWRITEBYTECODE=1 python -m pytest tests/ -q -p no:cacheprovider --ignore=tests/north_star/test_partitions.py
```

Observed: **744 passed in 4.85s** (exit 0).

Additional real direct-call assertions passed for zero, one, cap-minus-one, cap, cap-plus-one, explicit unknown optional values, missing size with known cash/reservations, and fresh independent result lists. `git diff --check` exited 0, with an unrelated existing LF/CRLF warning for `OPERATING_CONTRACT.md`.

## Independent review

Review `deleg_170fa842` returned BOUNDED_PASS with no must-fix findings: 51 focused tests and 10,648 synthetic combinations passed; contract mutation and unsupported override probes rejected as expected. Parent independently verified all three source/implementation/test hashes below remain unchanged. This closes review of the offline predicate only, not runtime integration or Stage 1. Earlier full-suite failures above are historical concurrent-work observations, not a current full-suite result.

## Artifact hashes

- `src/north_star/sizing_constraints.py`: SHA-256 `8cf98021cdd5fe46c186323f983494c6785a7b2d209a902fd6e66a921c6855b2`
- `tests/north_star/test_sizing_constraints.py`: SHA-256 `29438767b691bb76006c6abfeeb46f12d7176af46d569a06bc17835d546f5132`

All test amounts besides the source cap are synthetic offline fixtures, not measured account balances or evidence of operational approval.
