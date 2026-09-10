# Corrected capture binary — offline full build, not deployed

Parent executed on September 9, 2026, after the source-only preflight:

```text
CARGO_TARGET_DIR=/home/alon/northstar-reviewed-capture-target cargo test --locked --offline -j 4
CARGO_TARGET_DIR=/home/alon/northstar-reviewed-capture-target cargo build --release --locked --offline -j 4
```

WSL Ubuntu; source `D:/repos/mev_bot-north-star/tools/stream-capture-rs/grpc-server-only`. Background process `proc_ac57a314ac7f` exited 0. Full dependency graph compiled from cache with no network downloads. Release completion reported 1m45s after test-profile compilation. Fresh parent test rerun: **7 passed, 0 failed**, actual crate encoder tests, exit 0.

Candidate path `/home/alon/northstar-reviewed-capture-target/release/pq-laserstream-grpc`.
SHA256: `e87d98ac982d149b2f4afcfabc71b7fbbdb36e782f343566d243de15169cfc59`.

This confirms the corrected encoder compiles inside the complete capture application, not merely the earlier extracted harness. It does not certify live endpoint behavior or all raw serialization. Additional raw serialization fixture tests are in preparation and must be rerun before candidate selection.

Original binary, source, launcher and capture outputs were not replaced. No live candidate invocation, subscription restart, production trading changes or deployment was performed. Publisher-verified program IDs remain unchanged. The prior preflight's missing cached platform dependencies did not block this actual Linux-target offline build.

Future use must preserve external binary/source/lock provenance: the current manifest records runtime repository HEAD, which alone is not a binary-build identity. Existing raw strings are never normalized to repair historical malformed addresses.
