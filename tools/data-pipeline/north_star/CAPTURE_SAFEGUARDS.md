# Immediate capture safeguards: passive implementation boundary

**Status: implemented/tested offline, NOT deployed into active collectors.**
This module pair cannot start, stop, restart, compact, finalize, rename or delete
capture files. It does not install a process owner, scheduler, lease database or
retention hook. Preserve the approved capture; do not launch the legacy daemon
as a preservation measure. The real two-sample observation is in
`BUILD_CAPTURE_RECEIPT.md`.

## Audited legacy risks (original repo, read only)

- `src/capture_daemon.py:42–52`: matching Parquet filename causes compaction to
  be skipped; failed compaction only logs a failure.
- `src/capture_daemon.py:58–81`: raw files are removed by mtime regardless of
  compaction outcome, session closure, leases or full-account dependencies.
- `src/compact_events.py:67–80,120–147`: default scalar projection omits account
  keys, balances, inner instructions and logs; final Parquet path is written
  directly, without an atomic validated completion receipt. Even `--full`
  normalizer output is not proof of full raw account fidelity.
- `grpc-server-only/src/capture.rs:257–297`: normal finalization finalizes raw,
  flushes events, hashes files, then writes a manifest. A missing manifest may
  mean active collection, finalization/hash work, interrupted capture or absent
  evidence; it does not alone prove a crash.
- `grpc-server-only/src/manifest.rs:138–140`: manifest itself is written directly;
  a present path can still be partial. We do not declare closure/integrity from it.
- `grpc-server-only/src/main.rs:124–128`: session prefix is generated in UTC.
  This supports an approximate bounded deadline, not precise capture start.

No audited source file is modified here. User context reported a standalone WSL
launcher and no observed resident capture daemon; this receipt does not claim a
new process census or infer restart ownership from a file's byte growth.

## Pure retention eligibility contract

`src/north_star/retention.py::retention_eligibility(evidence)` returns
`{"eligible": bool, "blockers": [stable_field_codes...]}`. It performs no I/O,
reads no clock and exposes no deletion function. Inputs are reviewed attestations,
not raw filesystem facts. **Eligibility is advisory, not deletion authorization.**

Every gate must be explicitly satisfied:

| Evidence field | Required value / meaning |
|---|---|
| `session_state` | `closed`, supported by validated terminal evidence |
| `active`, `frozen` | both literal `False`; missing or unknown blocks |
| `integrity`, `raw_sha256` | `verified`, canonical lowercase SHA-256 of the exact candidate |
| `compaction` | `succeeded`; failed/unknown always blocks |
| `derivatives` | nonempty list of complete, durable, integrity-verified derivative receipts |
| `dependency_inventory` | `reviewed_complete`, all downstream dependencies enumerated |
| `full_account_dependencies` | `durable_verified`, full accounts/balances/instructions/fees preserved, not scalar-only projection |
| `lease_state` | `cleared`, authoritative review of writer, compaction, audit and holdout leases; no guessed expiry |
| `retention_policy` | source-specific `review_approved`, including retention/revocation obligations |
| `retention_elapsed` | literal `True`, derived by the reviewed policy, never mtime alone |
| `review_receipt` | nonblank reference to independent retention review |

Each derivative requires `integrity=verified`, literal `durable=True` and
`complete=True`, nonblank `receipt`, plus canonical `input_sha256`,
`output_sha256`, `schema_sha256`, `code_sha256`, `config_sha256`. Input hash must
match the candidate raw hash. All derivatives must pass. The caller must
resolve and authenticate receipts, validate fsync/durable placement, reconcile
counts/coverage, and bind all dependencies before constructing attestations.
This pure predicate does not pretend that nonblank reference strings establish
those facts. Unknown/malformed data fail closed. No fallback deletion, age-only
exception, disk-pressure override or mutation of inputs exists.

Future integration requires separate authority/review, a real lease registry and
terminal/integrity/derivative receipts. Until then, this does **not** neutralize
legacy daemon purge code if someone independently runs that daemon.

## Passive health contract

`scan_sample(raw_directory, media_directory, session_id=..., stream_id=...,
channel=...)`:

- Validates exact identifiers before I/O; rejects wildcards, traversal and invalid
  session dates. Enumerates only direct-directory exact raw/events names and the
  exact `channel_streamid.mp4` / `.mp4.part` pair; unrelated sessions excluded.
- Stats payloads, records file names/bytes/mtime and observation Unix time. Does
  not decode, hash or read active raw/media payloads. Selected symlink files are
  excluded. Roots must be operator-selected trusted local directories.
- Reads at most 1 MiB plus a size sentinel from the exact manifest/context paths.
  Outputs only whitelisted identity/numeric/status fields; never dumps metadata,
  endpoint, title/note, process command line, log content, environment or media URL.
- Reports missing/inaccessible/racing files, manifest status and media final-name
  status without treating any of them as verified session integrity.
- Keeps `media_epoch_unix=null` even when context metadata time is present.
  `CAPTURE_CONTEXT.json.capture_started_unix=1788965406` is not media epoch.

`build_report(first, second, capture_minutes=120, warning_minutes=15)` requires
matching identities and an increasing finite observation interval. It retains
both observations, byte deltas, disappearance/shrinkage warnings and a bounded
LS deadline. No-growth in a short window is not a proven stall: buffering may
hide writes. Growth is not continuity, causal availability, source rights,
complete account evidence or narrative-to-market alignment.

The actual receipt uses a **60-minute warning threshold** to surface the
approved 120-minute LS session's approaching end. Expected end for
`20260909_144906_000490` is **2026-09-09 16:49:06 UTC / 09:49:06 PDT**, based on
session-ID generation time. A finalization/gap check is required at that bound;
there is no automatic extension, restart or claim of an unbounded capture.
Megga stream `320254509148` remains relative-only until media UTC epoch,
buffering/ad/discontinuity intervals and uncertainty are established.

## Reproduce offline tests and a passive observation

From `D:/repos/mev_bot-north-star/tools/data-pipeline`:

```bash
python -m pytest tests/north_star/test_retention.py tests/north_star/test_capture_health.py -q -p no:cacheprovider
```

Python API (no report write or collector control performed by these APIs):

```python
import sys, time
sys.path.insert(0, 'D:/repos/mev_bot-north-star/tools/data-pipeline/src')
from north_star.capture_health import scan_sample, build_report
raw = 'D:/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/training-data'
media = 'D:/mev_bot-artifacts/narrative/twitch_live'
identity = dict(session_id='20260909_144906_000490',
                stream_id='320254509148', channel='megga')
a = scan_sample(raw, media, **identity)
time.sleep(10)
b = scan_sample(raw, media, **identity)
report = build_report(a, b, capture_minutes=120, warning_minutes=60)
```

Persist only the resulting allowlisted report to an authorized non-capture
artifact path. Keep collector roots read-only. Do not rerun a launcher, modify
capture configuration or compact active files to obtain a health receipt.

## Acceptance limits

This is implemented/tested preservation support, not an admitted dataset or
live daemon safety deployment. Finalization/decoding, complete raw/account
coverage, retention review evidence, source rights, media timing and ongoing
supervision remain separate dependencies. Both modules use only Python stdlib;
pytest fixtures are small synthetic files and never touch production captures.
