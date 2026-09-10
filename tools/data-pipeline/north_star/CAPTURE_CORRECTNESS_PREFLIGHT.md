# Capture correctness / deployment preflight — NOT DEPLOYED

## Outcome and scope

Read-only inspection requested September 9, 2026, at operator-provided 18:32 PT. **The suspected Pump.fun literal is not malformed: it decodes to exactly 32 bytes and matches the publisher's pinned Pump IDL. Do not replace it with the conflicting v2-builder literal.** The genuine confirmed defect is the old all-zero Base58 encoder; reviewed fix `ec2b576baa5e25e9e6aa1ce62147836be1fe4804` is present only in the North Star source worktree, not the original capture executable.

No collector, WSL distribution, daemon, training job or trading process was launched. No compilation, package installation, RPC request, source edit, binary replacement, capture-file mutation or deletion was performed. Initial examination was fully local; subsequent credential-free HTTPS reads retrieved publisher IDLs and pinned their commit. This report is the sole intentional file write. No credentials, endpoint values or signed URLs were printed. Existing raw preservation is the parent task's responsibility and is not re-certified here.

## Exact identity validation, before lookup

A Python standard-library Base58 decoder accumulated the exact string as a big integer and reconstructed leading zero bytes. No token was repaired, padded or normalized before decoding.

| Exact literal | Characters | Decoded bytes | Result |
|---|---:|---:|---|
| `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` | 43 | 32 | Valid public-key encoding; publisher Pump IDL match |
| `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` | 43 | 32 | Valid public-key encoding; publisher Pump AMM IDL match |
| `6EF8rrecthR5Dkzon8NwseJy4tL5nMKCdia5j7T2qsk` | 43 | 32 | Syntactically valid conflicting identifier; not the address in the retrieved Pump IDL |

The questioned Pump key decodes to hex `0156e0f693665acf44db1568bf175baa5189cb97f5d2ff3b655d2bb6fd6d18b0`. This equals the explicit 32-byte `PUMP_PROGRAM_ID` in `rust/crates/pump-quant-protocol/src/venue_accounts.rs:60–63`. `registry.rs:29,214` uses the same string. The conflicting string occurs in `tools/data-pipeline/src/build_laserstream_gold_v2.py:42`; that file's claim that its normalized input was verified is not independent authority for program identity.

Publisher reference, pinned commit `9c82f61cb711b044a17f770ab8ce9f9bdf78f333`:

- Pump: <https://raw.githubusercontent.com/pump-fun/pump-public-docs/9c82f61cb711b044a17f770ab8ce9f9bdf78f333/idl/pump.json>; top-level `address` exactly the questioned literal; fetched bytes SHA-256 `b90bc471327f671449271d5d1d42354d1fae6f5a06502f5834459a3108138e49`.
- Pump AMM: <https://raw.githubusercontent.com/pump-fun/pump-public-docs/9c82f61cb711b044a17f770ab8ce9f9bdf78f333/idl/pump_amm.json>; top-level `address` exactly the current PumpSwap literal; SHA-256 `6b5c7ec4e5ef9742fa99dc57b0d75b1031b379bba02a7e1b3c5a4cad68d77e56`.

This is publisher-code identity verification, not a fresh on-chain owner/executable/account-type attestation. The configured web extraction gateway failed; direct public HTTPS succeeded without credentials. No Helius credits were consumed by these lookups.

## What the original launcher actually subscribes to

Original root: `D:/repos/mev_bot/tools/stream-capture-rs/grpc-server-only`.

`run_capture.sh:8–19` reads endpoint/API-key/wallet variables from the existing credential file inside WSL, strips CR, exports those variables and `TRAINING_CAPTURE_DIR`, changes directory to the **original** crate, and executes:

```text
target/release/pq-laserstream-grpc --training-capture --duration "$DUR"
```

Default duration is 300 minutes; the inspected second-session manifest reports 120 minutes. No program list is supplied by the script. `main.rs:90–145` reads endpoint, key, output directory, optional fallback manifest directory and optional public wallet; there is **no environment override for program IDs**, nor a parsed `--programs` option. Ambient ID variables cannot change this capture's subscription. `WALLET_ADDRESS` is only used for wallet flagging, not an account-required filter.

`capture.rs:35–38,98–135` constructs the concrete request:

- `transactions["pump_broad"]`: `vote=Some(false)`, `failed=None`, `account_include` contains the exact current Pump and PumpSwap literals above; account-required/exclude remain default empty.
- `accounts["pump_accounts"].owner`: the same two exact program IDs; no account-data slice or memcmp optimization is added.
- Default `slots["slots"]` and `blocks_meta["blocks_meta"]` subscriptions.
- Commitment `CONFIRMED`.
- `capture.rs:278–295` records those same constants into the manifest.
- `normalizer.rs:26–27` uses the same two IDs for venue routing.

The non-capture production branch is different: `main.rs:44–52` requests Pump only, non-vote, successful-only, `PROCESSED`. It is **not** the mode selected by `run_capture.sh`, and this task does not deploy or validate the production runtime.

Original `main` and worktree capture sources are text-identical except `encoding.rs`. Original `main` was `d17645623f764931b0b81866a0a2819103e0acdd`; local `origin/main...main` reported `0 0` without fetching. No local `master` branch exists. The worktree advanced concurrently from `858eefc5` to `75a2467064a799cb918d4c2d596e4435dc18b4e5`; do not assume this inspection froze the parent agent's branch.

## Manifest and bounded raw corroboration

Exact session `20260909_173928_000596`:

- Start `1788975568113` = `2026-09-09T10:39:28.113-07:00`.
- End `1788982768122` = `2026-09-09T12:39:28.122-07:00`.
- Manifest `repo_sha=d17645623f764931b0b81866a0a2819103e0acdd`, commitment `CONFIRMED`, programs exactly as above.
- Manifest-reported 10,244,602 raw records, 2,756,417 normalized events; creates 5, Pump buys 42,997, PumpSwap buys 996,558. These are not independently recounted totals in this preflight and are not proof of completeness or economic correctness.
- The manifest lists 205 raw parts plus one events file = **206 payload files**; including the manifest itself gives **207 files**, matching the original exact-session directory enumeration. Preserve all; never reinterpret “206 files” as permission to omit the manifest.
- A bounded read of the first 10,000 records in `part0000` yielded 2,369 account records, 7,505 transactions, 18 block-meta and 108 slot records. Account-owner fields included 351 exact current Pump IDs and 2,018 exact current PumpSwap IDs. This corroborates both requested owner lanes, not complete session coverage. No transaction-program count is claimed from this sample.

The executable contains both subscribed literals (four byte-string occurrences each), not the v2-builder alternative (zero occurrences). Together, launcher, source, executable strings, manifest and bounded raw owner observations support the actual filter above. There is no retained wire-request capture or historical process-environment attestation, so do not elevate this to cryptographic proof of the past server-side request.

Manifest quality reports unknown events and slot-gap entries. Low creates, class imbalance, gap semantics and unsupported instructions remain separate audit questions; neither a malformed program ID nor an encoder fix explains them automatically.

## Reviewed encoder fix versus installed binary

`ec2b576b` changes Base58's initial significant-digit vector from `vec![0]` to `Vec::new()` and adds seven tests. It corrects empty/all-zero values without substituting any program ID. `BUILD_ENCODING_RECEIPT.md` records seven passing standalone Rust tests and 74,194 unique differential vectors with zero mismatches. That receipt explicitly excludes a full capture application build. Those tests were **not rerun** in this read-only task.

Observed SHA-256:

| Artifact | SHA-256 |
|---|---|
| Original `src/encoding.rs` | `14c35252dc1069f7e7bf2c7e5dd330f15bcced1d7ddbb86d78aba1542ea1ca42` |
| Worktree `src/encoding.rs` | `67094ac31d128ea6d63a644ce363e0bfc68b8736e78ba40b6797d060a7ea6fec` |
| Original `target/release/pq-laserstream-grpc` | `634015eb96333b2d8c2291f81f2cc51bb368f8ac7e5de6e3919354a718319eca` |

Original executable is ELF64, 7,536,904 bytes. Its dependency file names original-worktree sources. The North Star crate has no `target/release/pq-laserstream-grpc`. Source fix availability is not deployment. Running the existing launcher again would select the old executable.

## Offline full-build viability: plausible, not verified

Initial `wsl.exe --list --verbose` reported **Ubuntu Stopped / WSL2** and **docker-desktop Stopped / WSL2**. At final scope verification Ubuntu had become **Running** externally; this task did not start or restart the distribution. A subsequent read-only Python inspection inside the already-running Ubuntu checked file/cache presence only, without invoking Cargo/rustc or compiling. There was no UNC access that could implicitly start WSL.

That inspection found 439 registry packages in the lock: **387 source trees available; 52 missing both source and archive**. All package-name sparse-index paths were present after correctly lowercasing index names (the initial `Inflector` case-sensitive path probe was corrected). No git/nonregistry package sources occur in the lock. Most absent packages are platform/optional dependencies (Windows, wasm, Apple, Android); others include `fiat-crypto`, `lock_api`, `parking_lot`, `parking_lot_core`, `scopeguard`, `solana-define-syscall`, `solana-stable-layout`, `valuable`, and `zerocopy-derive`. This is not a Linux target-feature closure evaluation: missing unrelated platform packages do not prove the Linux build impossible, and index-path existence does not prove the required version entry is cached. A full cross-platform `cargo metadata` can fail on these even when a target-filtered Linux build could succeed. Prospectively use `cargo metadata --offline --locked --filter-platform x86_64-unknown-linux-gnu --format-version 1`, followed by the actual frozen-target build/test; fail closed on any required missing dependency.

`/home/alon/.cargo/bin/{cargo,rustc}` and `stable-x86_64-unknown-linux-gnu` exist. `/usr/bin/gcc`, `g++`, `make`, `ld` and `pkg-config` were found; `cmake` and `protoc` were absent from PATH (cached protobuf-built protoc exists below). Tool executability/versions were not invoked. The observation reported 335,989,997,568 free bytes on `/mnt/d`; this is a point-in-time observation, not a reservation.

Windows-visible evidence supports an offline attempt after separate authorization:

- Original target has 1,610 `.rlib` and 1,606 `.rmeta` files (multiple versions/profiles; not a dependency-closure proof).
- Dependency files include `/home/alon/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/helius-laserstream-0.6.4/src/lib.rs`, plus older 0.5.0 artifacts. Match the lock, not whichever file appears first.
- Both Cargo locks parse to exactly the same structure, 440 packages, including `helius-laserstream 0.6.4`, `protobuf-src 1.1.0+21.5`, `zstd 0.13.3`. Their byte hashes differ solely with line-ending representation; LF-normalized text is equal.
- Original lock SHA-256 `e60450e9635979a8090be06ee5b9e5488b4c5fb39bb9476fe176bf622aa9ec51`; worktree lock `c2bd724babe6dcd6d7ac718cc181a3817af2cff608a68163fb981af76ea97ff4`.
- Existing release build trees contain two cached `protobuf-src` `out/install/bin/protoc` executables, each 8,998,192 bytes. This disproves “no Linux build artifacts exist,” not current full-build feasibility.
- Prior standalone receipt records WSL `/home/alon/.cargo/bin/rustc`, version `1.97.1 (8bab26f4f 2026-07-14)`. This is historical evidence, not a fresh version probe.

Do not use `build_in_wsl.sh` as-is: it targets the original crate, runs unrestricted `cargo build --release` without offline/locked flags, and has broad environment-unset logic. Do not reuse the original target as a write destination. Do not assume old comments claiming no WSL toolchain remain accurate.

## Master comparison and admission boundary

`F:/handoff-linux/northstar/2026-09-06-expert-v5/NORTH_STAR_WINDOWS_MASTER_V5.md` was read and hashed to its expected `ebee45d5b98a5daeed72edebf16e7ef2b8878d5cc96b191c451584a315cea57c`.

Master lines 334–337 require exact identity/owner/executable/type evidence, prohibit silently correcting source addresses, preserve authoritative raw lineage, and warn about migration gaps and nonexistent joined lifecycles. This report preserves exact literals and distinguishes valid encoding, publisher identity and unperformed on-chain checks. Publisher IDLs resolve the alleged typo; they do not admit this dataset or validate historical decoder/economic assumptions. LaserStream remains essential to the requested live runtime; no fallback substitution is proposed. Dataset acquisition authority is not training/trade authorization.

## Prospective integration plan — NOT EXECUTED

1. **Freeze inputs and approval boundary.** Parent must first finish preservation/verification of all 206 payloads plus manifest. Record an immutable reviewed candidate commit, source hashes and normalized lock hash. Check source/lock equality again because the worktree moves concurrently. Keep original source, executable, launcher and captures untouched. No program-ID change is indicated by this evidence.
2. **Authorize build execution separately.** Recheck Ubuntu state; do not start it without authority if it has stopped again. Confirm compiler/link tool versions and target-specific `/home/alon/.cargo/{registry,git}` closure; inspect configuration names/flags without revealing credential values. Review inherited `.cargo/config*`, rustup overrides, CPU flags and wrapper executables. Use the isolated crate, not the trading workspace or deploy-specific CPU/PGO profile. Missing required cache entries are a hard offline blocker, not permission for downloads.
3. **Build outside original target.** Select a new empty non-capture directory, for example `/mnt/d/mev_bot-artifacts/north_star/builds/capture-ec2b576b-preflight`. Do not use hardlinks to original artifacts. A normal byte-copy of a verified original target cache is optional, capacity permitting, but relocated/absolute-path build dependencies can still rebuild. Record copy provenance; never modify the original cache. In an authorized WSL shell, from the exact reviewed North Star crate and with sanitized compiler environment, use these build commands (not run here):

   ```bash
   export PATH=/home/alon/.cargo/bin:/usr/local/bin:/usr/bin:/bin
   export CARGO_HOME=/home/alon/.cargo
   export CARGO_NET_OFFLINE=true
   export CARGO_TARGET_DIR=/mnt/d/mev_bot-artifacts/north_star/builds/capture-ec2b576b-preflight
   cd /mnt/d/repos/mev_bot-north-star/tools/stream-capture-rs/grpc-server-only
   cargo metadata --offline --locked --filter-platform x86_64-unknown-linux-gnu --format-version 1
   cargo test --offline --locked --release --bin pq-laserstream-grpc
   cargo build --offline --locked --release --bin pq-laserstream-grpc
   ```

   First verify the chosen compiler/profile matches the inspected cache and review test inventory for network/side effects. Metadata must resolve the full graph; `--no-deps` alone is insufficient. Capture compiler/version, flags, exact source/lock identities and exit statuses. Do not quietly relax `--offline` or `--locked` on failure. Hash the resulting executable. A successful build alone is not capture integration.
4. **Exercise without connecting.** Full-crate unit tests must include the seven encoder tests. Add separately reviewed fixture-level serialization/normalizer integration checks if not already available: all-zero 32-byte keys become exactly 32 ones; ordinary and leading-zero keys/signatures remain correct; raw and events preserve account keys, balances, fees, inner instructions, instruction bytes and ordering. Test filter construction against the two verified IDs. Existing CLI has no offline replay or safe informational branch: **do not invoke `--help`, no-args, or `--smoke` as an offline probe**; `main.rs` does not parse help and proceeds to credentials/connection, and `--smoke` is live capture.
5. **Stage candidate, do not overwrite original.** Keep candidate at its versioned build path. Before any integration run, record binary SHA, reviewed source SHA, lock identity, exact request fields, intended duration, output directory and credential-reference name without values. Existing `run_capture.sh` hardcodes original binary/output; it cannot select this candidate merely through `TRAINING_CAPTURE_DIR` or `PQ_LASERSTREAM_BIN`. A separately reviewed explicit candidate invocation/wrapper is required. Do not point the live production runtime at the training-capture branch or launch a trading daemon.
6. **Gate any bounded live validation.** Obtain a current billing-cycle interval, measured account usage/other consumers, remaining allowance, explicit bounded duration and monitoring/stop budget. Operator-reported 150M allowance and 1.7M used do not establish the cycle date, refresh interval or authorize overage. Only then may an approved one-minute `--training-capture --smoke` candidate run write to a fresh uniquely named non-capture validation directory, with credentials read privately inside WSL, no xtrace, no endpoint/secret echo, and no inherited trading/signing configuration. Inventory process identities without secret-bearing command-line dumps; require no duplicate collector, a supervised terminal deadline and graceful finalization. No extension/restart is implicit.
7. **Verify before selecting candidate for future capture.** Read back exact output paths; parse manifest and every produced frame; verify SHA/size/counts, request identity, expected account-owner lanes and causal field fidelity; compare raw-derived truth to normalized events. Confirm binary provenance independently: `get_repo_sha()` records runtime cwd HEAD, not executable build SHA. Preserve validation output and old binary. On failure stop candidate only, preserve everything, and remain unpromoted; restarting the old defective binary requires its own decision. Promote only by a separately approved integration change and verify exact selected binary path/hash after that change.

## Remaining blockers / nonclaims

- No corrected full executable, no offline dependency-closure verification and no integration run yet.
- Ubuntu transitioned from stopped to running externally during this task. Read-only cache inventory found 52 absent platform/optional package sources; Linux target-feature closure and executable toolchain viability remain unverified. No distribution was started/restarted by this task.
- Billing-cycle/usage freshness and bounded live-validation budget remain unresolved. No overage authority inferred.
- Current output schema lacks binary-build provenance and has direct manifest writes; preserve external receipts and verify closure, rather than treating manifest existence as sufficient.
- Existing low-create/class imbalance, unknown events, gaps, exact transaction-accounting and migration coverage need their own audits. No automatic old-data rewrite, identifier replacement or deletion is justified.
- Original checkout already reports unrelated historical missing tracked raw parts and untracked helper files. They were not changed, repaired or deleted here. Worktree HEAD changed concurrently; this report does not claim exclusive repository custody.
