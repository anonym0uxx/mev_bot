# REGRESSION BASELINES — pinned end-to-end invariants

This file is the human-readable ledger of every invariant asserted by
[`scripts/regression_e2e.py`](scripts/regression_e2e.py), the authoritative
end-to-end regression runner. The runner exercises the repository the way a
fresh clone + CI would and exits **non-zero on any drift** below these pins.

Growth is always allowed (more tests, more taxonomy classes); a **drop** below a
pin is the regression these numbers exist to catch. Comparators are noted per
row (`==` exact, `>=` floor, `pass` boolean gate).

Run it with:

```sh
python3 scripts/regression_e2e.py --repo . --vendor-dir /tmp/vw/vendor
```

---

## 1. Rust workspace gate  (referenced, not duplicated)

The Rust digest / net-SOL / promoted-admitted-rejected counts are **owned by
QA-1** and live in the regression manifest
[`rust/crates/pq-regression/src/baselines.rs`](rust/crates/pq-regression/src/baselines.rs)
(narrated in QA-1's `BASELINES.md`). This runner **references** that manifest and
only asserts the gate is green — it does not re-pin those numbers here. The
canonical values, for cross-reference, are:

| Invariant | Value | Source (authoritative) |
|---|---|---|
| Golden decision-journal digest | `2_392_030_750_322_148_229` | `baselines.rs::GOLDEN_DIGEST` |
| Golden net-SOL (lamports) | `31_465_931` | `baselines.rs::GOLDEN_NET_LAMPORTS` |
| Promoted / admitted / rejected | `504 / 11 / 493` | `baselines.rs::GOLDEN_{PROMOTED,ADMITTED,REJECTED}` |
| Universe-filtered | `72` | `baselines.rs::GOLDEN_UNIVERSE_FILTERED` |
| `cargo test --workspace` | 1908 tests / 0 fail | live workspace run |
| `cargo fmt --all --check` | clean | live |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean | live |
| `pq-regression` crate (golden-tape / fail-closed tripwire) | all pass (16) | `cargo test -p pq-regression` |

The runner asserts: fmt clean, clippy clean, workspace tests all pass, and — if
QA-1's `pq-regression` crate is present — that crate's golden-tape tests all
pass. **If those numbers change, change them in QA-1's manifest, not here.**

---

## 2. Capture-suite test-count floors  (`>=`)

Each capture suite builds offline against the vendored crates.io tree (see
§Vendored build below). The floor is the **current hardened total**; removing
either the base suite OR the additive regression hardening trips the gate.

| Suite (`tools/…`) | cargo package | Pre-hardening baseline | Hardened floor | Hardening file |
|---|---|---:|---:|---|
| `stream-capture-rs` | `pq-stream-capture` | 122 | **134** | `tests/regression_ws_fuzz.rs` |
| `social-ingest-https-rs` | `pq-social-capture` | 183 | **191** | `tests/regression_drift_schema.rs` |
| `social-ingest-rs` | `pq-twitch-capture` | 23 | **23** | (no additive hardening) |

The additive hardening tests (owned by this regression layer, not the crate
authors) add: **+12** to stream-capture (WS frame-codec adversarial fuzz,
exhaustive truncation, webhook auth+dedupe idempotency, deterministic RPC
failover, per-lane fail-closed exit 3) and **+8** to social-ingest-https
(birdeye/coingecko drift-sentinel on perturbed fixtures, birdeye fail-closed
exit 3, and record-schema key-order pins for `birdeye_ohlcv_1d_v1` /
`birdeye_token_overview_v1` / `birdeye_token_security_v1`).

---

## 3. Portable CI gate  (`pass`)

`scripts/ci_gate.py --repo .` must print **`portable gate PASSED`**. It runs the
portable-profile checks (no-stubs, secrets, hot-path lint, RSS-trend soak,
dossier presence). Benchmarks and latency budgets are Phase-B and deliberately
excluded (§9.5).

| Invariant | Comparator | Baseline |
|---|---|---|
| `ci_gate.py --repo .` | pass | `portable gate PASSED` |
| hot-path lint globs non-empty | `>=` | hot `>=1`, money `>=1` (observed hot=9, money=11 from `rust/lint_rules.yaml`) |
| secrets check | policy | **WARN-only, non-blocking** (operator-accepted risk; findings still printed) |

The hot-path lint is now LIVE over the app crates: the runner parses ci_gate's
`hot=<n> money=<m>` scope line and asserts both are non-zero, so an empty scope
(which would make the lint vacuously "clean") is caught. The **secrets check is
WARN-only by explicit repo policy** and must stay that way — it prints findings
for visibility but never fails the gate.

---

## 4. Materialized dossier count  (`==`)

| Invariant | Comparator | Baseline | Source |
|---|---|---:|---|
| materialized dossier tests | `==` | **191** | `scripts/materialize_tests.py --repo . --verify` |
| `dossier_*.rs` files in workspace | `==` | 191 | `baselines.rs::DOSSIER_FILE_COUNT` |

---

## 5. Supervisor invariants

### Python test suite  (`>=`)

| Invariant | Comparator | Baseline | Observed |
|---|---|---:|---:|
| `python3 -m unittest discover -s supervisor/tests` | `>=` | **51** | 58 |

### Term-set sizes — must not silently shrink  (`>=`, closed sets)

| Invariant | §ref | Comparator | Baseline | Symbol |
|---|---|---|---:|---|
| Root-cause taxonomy | §56.5 | `>=` | **50** | `supervisor.analysis.root_cause.ROOT_CAUSE_CLASSES` |
| Evidence-status enum (KB-seeding) | §45.1 | `>=` | **7** | `supervisor.store.evidence.SEEDED_FINDING_STATES` |
| Inference ladder | §56.10 | `>=` | **7** | `supervisor.store.evidence.VALID_INFERENCE_STATES` |

Additive supervisor regression guards (`supervisor/tests/`):
- `test_regression_invariants.py` — taxonomy/enum count + closed-set + full
  round-trip through the store; SQL `CHECK` constraints agree with the Python
  tuples; the soak gate still **catches an injected leak** (not vacuous); and
  `evidence.py` migrations are **idempotent** (open twice + `_migrate` twice).
- `test_build_phase_split.py` — the criterion-109 **Phase-A / Phase-B clause
  split** (§9.5 / criterion 113): 109 is no longer a blanket Phase-B criterion;
  its deploy clauses (`pgo`, `latency_budgets`, …) force Phase-B while its
  Phase-A-obligatory clauses (`zero_alloc_harness`, `hot_path_purity_lint`,
  `unsafe_dossier`, `money_wrap`) are authoring-time-permissible; 103 stays
  wholly Phase-B.

---

## Vendored build convention  (offline capture-suite builds)

crates.io is **unreachable** in this sandbox. Each Rust crate resolves its pinned
dependency tree against a **local vendored copy** at `/tmp/vw/vendor` via an
**uncommitted** `.cargo/config.toml`:

```toml
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "/tmp/vw/vendor"
```

`regression_e2e.py` **writes this config into any capture suite that lacks one**
(pointing at `--vendor-dir`) and removes only the configs it created. It never
commits `.cargo/` or the vendor tree.

---

## Fresh-clone requirements  (NOT in the repo)

A fresh clone has neither the vendor tree nor a populated cargo registry. To run
the runner end-to-end you need **one** of:

1. **Network access to the real crates.io** — delete/skip the vendored `.cargo`
   configs and let cargo fetch, **or**
2. **The vendored tree reconstructed** at `--vendor-dir` (default
   `/tmp/vw/vendor`), e.g. via `cargo vendor` on a networked machine copied in.

The `rust/` workspace itself has **no external registry dependencies** and builds
with neither. **Live capture lanes** (not exercised by any test) additionally
need per-lane API keys documented in each suite's `README.md`
(`HELIUS_API_KEY`, `WEBHOOK_AUTH_SECRET`, `RPC_URLS`, `BIRDEYE_API_KEY`,
`CG_API_KEY`, `TWITTERAPI_IO_KEY`, `TIKTOK_API_KEY`, `FIRECRAWL_API_KEY`); every
lane is **fail-closed (exit 3)** without them. **The test suites never require a
key** — fail-closed refusal is itself asserted.

---

## What a fresh clone still needs that is not in-repo

| Requirement | Why it is not in-repo | How to satisfy |
|---|---|---|
| Vendored crates tree (`/tmp/vw/vendor`) | `.cargo/` + vendor are deliberately uncommitted | `cargo vendor` on a networked host, or reachable crates.io |
| Live-lane API keys | secrets are not committed (WARN-only policy) | export per-lane env vars; only needed for LIVE capture, never for tests |
| QA-1's `pq-regression` crate | concurrently owned by QA-1 | present in tree; runner degrades gracefully if absent |
