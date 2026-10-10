# Parity fixtures — how these files were produced

These two JSONL files are the byte-parity oracle for `render_decision` and
`render_management`. Each line is `{"expected": <corpus user prompt>, "input": <typed
inputs>}`; the harness in `tests/parity_corpus.rs` rebuilds the typed inputs and asserts
the rendered prompt equals `expected` byte for byte.

- `decision.jsonl` — 161 rows, one per variant class (venue x curve-reserve presence x
  AMM-reserve presence x `no_prior_flow` x `evidence_status` x optional-field absence)
  plus a fill sample, drawn from `c11/candidate_sft_c11/train.jsonl`.
- `management.jsonl` — 151 rows, same sampling rule, drawn from the **c12** corpus
  (`/training/v2/candidate_sft_c12/train.jsonl`, the corpus the served model was trained on).
  `expected` is the c12 row's user prompt verbatim: no `ENRICHED CANDIDATE STATE:` /
  `DEV HISTORY:` / `LIVE FLOW STATE:` lines and no trailing newline. c12 does not print the
  enriched/dev/flow inputs, so they are joined from the matching c11 row on
  (mint, t_dec, step): 30,011/30,011 joined, 0 synthetic (`input.enr_dev_flow_source`).
  sha256 316bead55072305a4074c2664cab1a38e6a04ee495f5720950cc420f27885bee.
- `management_c11.jsonl` — the previous c11 fixture (151 rows, sha256
  6c5963a4442f9e485a4953f826914c84797896912ae8eae20982d14143510973), kept as evidence.
  The management renderer is expected to FAIL against it (it carries the three lines).

## Full-corpus validation (the real claim)

The fixtures are the fast regression guard. The claim "the renderer reproduces the
corpus" was established over EVERY row, with a Python reference implementation of the
same format table (ported 1:1 to Rust):

| family | rows reproduced | total | notes |
| --- | --- | --- | --- |
| decision | 67,301 | 67,301 | 100.0% |
| management_replay | 30,010 | 30,011 | 1 row excluded, see below |

The single excluded management row prints `mark price (SOL per raw token)` one unit in
the 12th significant digit away from what any `mark` in its own printed interval can
produce — the corpus row is internally inconsistent by 1 ulp of a derived print, so no
input reproduces it. Excluded rather than special-cased.

The oracle scripts live outside the repo (the build is Rust-only):

    /training/v2/code/src/v2/rl/parity_rust/p1_ref2.py          decision format table
    /training/v2/code/src/v2/rl/parity_rust/p1_mgmt.py          management format table
    /training/v2/code/src/v2/rl/parity_rust/gen_fixtures.py     decision fixtures
    /training/v2/code/src/v2/rl/parity_rust/gen_mgmt_fixtures.py management fixtures (c11)
    /training/mh_build/proc/mgmtrender/gen_mgmt_fixtures_c12.py   management fixtures (c12)

Regenerate (writes LF; the checked-in files are CRLF):

    cd /training/v2/code/src/v2/rl/parity_rust
    python3 gen_fixtures.py 150 /tmp/decision.jsonl
    python3 gen_mgmt_fixtures.py /tmp/management_c11.jsonl 150
    python3 /training/mh_build/proc/mgmtrender/gen_mgmt_fixtures_c12.py /tmp/management.jsonl 150

## Inputs that the prompt prints only lossily

The prompt prints `qty_pre` at `.6g`, `mark` at `.12g`, the pool depth at `.4g`/`.1f` and
the cost line's clip at `.2f`. The fixture loader therefore:

* takes the pool depth from the row's own `meta` where the corpus recorded it;
* recovers `qty_pre`/`mark` inside their print intervals so that every derived print
  (`position value at mark`, the two mark scalings, the cost line's impact) reproduces;
* for the decision family, recovers the SIZE OPTIONS depth from the printed interval —
  the entry corpus does not record it in meta. The cost arithmetic itself is pinned
  separately by `cost.rs`'s unit tests against the Python authority's golden numbers.

`serde_json` must be built with `arbitrary_precision` (see `Cargo.toml`): its default
float parser is not correctly rounded, and this fixture set contains values where that
shows up (it cost exactly 1 ulp on a mark price).
