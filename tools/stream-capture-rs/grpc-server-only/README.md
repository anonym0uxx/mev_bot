SERVER-BUILD-ONLY: requires network to crates.io; not compilable on the laptop profile — see docs/SERVER_BUILD_MANIFEST.md.

# `pq-laserstream-grpc` — Helius LaserStream gRPC tap (Yellowstone)

The PRIMARY low-latency transaction stream for the server deployment
(Helius **Business/Professional plan** — LaserStream gRPC is not available on
Developer). The laptop-profile fallback is `../` (`pq-stream-capture
helius-ws`), which speaks Enhanced WebSocket instead and has **no replay**;
this crate exists precisely because the official `helius-laserstream` SDK
handles reconnect **with `from_slot` resume** internally — the stream hole a
raw WS reconnect leaves is replayed by the SDK, not papered over.

## One-command server build

```sh
cargo build --release   # inside grpc-server-only/, with crates.io reachable
```

No `Cargo.lock` is committed; it is generated at the server (§ SERVER_BUILD_MANIFEST).
This crate is deliberately NOT in any workspace so `cargo` invocations in the
rest of the tree never try to resolve its (unreachable-offline) dependencies.

## Run

```sh
HELIUS_API_KEY=... ./target/release/pq-laserstream-grpc \
    [--accounts-file accounts.txt] [--programs p1,p2]
# LASERSTREAM_ENDPOINT overrides the default
# https://laserstream-mainnet-ewr.helius-rpc.com (pick the region nearest the
# server; ewr = US-East).
```

Subscribes (commitment PROCESSED):
* `transactions`: vote=false, failed=false, `account_include` = accounts-file
  entries + `--programs` ids (default: PumpSwap + pump.fun programs);
* `slots`: `filter_by_commitment=true` (staleness heartbeat downstream);
* `accounts`: `owner = [pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA]` — every
  PumpSwap pool account mutation.

Emits one NDJSON line per update:
`{"lane":"laserstream","recv_unix_ms":...,"kind":"transaction|account|slot|block_meta|other","raw_b58_or_json":...}`
where `raw_b58_or_json` carries the base58 transaction signature / account
pubkey plus the SDK's Debug rendering of the full update (JSON-escaped).
Phase-B consumers that need the full protobuf consume the stream in-process;
this binary is the capture-tap/visibility form of the lane (§6.3: nothing is
dropped — the Debug rendering is lossless over the update contents).
