# Fixtures — how these files were produced

## `decisions.jsonl` (120 rows)

Each line is one **real** c12 assistant completion plus the action/size/price the corpus
itself carries:

```json
{"action": "BUY", "completion": "DECISION: BUY\nSIZE: FULL\n...", "price_limit": null}
```

`tests/decision_parse.rs` asserts `parse_decision_payload` reproduces the corpus's own reading
of its own text. Regenerate with:

    python3 /training/v2/code/src/v2/rl/parity_rust/gen_decision_fixtures.py \
        rust/crates/pump-quant-inference/tests/fixtures/decisions.jsonl 300

Two facts the corpus forced, both load-bearing for production:

* **`PRICE LIMIT` is optional on BUY.** 5,415 of the 7,927 trained BUY rows carry none, so a
  parser that requires it refuses 68% of valid BUYs. When present it must be a finite,
  positive float.
* **`SIZE` is required on BUY and forbidden elsewhere.** Non-BUY rows carry `SIZE: NONE`;
  a tier on a non-BUY means the completion drifted off-distribution.
* **`MID` is retired as a SIZE but still advertised.** KELLY_AUDIT_C12 (2026-09-19) drops it
  from the vocabulary, and c12's labels never choose it (`FULL` 5,406 / `SMALL` 7,970 / `NONE`
  53,925). Its PROMPT still offers it, though: all 132,326 rows that carry the size menu render
  `SMALL`/`MID`/`FULL` at 0.25/0.50/1.00 SOL. So MID must keep parsing (the prompt invites it)
  while never sizing anything - the served size is the venue rule (`amm -> FULL`,
  `curve -> SMALL`), and a MID answer lands in `buy_with_venue_mismatched_size`.

The corpus's implicit management magnitudes (used by the seam, never emitted by the model as
a size): `ADD` adds 50% of inventory, `REDUCE` trims 50%, `EXIT` closes all — from
`build_management_c10.py::invert_archived`.

Provenance: `/training/v2/candidate_sft_c12/train.jsonl` (the ruled corpus). The generator
defaults to c12 and takes a corpus override as its third argument.
