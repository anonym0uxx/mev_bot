# Task 2: PumpPortal WebSocket Lane — Acceptance Evidence

**Date:** 2026-07-31  
**Directive:** §4.3 (HERMES_PHASE_B_ACTIVATION_ONESHOT.md)  
**Status:** LIVE — lane is up and flowing  

## Build

```
cd tools/stream-capture-rs
RUSTFLAGS="-C target-cpu=znver5" cargo build --release -j 16
```

Build completed in 10.56s. Binary: `target/release/pq-stream-capture.exe` (2.6 MB).

The stream-capture crate is NOT part of the workspace; it builds standalone
with minimal dependencies (ureq + rustls, no tokio/serde/tungstenite).

## Launch

```
./target/release/pq-stream-capture.exe pumpportal
  > /d/tmp/pumpportal/live.jsonl
  2> /d/tmp/pumpportal/stderr.log
```

- **Endpoint:** `wss://pumpportal.fun/api/data` (no auth, no credential)
- **Subscriptions:** `subscribeNewToken` + `subscribeMigration`
- **Process:** PID 27992, background, still running at time of journal

## Live Data Confirmed (criterion 65 — provider-replay vs live observation)

All measurements below are **LIVE OBSERVATION** — the WebSocket connected to
`wss://pumpportal.fun` and received real-time token creation events from the
pump.fun mint stream. No replay, no cached data, no provider-replay.

### Subscription Acknowledgements (live, within 1ms of each other)

```json
{"lane":"pumpportal","recv_unix_ms":1785521765899,"raw":{"message":"Successfully subscribed to token creation events."}}
{"lane":"pumpportal","recv_unix_ms":1785521765900,"raw":{"message":"Subscribed to 'migration' events."}}
```

### Token Creation Events (live)

- **Count:** 33 `txType:create` events in 65.8 seconds
- **Rate:** ~0.5 events/sec (30 events/min — consistent with pump.fun mint cadence)
- **First event recv_unix_ms:** 1785521767733 (1.8s after subscription)
- **Last event recv_unix_ms:** 1785521831746

### Sample Event (live observation)

```json
{
  "lane": "pumpportal",
  "recv_unix_ms": 1785521767733,
  "raw": {
    "signature": "6RzS725ZkHL5Y5oytkKSbSPpfjGyEuCwNBiJ3wJxRSUySUAtqCa5zgxB7ioaJhxTfo7HhTiGdhWoVBESMCdTRgf",
    "mint": "4233nw91CWLQXpgMBpNg6YP3PCW7ZyUVHpZTLyo7pump",
    "traderPublicKey": "8AKsAJ2YTS3cMbeFidfvgMe3AK7ba6FepRcT42NPanYX",
    "txType": "create",
    "initialBuy": 3520918.746848,
    "solAmount": 0.098765431,
    "bondingCurveKey": "7RHixEXXNGc2o2xdPVVTTTZeEy7aCmmgNyxhq8wZ59Pg",
    "vTokensInBondingCurve": 1069479081.253152,
    "vSolInBondingCurve": 30.098765431,
    "marketCapSol": 28.14338864462131,
    "name": "Slots",
    "symbol": "SLTS",
    "uri": "https://ipfs.io/ipfs/bafkreibnxsaszcmvru3wwjxwisaysgpoq7wwipbdbshn6uo22y653svvce",
    "is_mayhem_mode": true,
    "pool": "pump"
  }
}
```

### Data Shape

Every event is emitted VERBATIM per §6.3 (raw-bytes-first):
```json
{"lane":"pumpportal","recv_unix_ms":<epoch_ms>,"raw":<payload>}
```

The payload is the PumpPortal JSON untouched. The `pump-quant-ingest::pumpportal_parse`
crate already consumes this exact shape downstream.

## Criterion 65 Compliance

Every record in this report is labeled as **LIVE OBSERVATION**. No provider-replay
data was observed or reported. The `recv_unix_ms` field is the capture clock (§22),
not a provider timestamp. The lane emits raw payloads before interpretation.

## Acceptance Bar

- [x] WebSocket connected to `wss://pumpportal.fun/api/data`
- [x] Subscriptions sent (`subscribeNewToken`, `subscribeMigration`)
- [x] Live data flowing (33 token-creation events in 65.8s)
- [x] Raw-bytes-first emission (§6.3) confirmed in output format
- [x] No credential required (no auth, no API key)
- [x] Process stable (75s+ uptime at journal time)
- [x] Staleness watchdog armed (PUMPPORTAL_STALE_SECS = 60s)

## Notes

- PumpPortal asks clients NOT to open multiple connections (one process = one socket).
- The lane is DISCOVERY tier (§6.6/§28) — corroborated on-chain before canonical use.
- No migration events observed in the 65.8s window (expected — migrations are rare
  relative to token creations).
- The `is_mayhem_mode: true` field indicates pump.fun's mayhem mode is active.
- Process continues running in background (proc_2d1d70b7a21d, PID 27992).

## Metrics

- common_chat_peg_parse: 0 (delta: 0)
- Largest stop_processing n_tokens: 112,858 (carry-forward)
- compression_count: 6 (delta: 0)
