# PHASE-B PREFLIGHT — executing this build with a weaker model (2026-07-29)

**Who this is for.** Any model handed Phase B, and specifically a **quantized GLM-5.2** or similar
smaller open-weight model. The constitution's model-capability clause
(`docs/HERMES_ONE_SHOT_PROMPT.md:22`) already permits this: *"a frontier engineering model or a
smaller local open-weight model such as GLM-5.2. The requirements are identical for both; only the
working method adapts."*

**What that clause gets right and what it misses.** Its four remedies — re-read the section, work in
milestone order, re-read when uncertain, never claim completion from memory — all treat the weaker
model as a **forgetful frontier model**. That is a real failure mode and the remedies are correct
for it. It is not the failure mode that costs SOL. The one that does is a model that reads every
sentence, understands each one, and still draws a confident wrong inference — and every remedy in
§1 is conditioned on the model already knowing it is uncertain (*"when uncertain whether a
requirement applies…"*). **A model that knew it was fabricating would not fabricate.** This document
exists to replace self-knowledge with mechanism.

---

## 1. Preflight — run this first, paste the output, stop on any mismatch

**Run it as a script, not as a table:**

```
python scripts/preflight.py           # ENVIRONMENT: is this box able to build at all?
python scripts/phase_b_preflight.py   # TREE: is this checkout the one the docs describe?
```

Both. They check different things and neither substitutes for the other — the first
is per-machine and one-off, the second is per-work-item. `phase_b_preflight.py` runs
every row below, prints a verdict each, and exits non-zero if any blocking row failed.
**Paste its output verbatim; do not summarise it.** A table has to be read, obeyed and
honestly reported on, which is three places to drift; the most likely drift is not
skipping a row but running it, seeing red, and reasoning that the red is unrelated.

The rows, for reference — and note `phase_b_preflight.py` re-derives the decision
vector **from `baselines.rs` at runtime**, so it can never carry a stale copy the way
this table can:

Do not begin any Phase-B work item until every row passes. Do not proceed on a remembered pass.

| # | Command | Expected | On mismatch |
|---|---|---|---|
| 1 | `git rev-parse HEAD` | a commit on `origin/main` | STOP — you are on a fork or a stale checkout |
| 2 | `git status --porcelain \| grep -v "^ M"` | empty | STOP — untracked or staged work exists that this build did not create |
| 3 | `cargo fmt --all -- --check` | exit 0 | fix and re-run |
| 4 | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | fix and re-run |
| 5 | `cargo test --workspace --no-fail-fast` | **0 failures** | STOP |
| 6 | `cargo test -p pq-regression` | all pass | STOP |
| 7 | `cargo test -p pq-regression --test hermes_doc_pins` | 6 pass | STOP — the docs you are about to follow disagree with the code |
| 8 | `python scripts/regression_e2e.py` | exit 0 | STOP |
| 8a | `cargo test -p pump-quant-core --test ostune_conformance` | 10 pass | STOP — the OsTune acceptance battery is broken |
| 9 | `python scripts/ci_gate.py --repo . --config supervisor/config/supervisor.yaml` | exit 0 | STOP |
| 10 | `cmp docs/HERMES_ONE_SHOT_PROMPT.md CONSTITUTION.md` | identical, or `CONSTITUTION.md` absent | if it exists and differs, the untracked local mirror is stale — **`cp docs/HERMES_ONE_SHOT_PROMPT.md CONSTITUTION.md`** and never read the mirror as authority |

**The pinned decision vector** (source of truth: `rust/crates/pq-regression/src/baselines.rs` and
`rust/crates/pump-quant-app/tests/golden_digest.rs`). Row 7 above proves the documents quote these
correctly, so you do not have to check by eye:

```
GOLDEN_DIGEST            = 13_693_021_370_354_439_552
GOLDEN_NET_LAMPORTS      = 31_111_528
GOLDEN_PROMOTED          = 504
GOLDEN_ADMITTED          = 11
GOLDEN_REJECTED          = 448
GOLDEN_UNIVERSE_FILTERED = 72
GOLDEN_ALPHACALL_NET     = +815_594     <- NOT -2_721_835; that reading is retired
```

**If any document you read disagrees with these, the code governs and the disagreement is itself a
§7 halt.** Report it. **Never reconcile a doc/code disagreement by editing the code.** This is not
hypothetical: `GOLDEN_ALPHACALL_NET` was quoted stale in the activation directive's own halt
checklist for two re-pins, on the exact line that decides whether to halt.

---

## 2. STOP AND ASK IF — preconditions, not events

`docs/HERMES_PHASE_B_ACTIVATION_ONESHOT.md` §7 lists halt conditions and they are correct, but every
one of them is an **event that has already happened at runtime** — a failed signature, an unhealthy
provider, a budget counter tripping. None is a **decision you are about to take**. The silent
failures are all in the second category. These are phrased as preconditions so they fire *before*
the damage:

1. **You are about to write an `AccountMeta` list you did not decode from a real on-chain
   transaction.** STOP. `docs/VENUE_TX_LAYOUTS.md` §4.1 is marked UNVERIFIED ON-CHAIN on purpose.
2. **You are about to substitute a default, placeholder, or plausible-looking value for something
   you could not resolve** — a pubkey, an endpoint, a key, a discriminator, a fee recipient. STOP.
   `Pubkey::default()` is the System Program and every venue validates it as something else.
3. **You are about to change a value in `baselines.rs`, `golden_digest.rs`, `REGRESSION_MANIFEST.md`
   or `golden_tape.rs`.** STOP. Post the seven-value decision vector before and after, and the cause.
4. **You are about to flip any config field in `LAW_BOOL_DEFAULTS` or `LAW_INT_DEFAULTS`**
   (`baselines.rs`). STOP. That requires an A-11 study **and** operator approval under §68 /
   criterion 111. It is not yours to take. This especially includes LAW B7 — the directive warns
   that *"if you find yourself reasoning toward arming it, that is the expected pull."*
5. **You are about to write the words "verified", "enumeration complete", "zero unexplained gaps",
   "no determinism impact", or "confirmed".** STOP. Attach the command and its literal output, or
   delete the word.
6. **A number in a document disagrees with a constant in the code.** STOP. See §1.
7. **You are about to "fix" something that looks obviously wrong in a file whose comment says not
   to.** STOP. `PUMPSWAP_BUY_DISCRIMINATOR = [10,20,30,40,50,60,70,80]` in
   `ex_construction_gate.rs` looks like a bug and is load-bearing: the real PumpSwap `buy`
   discriminator is byte-identical to pump.fun's (both are `sha256("global:buy")[..8]`), so the
   gate needs a synthetic one to keep the venues distinguishable in fixtures.
8. **You are about to mark a JUDGMENT or RESEARCH item (§3) complete.** STOP — those require
   operator sign-off. MECHANICAL items you may complete on a green gate.

---

## 3. Work-item classification — what may be completed on a green gate

| Class | Meaning | Authority to complete |
|---|---|---|
| **MECHANICAL** | The spec fully determines the output and success is checkable by a test that exists or is specified. | **The gate.** Green gate = done. |
| **JUDGMENT** | The spec states a goal or constraint; you must decide *how*, or notice something unstated. | **Operator sign-off.** |
| **RESEARCH** | Requires verifying an external fact — on-chain layout, provider behaviour, IDL version — where a confident wrong answer is the default failure mode. | **Operator sign-off, with the primary evidence attached.** |

### Activation directive §4

| Item | Class |
|---|---|
| 1 — release build, `RUSTFLAGS` from the infra manifest, never `target-cpu=native` | MECHANICAL |
| 2 — credential provisioning, private-repo visibility confirmed first | JUDGMENT (the CI secrets check is WARN-only by design — Amendment A-12) |
| 3 — stream lanes: LaserStream, Enhanced WS, PumpPortal, RPC failover, webhooks, Discord | MECHANICAL for wiring credentials into pre-built, pre-tested adapters; JUDGMENT for "preserve raw before interpretation" and "distinguish provider-replay from live", which no test asserts on a live socket |
| 4 — soak acceptance evidence | JUDGMENT — "zero *unexplained* gaps" has no definition of *explained*; failover parity (digest equality) is the one mechanical sub-item |
| 5 — OsTune + the seed-only-re-pin judgment | **RESEARCH** (was JUDGMENT). Now specified end-to-end in `docs/OSTUNE_BUILD_SPEC.md` with `ostune_conformance` as a real acceptance test, so the *adapter* is mechanically checkable — but §4.0's `unsafe`-versus-§24(b) contradiction and the `Config`/digest decision are STOP AND ASK, and the Linux-ism ban is lint-enforced globally |
| 6 — fee sampler → versioned calibration | MECHANICAL |
| 7 — pre-trade `simulateTransaction` on the real sell route | **RESEARCH** — depends on an unverified account layout |
| 8 — sender submission client under the signing boundary | **RESEARCH** — where a confident wrong answer costs real SOL |

### `docs/SERVER_BUILD_MANIFEST.md`

MECHANICAL: §2 (LaserStream credentials + soak), §4 (RPC failover — its acceptance criterion is a
byte equality, the best-specified item in the manifest), §5 (bench + RUSTFLAGS + PGO), §7 (funded
probes — protected by the `BankrollOrigin` type, see §4 below), §8 (fee sampler), §11 (webhook
creation), §12 (Discord Gateway).
JUDGMENT: §1 (OsTune).
RESEARCH: §3 (Jito ShredStream — and note the constitution's headline mentions ShredStream
**sunset** handling while the manifest section does not; check before building a dead lane), §6
(signer + live submission), §9 (`simulateTransaction`), §10 (Birdeye plan-tier gates — *"never
fabricate"*, and tier matrices change).

**Every item on the critical path to a signed transaction is RESEARCH.** That is the shape of the
risk: the plumbing is safe to delegate; the chain interface is not.

---

## 4. What the repo catches for you, and what it does not

**Catches, mechanically:**

- **The golden digest**, three independently-checked layers: `golden_digest.rs` pins the vector;
  `pq-regression/src/golden_tape.rs` is a *second, independent reconstruction* over the public
  engine API that must reach the same digest; `regression_manifest.rs` asserts the markdown
  narrates the same numbers. Two constructions reaching one digest is a real determinism tripwire.
- **`hermes_doc_pins.rs`** (new, 2026-07-29) — the documents you are handed must quote the live
  pins. This is the guard that turns "notice the contradiction" into a red test.
- **`check_dossier_test_integrity`** — you cannot alter a materialized dossier property test to
  make it pass; it is re-hashed against what its dossier renders.
- **`check_no_stubs`** — `todo!`, `unimplemented!`, stub panics in production `src/`.
- **`check_tests`** — verifies *named required tests actually ran*, not merely that some passed.
- **`substance.py`** — added after an overnight run *"passed every gate while producing seven files
  containing only doc comments and zero code."* Empty implementations fail.
- **`runner.py::_trust_check`** — compares your self-claims against verified reality and records
  over-claiming. Claiming completion is itself measured.
- **Global lints** (`ALL_RUST` scope): `mlockall`, `sched_setaffinity`, `/tmp/` paths — these *will*
  catch a builder reaching for Linux tuning idioms on a Windows target.
- **`BankrollOrigin`** — `PaperSeed::require_live_verified()` fail-closes. A wrong build refuses to
  arm rather than sizing off a config seed. **This is the template every other refusal should copy.**

**Does not catch:**

- **Anything outside the golden tape.** The tape is a file-driven replay fixture: no socket, no
  signer, no submission, no account meta, no OsTune call is exercised by it. It detects *drift*,
  never *correctness*. It is 11 admitted trades in 5 markets, statistically indistinguishable from
  zero.
- **A coordinated edit.** `golden_digest.rs` and `baselines.rs` mirror each other **by hand-copy**;
  a builder who edits both plus the manifest passes all three layers. (`hermes_doc_pins.rs` now
  closes part of this by asserting the app-side file contains the values pinned here.)
- **A wrong account list.** `ex_construction_gate`'s fixture-parity rung compares the built
  instruction against `golden_fixture(op)` = `serialize(&build_ix(op))` — **a golden derived from
  the builder's own output.** If your account list is wrong, the golden is wrong the same way and
  the rung passes. `build_ix` emits 3 synthetic accounts against a real bonding-curve `buy` that
  needs 17. Rung (b), live-state simulation, returns `false` unconditionally by design and is your
  Phase-B work. **Until a golden fixture is rebuilt from a recorded real transaction, criterion
  77(a) validates you against yourself.**
- **Floats, panics, syscall clocks or `unsafe` in the Phase-B crates.** `rust/lint_rules.yaml`
  scopes `hot_globs` and `money_globs` to named crates, and `pump-quant-execution`,
  `pump-quant-ingest` and `pump-quant-journal` are in **neither list**, nor do they carry
  `#![forbid(unsafe_code)]`. The submission client — the code that computes what to send to chain —
  is currently outside the §22 integer-only enforcement. Treat §22 as binding there by hand, and
  say so in your work record, until the lint scope is extended.
- **`check_bench` p99/p999.** Only p50 is parsed from criterion output, so higher-percentile budgets
  silently never bind.
- **Arming a disarmed law.** LAW B7's neutrality test asserts it is *inert on the golden tape*,
  which stays true whether it is armed or not. `cargo test` will not stop you. §2 item 4 will.

---

## 5. Compliance with A-11 and A-13 — where the letter is easy and the point is not

Both amendments are largely **judgment**, and a weaker model will produce artifacts that satisfy
their form. Read this before writing any study.

**A-11(1) pre-registration** — *"the PRE-REGISTERED RULE, written before any number was measured."*
Nothing in the artifact distinguishes a rule written before measurement from one written after.
**Mitigation:** commit the pre-registered rule as its own commit, before running anything, and cite
that commit hash in the study. A hash is checkable; a claim is not.

**A-11(2) vacuous mirror** — a law's counter-tape must not be one on which the law is *inert by
construction*. Recognizing that is a structural observation about a generator, not a number.
**Mitigation:** in every mirror, assert the law's decision path was actually **entered** (a counter
> 0), not merely that the net was 0. An untaken branch and a taken-but-harmless branch look
identical in the net and are completely different findings.

**A-11(4) materiality basis** — the bar is one 0.1 SOL bite = 100,000,000 lamports, judged
**absolutely** where the book is large relative to a bite and **relatively** where it is not, *"with
the book size and the choice of basis stated explicitly."* The golden tape's entire book
(31,111,528) is **smaller than one bite**, so applying an absolute bar there is the reporting defect
A-11 names. **Mitigation:** state the book size and the chosen basis as the first line of the
FINDINGS section, every time, or the verdict is void.

**A-13(3) "prove the enumeration was complete"** — this has **no procedure** and a frontier model
already got it wrong once, producing −379,067,452 that read as a damning verdict on the strategy and
was a verdict on two unfixed cohort blocks. **Mitigation:** do not claim completeness. List the
sites you changed and the search that found them, verbatim, and hand the completeness question to
the operator. "Here is my search and here is what it returned" is honest; "enumeration complete" is
not something you can know.

**A-13(4) admission-count nullity** — *"a synthetic tape arbitrates direction only if it is first
shown to ADMIT under the arms being compared. A tape whose admission count is zero… arbitrates
nothing — report it as a null, never as a verdict."* The failure is reading zero admissions as "the
law rejected everything, so it is protective." **Mitigation:** print admitted-count for both arms
before printing any net. If either is 0, or if they differ for a reason other than the lever, write
NULL and stop.

**A-13(5) chase the falsification** — now partly mechanical: `hermes_doc_pins.rs` fails if a
document stops quoting a live pin. It does not catch a falsified *claim* (a sentence, not a number).
For those, the obligation stands and it is judgment.

---

## 6. The honest summary

**A quantized GLM-5.2 can build:** the release profile and RUSTFLAGS injection; credential wiring
into the pre-built, fixture-tested capture lanes; the fee-sampler calibration; RPC failover (its
acceptance criterion is a digest equality it can run); the live-bankroll wiring, because
`BankrollOrigin` makes the wrong answer refuse rather than compile. It will pass the CI gate, the
full test suite, clippy, the hot-path lint and `regression_e2e.py`.

**It will fail silently at:** the on-chain account layouts, because the only validation is against a
fixture it generates itself; the digest-move judgment, unless preflight row 7 is green; A-11/A-13
substance, because "pre-registered", "vacuous mirror", "prove the enumeration was complete" and
"report it as a null" have no executable form; arming a disarmed law, because no test forbids it;
and every point where refusing is correct, because the repo's one working refusal is enforced by a
type and the rest are enforced by prose.

**So the split is:** hand it the MECHANICAL items and let the gate judge them. Hand it the JUDGMENT
and RESEARCH items only with a human reading the output, and require primary evidence — a recorded
transaction, a dashboard screenshot, a command and its literal output — rather than a conclusion.
**The generalisable move is the one this document is an instance of: every time you would rely on
the builder noticing something, spend the afternoon turning it into a red test instead.** The repo
already knew how — `regression_manifest.rs` has done exactly that for the manifest since it was
written. `hermes_doc_pins.rs` extends it to the documents. There is more of this available, and
each one is worth more than a paragraph of instruction, because a test does not care which model is
reading it.
