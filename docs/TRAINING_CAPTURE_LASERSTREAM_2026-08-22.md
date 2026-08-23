# Training Capture Mode — pq-laserstream-grpc `--training-capture`

## Overview

The `--training-capture` mode extends the existing `pq-laserstream-grpc` binary
(without changing production behavior) to capture broad, lossless Pump.fun
bonding-curve + PumpSwap activity from Helius LaserStream gRPC for future Qwen
training data generation.

## Design Principles

1. **Lossless truth first**: The raw NDJSON.ZST files preserve the full
   protobuf-derived transaction/meta/account/slot/block-meta truth. No fields
   are dropped or reduced.
2. **Causal correctness**: The events NDJSON is hindsight-free — it never
   labels GOOD/BAD/profit live. It contains only observable truth at arrival
   time, preserving arrival order and canonical chain order.
3. **No USD market cap**: All amounts are integers (lamports, token base units)
   or exact rationals. No canonical floats, no USD market cap.
4. **Broad capture**: No mayhem/cashback/complete/account-required/data-slice
   optimizations. We capture winners, losers, weird tokens, creates,
   graduations, failed transactions — everything.
5. **CONFIRMED commitment** (not PROCESSED): avoids fork-rolled transactions.
   The production/low-latency path remains PROCESSED, unchanged.
6. **No secrets**: The API key is never written to any file. The endpoint host
   (without key) is recorded in the manifest. No private keys are logged.
7. **No LLM inference per trade**: The recorder only decodes and writes. GLM
   builds/tests the recorder only.

## CLI

```
pq-laserstream-grpc                           # production mode (PROCESSED)
pq-laserstream-grpc --training-capture        # training capture, 300 min default
pq-laserstream-grpc --training-capture --smoke # ~60s smoke test
pq-laserstream-grpc --training-capture --duration 950  # explicit duration
```

## Environment Variables

| Variable | Purpose | Secret? |
|----------|---------|---------|
| `LASERSTREAM_ENDPOINT` | Helius LaserStream gRPC URL | No (endpoint ref) |
| `HELIUS_API_KEY` | Helius API key (gRPC auth) | **Yes** — never logged |
| `WALLET_ADDRESS` | Our wallet pubkey (for `is_our_wallet`) | No (public) |
| `TRAINING_CAPTURE_DIR` | Output directory (default: `training-data/`) | No |

## Output Artifacts

All files are written to the `training-data/` directory (gitignored, never
committed).

### 1. `pumpfun_laserstream_raw_v1_<SESSION>_partXXXX.ndjson.zst`

Lossless zstd-compressed NDJSON. Each line is a JSON object with one of:

- **Transaction record**: Full protobuf tx + meta — signatures, message
  (account_keys, header, instructions, recent_blockhash, versioned,
  address_table_lookups), meta (err, fee, pre/post balances, inner instructions,
  log messages, pre/post token balances, loaded addresses, return data,
  compute units consumed), slot, tx index, is_vote, receive time, raw hash.
- **Account update record**: Full account info — pubkey, lamports, owner,
  executable, rent_epoch, data (base64), write_version, txn_signature,
  slot, is_startup, receive time, raw hash.
- **Slot record**: Slot, parent, status, receive time.
- **Block-meta record**: Slot, blockhash, parent_slot, parent_blockhash,
  executed_transaction_count, entries_count, block_time, block_height,
  receive time.

Files rotate at 500,000 lines so each part is independently decompressible.

### 2. `pumpfun_laserstream_events_v1_<SESSION>.ndjson`

Causal normalized `pump_event_v1` events. Uncompressed NDJSON, one event per
line. Each event is a decoded pump.fun / PumpSwap trade/lifecycle event with:

- **Event type**: `create`, `buy`, `sell`, `complete`, `migration`,
  `pumpswap_buy`, `pumpswap_sell`, `pumpswap_create_pool`,
  `pumpswap_deposit`, `pumpswap_withdraw`, `unknown`
- **Venue**: `pumpfun` or `pumpswap` (by program ID — discriminators overlap)
- **Identifiers**: mint (b58), trader (b58), creator (b58), curve/pool account (b58)
- **Amounts**: All integers — amount_in/out, min_amount_out, max_amount_in,
  fee_bps, fee_lamports, CU consumed, tx status, err hex
- **Curve state** (when account updates are available): virtual_sol,
  virtual_token, real_sol, real_token, curve_complete, mayhem, cashback
- **PumpSwap state** (when available): base_reserve, quote_reserve, lp_supply
- **Token metadata** (from create events): token_name, token_symbol,
  token_uri, decimals, initial_supply, initial_virtual_sol,
  initial_virtual_token
- **Provenance**: signature (b58), slot, tx_index, event_index, recv_unix_ms,
  is_live, raw_hash
- **Full truth for labeling**: pre/post SOL balances, pre/post token balances,
  inner instructions (b64), log messages, account keys (b58)
- **is_our_wallet**: True if our wallet address appears in the account keys
  (safe — public key only, no private key material)

### 3. `pumpfun_laserstream_manifest_v1_<SESSION>.json`

Capture metadata:
- Schema versions (raw, events, manifest)
- Repo SHA (git HEAD at capture time)
- Endpoint host (no key), commitment level, programs
- Start/end times (unix ms) + start/end slots
- Duration minutes
- Raw files (filename, bytes, SHA-256 hash)
- Events file (filename, bytes, SHA-256 hash)
- Counts: creates, pump buys/sells, completes, migrations,
  PumpSwap buys/sells, create_pools, deposits, withdraws
- Quality: duplicates, decode_failures, unknown_events, reconnects, gaps

## Future Labeling Compatibility

The `pump_event_v1` stream is designed for ONE later labeler (shared with
Slinky) to derive:

- **Markouts**: 1/2/5/10/30/60/120/300s price changes
- **MFE/MAE**: Maximum Favorable/Adverse Excursion
- **Threshold-first timing**: Time to reach X% gain
- **Graduation/time**: Time to PumpSwap migration
- **PumpSwap transition**: Bonding-curve → AMM lifecycle
- **Peak/time-to-peak**: Maximum price + time to reach it
- **Collapse/survival**: Token death + survival metrics
- **Simulator P&L**: After latency/fees/slippage

If our wallet appears (`is_our_wallet: true`), the preserved truth is
sufficient to reconstruct actual entry/exit/realized P&L later.

## Build

The crate is **server-build-only** (Linux/WSL2). The `protobuf-src` crate
requires a real C++ toolchain + `make`, which MSYS/git-bash lacks. Build in
WSL2:

```bash
wsl -d Ubuntu -- env -i HOME=/home/alon USER=alon bash -c \
  'export PATH=/home/alon/.cargo/bin:/usr/bin:/bin:/usr/local/bin; \
   cd /mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only && \
   cargo build --release'
```

The binary is at `target/release/pq-laserstream-grpc` (Linux ELF).

## Files Changed

| File | Purpose |
|------|---------|
| `src/main.rs` | CLI parsing, mode dispatch, env var loading |
| `src/capture.rs` | Training-capture orchestrator (gRPC subscription, signal handling, manifest finalization) |
| `src/raw_recorder.rs` | Lossless zstd-compressed NDJSON recorder with file rotation |
| `src/normalizer.rs` | Causal event normalizer (pump_event_v1 decoder) |
| `src/events_writer.rs` | Uncompressed NDJSON events writer |
| `src/manifest.rs` | Manifest writer (JSON metadata) |
| `src/encoding.rs` | Base58/base64/hex/SHA-256 utilities |
| `Cargo.toml` | Dependencies: serde, serde_json, sha2, zstd, chrono |
| `.gitignore` | Added `training-data/` to ignored paths |

## Constitution Compliance

- §22: No f32/f64 on outcome-controlling paths. All amounts are u64/u128 integers.
- §61: LaserStream gRPC on mainnet.
- §64: Disconnects don't fabricate state.
- §65: Live observation distinguished from replay (`is_live` field).
- No secrets logged (API key never written to files).
- Production/low-latency path unchanged (PROCESSED, unchanged filter logic).
