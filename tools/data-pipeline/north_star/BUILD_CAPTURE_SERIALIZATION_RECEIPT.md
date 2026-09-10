# Offline capture serialization receipt

## Result

**VERIFIED: 6 new serialization integration tests pass; complete crate suite 13 passed, 0 failed.**

The parent initially reserved `/home/alon/northstar-reviewed-capture-target`; no Cargo process was started until the parent explicitly reported its offline test/release compilation complete and authorized reuse. The initial execution-unverified status is superseded by the actual runs below. No network, live capture, original checkout, dependency changes, or production semantic edits were used.

## Scope and exercised path

Only `tools/stream-capture-rs/grpc-server-only/src/raw_recorder.rs` gained a `#[cfg(test)] mod capture_serialization_tests`; this receipt is the other authored file. Tests construct actual `helius_laserstream` protobuf types, invoke existing `build_tx_payload`, `build_account_payload`, and `build_slot_payload`, then pass those values through actual `RawRecorder::write` and `RawRecorder::finalize`. Each test reads the emitted `.ndjson.zst`, decompresses with the existing zstd dependency, verifies one complete newline-terminated JSON record and its envelope, and asserts on the decoded payload. There is no copied serializer or encoder and no mocked protobuf dependency. Test-created temporary directories are isolated using atomic directory creation and cleaned by an RAII guard.

The original raw-recorder production prefix is byte-for-byte preserved (SHA-256 below). `main.rs`, `encoding.rs`, Cargo manifests and dependencies were not edited.

## Test inventory

All names have prefix `raw_recorder::capture_serialization_tests::`:

1. `system_pubkey_is_exactly_32_ones_in_captured_payloads`: literal 32-character System Program encoding across static keys, loaded writable/readonly keys, lookup account key, blockhash, return-data program, account pubkey and account owner. No expected value is computed with the encoder under test.
2. `nonzero_loaded_keys_and_signatures_match_independent_goldens`: nonzero 32-byte keys, two distinct 64-byte signatures including a leading zero, signature hash, ordered loaded key vectors and lookup index vectors; account update keys/signature also exercised.
3. `raw_integer_precision_survives_ndjson_zstd_roundtrip`: integer values beyond IEEE-754 exact-integer range and up to `u64::MAX`, fees, balances, account lamports, rent epoch, write version, transaction index, CU/cost units; token raw decimal strings remain exact independently of rounded UI values.
4. `account_bytes_are_preserved_without_text_or_layout_decoding`: embedded zero, high bytes, CR/LF, quote, backslash and trailing zero checked by independent literal base64, length and SHA-256; empty bytes and absent transaction signature are also checked.
5. `inner_instruction_group_order_and_raw_bytes_are_preserved`: deliberately unsorted outer group indices, inner instruction order, duplicate account indices, empty bytes, data bytes, stack-height nullability, top-level instructions, return data and embedded-newline log text.
6. `provider_owner_fields_and_processed_status_are_not_truth_claims`: different supplied pre/post token owners remain distinct, account owner remains supplied data, Processed status remains exactly Processed, and transaction/account payloads do not invent top-level true-owner/finality/settlement fields.

Golden base58 strings were computed independently using Python integer conversion/divmod; base64 and hashes used stdlib base64/hashlib, never the production encoder as oracle.

## Actual execution

From WSL, after authorization to reuse the now-idle parent target:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
CARGO_TARGET_DIR=/home/alon/northstar-reviewed-capture-target cargo test \
  --manifest-path /mnt/d/repos/mev_bot-north-star/tools/stream-capture-rs/grpc-server-only/Cargo.toml \
  --locked --offline -j4 capture_serialization_tests -- --nocapture
```

Exit 0; actual output:

```text
Compiling pq-laserstream-grpc v0.2.0 (/mnt/d/repos/mev_bot-north-star/tools/stream-capture-rs/grpc-server-only)
Finished `test` profile [unoptimized + debuginfo] target(s) in 3.31s
running 6 tests
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.00s
```

Then the same command without the test-name filter or `-- --nocapture`:

```text
Finished `test` profile [unoptimized + debuginfo] target(s) in 0.28s
running 13 tests
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

No warnings/errors were emitted in these two runs. Added test module passes `rustfmt --edition 2021 --check` via stdin, exit 0. `git diff --check` on raw_recorder.rs passes. Whole-file rustfmt reports a pre-existing formatting difference at production `RawRecorder::write`; it was intentionally not changed.

No historical-encoder mutation RED run was performed. This is regression coverage against already-fixed production encoding, not a claim of a complete historical RED/GREEN demonstration.

## Dependencies inspected and used offline

The existing Cargo.lock resolves `helius-laserstream 0.6.4`, `laserstream-core-proto 11.3.0`, `serde_json 1.0.151`, `sha2 0.10.9`, and `zstd 0.13.3`. Types were checked against cached 11.3.0 `solana-storage.proto`, `geyser.proto`, and crate re-exports. An earlier local 9.4.0 proto cache was not assumed to match the active dependency; the actual WSL 11.3.0 schemas were inspected before writing fixtures.

## SHA-256 (exact file bytes)

| Artifact | SHA-256 |
|---|---|
| Modified `tools/stream-capture-rs/grpc-server-only/src/raw_recorder.rs` | `e6dd15c87fb14fee69c39fcfb0f3c0c5c96ea9186dbf585fdd160667623784c3` |
| Original raw-recorder bytes, preserved as the production prefix | `720ecd71515b368fcfbff0e6b458a28cec71a993cb7fe41e4f1ced4a7a11527b` |
| Unchanged `src/main.rs` | `cb8f3a1e84032e83c49392e3cd92094bcfe9036e33b8bd3abecb73a84a91fb4e` |
| Unchanged `src/encoding.rs` | `67094ac31d128ea6d63a644ce363e0bfc68b8736e78ba40b6797d060a7ea6fec` |
| Existing `Cargo.lock` used by offline test | `c2bd724babe6dcd6d7ac718cc181a3817af2cff608a68163fb981af76ea97ff4` |
| Executed test binary `/home/alon/northstar-reviewed-capture-target/debug/deps/pq_laserstream_grpc-4576271c00b33e01` | `4fc556243ff8462c0b26aa039372009d14bd3cb040a9dcaa8ef2420e6ee01cec` |
| Cached 11.3.0 `proto/solana-storage.proto` | `891088dba3aa5337ef31c8266eb5200f6aceb55038305338140025a5a805610b` |
| Cached 11.3.0 `proto/geyser.proto` | `6fe738b1f3ca262e6793164c61ab0b0e1ff59fc1c3286eba0440a8e8c40b754b` |

## Evidence boundary

Fixtures are synthetic and not observed on-chain transactions, verified signatures or valid economic histories. Passing tests establish the specified byte/value preservation and actual offline capture serialization path, not live subscription integration, beneficial/true ownership, execution success, settlement, commitment or finality. Account `owner_b58` is the supplied owning-program field, not proof of a controlling wallet. Token `owner` strings are supplied metadata, not independent ownership resolution. A Processed slot is not a finalized transaction.

This is not a completeness certificate for every current protobuf field: for example, the inspected current schema includes transaction-message config and rewards that the existing serializer does not emit. Those production behaviors are unchanged and outside this test-only scope. Release rebuilding or independent parent acceptance, if required, remains the parent's decision; the subagent ran tests, not a new release build.
