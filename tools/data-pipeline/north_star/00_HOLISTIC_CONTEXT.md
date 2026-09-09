# North Star — Current Build Ledger and Astra Review Entry Point

Status: **implementation kicked off; no complete North Star release certified**.
Updated: 2026-09-09 PT. This replaces earlier overclaims in this derived document.
Historical versions remain in Git; legacy stage filenames are not the governing DAG.

## 1. Authority and objective

Source of truth: `F:/handoff-linux/northstar/2026-09-06-expert-v5/NORTH_STAR_WINDOWS_MASTER_V5.md`
(master SHA256 `ebee45d5b98a5daeed72edebf16e7ef2b8878d5cc96b191c451584a315cea57c`)
and `NORTH_STAR_DATA_MIX_AMENDMENT_V6.md`. Original master-package file hashes were
verified during the independent audit. The full audited implementation plan is
[BUILD_PLAN.md](BUILD_PLAN.md); its stage contracts, tests, dependencies and D/G
mapping govern execution. Its review-time findings are historical; this ledger and
per-task receipts record subsequent changes.

Later operator decisions: Qwen is the memecoin **trading brain only**; Astra owns
Rust development. Collect narrative as a required dimension, not optional decoration.
Kelly sizing philosophy and net returned SOL per trade are objectives; exact sizing,
risk/capital, execution-support and statistical thresholds require explicit contracts.
Report full sequential net-SOL/cash/opportunity economics, not cherry-picked averages.
No human-labeled decision history exists. Do not generate missing teaching rationale,
pad examples or conflate observed trades with recommendations. Permissive-source and
source/license/token inclusion gates remain. Dataset build permission is not training,
live-order, reboot, disk/firmware or capital-escalation permission.

## 2. Governing stage map

| Master stage | Scope | Evidence-backed baseline |
|---|---|---|
| 0 | Receiver, source/rights/coverage registry | IN PROGRESS; legacy inventory reusable, source recovery now verified; full registry closure pending |
| 1 | Protect evaluation and operating/admission contracts | IN PROGRESS; tested policy implementation being built; protected future acquisition/membership not yet certified |
| 2 | Bounded canonicalization, identities, time, exact ledger | NOT CLOSED; legacy parsers exist but audited defects prohibit treating their gold as canonical truth |
| 3 | Parallel capture, narrative, features, annotation, entity/propagation graphs | PARTIAL; ongoing acquisition and prototype lanes; no full narrative admission |
| 4 | Development-only empirical execution calibration | NOT CLOSED; legacy scenario grids are not measured calibration |
| 5 | Sequential reconstruction and provenance-separated targets | NOT CLOSED; no complete position-aware episode corpus |
| 6 | Inherited splits and tokenizer/mask-aware exports | NOT CLOSED; legacy exporter machinery reusable, North Star guards still required |
| 7 | Independent whole-corpus certification | NOT CLOSED; historical PASS reports are not sufficient |
| 8 | Verified portable handback and training proposal | NOT STARTED; no automatic training/live promotion |

Stages 0–8, D01–D14, expert gap contracts G01–G12, and operational promotion gates
G0–G6 are distinct axes. D14/code-teaching is explicitly superseded for Qwen by the
operator override, while Rust semantic-conformance testing remains required.
Stage 1 reserves outcome-blind source/entity/time boundaries before fitting/curation;
Stage 6 materializes inherited membership. Stage 2 owns detailed ledger implementation.
Authorized raw acquisition can continue while these foundations are built; no claim
that a running collector closes Stage 3 or resolves causal availability.

## 3. Execution workspace and durability

- Original repo/live sources: `D:/repos/mev_bot` — left dirty and intact; unrelated
  supervisor/narrative modifications and raw deletions must not be reset or staged.
- Isolated task worktree: `D:/repos/mev_bot-north-star`, `task/north-star-build`, from
  fetched `origin/main` at `d17645623f764931b0b81866a0a2819103e0acdd`.
- Sparse checkout avoids copying tracked output/raw/trading-data directories. No
  current collector is redirected by creating this worktree.
- Artifact store: `D:/mev_bot-artifacts`. Code/review receipts only in Git; no bulk
  source data added. Task commits are pushed after real test/review checks; no direct
  commits to main. A later reviewed merge must respect trunk/build invariants.
- Windows owns build/management; the current capture binary runs in WSL. No dual-boot
  Linux config, partition, credentials or boot changes are implied.

## 4. Verified source preservation

Both original raw manifests were reconciled by exact filename, not directory count.
August 23 session `20260823_133256_000398` had 184 of 185 parts absent from both known
raw locations. **All 184 have now been recovered** from exact Git blobs to:

`D:/mev_bot-artifacts/recovered_raw/20260823_133256_000398/`

**184 files / 3,270,751,952 bytes**, each validated against original manifest SHA256
and size before publication; independent final readback found zero mismatches.
Per-file provenance: `RECOVERY_RECEIPTS.jsonl` in that directory. Atomic no-clobber
publication, mismatch refusal and resume validation were tested; see
[BUILD_RECOVERY_RECEIPT.md](BUILD_RECOVERY_RECEIPT.md).

One August 23 part remains in the original artifact raw store and must be linked by
the registry. August 24 session `20260824_053543_000288` has 368 of 369 parts in its
known store; `part0368` is still missing. No "negligible for training" conclusion:
map actual affected coverage, censoring and dependent tasks. Recovery is not source
admission or complete source registry certification.

## 5. Current Megga acquisition — preserve, do not overclaim

- Twitch stream ID `320254509148`.
- Media path `D:/mev_bot-artifacts/narrative/twitch_live/megga_320254509148.mp4.part`.
- On-chain session `20260909_144906_000490`; standalone WSL launcher, **120-minute
  bound**, started around 07:49 PT. No resident `capture_daemon.py` was seen at kickoff.
- Preflight around 08:49 PT confirmed original media and raw/events files growing;
  these are unfinished recordings, not finalized/integrity-certified episodes.
- `CAPTURE_CONTEXT.json` is acquisition metadata, not a speech-epoch authority.
  Capture-start metadata differs from first on-chain receive time and contains no
  measured media PTS↔UTC anchor/discontinuity/buffering uncertainty. Relative ASR
  timestamps cannot simply be added to this timestamp or broadcast start.
- Required: finalization receipts, segment/media clock mapping, source/speaker/rights
  provenance, exact mint/instruction/owner attribution, availability gating and
  narrative→numeric→future-only outcomes. Uncertain joins stay unresolved.
- The active stream is development/debug exposure, not a pristine sealed holdout.
- Collector-health deadline/byte freshness must remain monitored while building;
  extending/replacing capture must preserve semantics and avoid duplicate feeds.

One historical Megga VOD was transcribed. Its reported 21% keyword-matching yield
is a triage measure, not independently measured reasoning quality or narrative
coverage. Narrative sources/versions have different stored counts and heuristic
EX_ANTE/GOLD labels; count by source version and admission evidence, not aggregate
incompatible inventories. Historical narrative↔Slinky 1/317 overlap is a recorded
sample result, not a current full-corpus certification or proof that only live media
can ever solve temporal support.

## 6. Reusable inputs versus admitted evidence

Verified Slinky Parquet metadata: 798,430 token rows; 33,581,765 trade rows across 18
shards; 26,934,864 flow-bucket snapshots; 1,392,133 post-graduation snapshots;
1,016,374 wallet-stat rows. These establish input availability, not admission.
`wallet_stats` has whole-window totals; flow snapshots are not historical token-account
holder snapshots. Future/global token and wallet aggregates must not enter causal
inputs without prior-only reconstruction.

Legacy LaserStream raw projections, event compression, Arrow batching, human source
text, ASR tooling, deterministic IDs and some exporter tokenizer accounting are
reusable behind audited adapters. Raw JSON is a protobuf projection, not guaranteed
wire-complete data. Scalar Parquet drops evidence needed for re-decoding and account
reconstruction; even full normalized events do not replace raw account updates.

Slinky permissions were reported granted by the operator; record the actual grant,
permissive terms, version and permitted uses rather than silently inheriting an old
CLEARED status. Every other source needs its own rights and exact token disclosure.

## 7. Blocking defects and withdrawn claims

- **Economic labels:** tested Slinky multi-size function can sell benchmark token
  quantity for every size and ignores supplied tips in net PnL. Trace affected
  producer/columns/exports before using positive-class counts as BUY support.
- **Latency:** legacy corrected LaserStream writer repeats a size's quote/return
  across latency IDs. Scenario rows/unique IDs do not demonstrate latency robustness.
- **Causality:** future reserve fallback, forward no-trade/censoring fields in L1,
  and whole-input quantiles labelled train-only need independent lineage fixes.
- **Attribution:** static/loaded-key ordering and generic mint/trader account-index
  heuristics need authoritative venue-version fixtures. Signer intersection, largest
  SOL delta or missing RPC match do not prove a beneficial owner or bundle blindness.
- **D13:** raw supply exponent is not token decimals. The earlier 15-decimal statement
  contradicts its own supply figures. Mint metadata and event-version semantics govern.
- **KOL records:** per-(creator,mint) whole-window totals/min/max/consensus are not
  sequential decisions or human rationale. Preserve observed actions including bad
  decisions; economically justified recommendations are a separate target origin.
- **Retention:** old daemon deletes by mtime even after failed compaction; existing
  filename skips can leave partial Parquet accepted. It must not be deployed as safe
  supervision. New eligibility safeguards must be integrated and failure-tested before
  any source deletion is enabled.
- **Certification:** legacy vocabulary/count/sample checks do not prove economics,
  all-parent split disjointness or full corpus hashes. Recompute actual invariants.
- **Balance:** Markdown pseudo-tests are not a wired exporter/loader guard. V6 applies;
  10,000 BUY / 1,000 per class are derived draft thresholds, not independently approved
  restrictions. Resolve denominators and policy before export. Natural-prevalence
  economic eval stays separate from preregistered class-support diagnostics.
- **Supervision:** the three infra facts are metadata, not a running process owner.
  Failed `artifact:live_status` lookup proves only that lookup failed, not an empty
  evidence database. Actual Windows process ownership and gateway-survival tests are
  required; double connections do not establish exactly double billed credits.

No earlier GOLD/PASS/complete claim overrides these blockers. Root-cause repairs need
failing-before/passing-after tests and source-backed independent semantic validation.

## 8. Build deliverables and next gates

### Reviewed implementation tranche 2

Task branch checkpoints: `5e801c17` foundations; `bf5f8b07` exact FIFO, explicit
action semantics, bounded ancestry resolution, and Windows direct-child job runner.
Both pushed and independently read back from origin; neither merged to main.
Original worktree/collectors remain untouched by code edits.

- FIFO accounting passed 58 synthetic tests plus independent conservation review.
  Real-chain fee completeness, native cash/rent/rebate/migration adapters remain open.
- Dependencies/actions passed 31 tests after independent review fixes. Supplied
  graph closure is not proof the source producer declared all parents.
- Job runner passed 22 tests and executed the actual accounting test target through
  its owned subprocess with durable logs, exit0 and verified no-relaunch resume.
  It remains a synchronous primitive, not deployed gateway-independent supervision.
- Raw envelope adapter exercised first100 already-exposed source rows:98 projections,
  two explicit unknown-slot rejections. No beneficiary/mint heuristics or training
  eligibility; malformed JSON/Unicode review hardening precedes admission of code.
- First live-session files finalized:199 raw parts plus events,200 files totaling
  10059323128 bytes, hash-verified and copied outside legacy purge root. Later session
  started at10:39 PT after the first ended09:49 PT. `BUILD_CAPTURE_CONTINUATION.md`
  and artifact `CAPTURE_RESTART_GAP.json` preserve the known capture discontinuity.
  Megga media kept recording; no UTC alignment claim.



First implementation checkpoint evidence:
- `BUILD_INVENTORY_RECEIPT.md`: original store manifest **818/818 files**, **126807316863 bytes**, all SHA256 rehashed with zero missing/mismatches.
- `BUILD_RECOVERY_RECEIPT.md`: 184 recovered parts; complete raw rehash across roots gives August 23 **185/185**, August 24 **368/369**. Missing tail remains explicit.
- `BUILD_S0_RECEIPT.md`: requirements/source-policy primitives and regression tests; corpus evidence is not admitted.
- `BUILD_S1_RECEIPT.md`: outcome-blind split/contract primitives; actual protected-window reservation still pending.
- `BUILD_CAPTURE_RECEIPT.md`: passive checks and retention eligibility predicates; NOT deployed process supervision.
- `BUILD_ATOMIC_IO_RECEIPT.md`: small-object commit receipts and recovery integrity; not bulk Parquet canonicalization.
- `BUILD_SCHEMA_RECEIPT.md`: initial 20 canonical + eight v5 object registry and per-field availability primitives; complete typed rows/adapters/ledger still pending.

Raw artifacts and original collectors remain in the original workspace. The isolated worktree contains code and documentation only. `MEGGA_MEDIA_PROBE.json` in the artifact build-receipt directory confirms the growing file is MPEG-TS audio/video; UTC anchor remains unknown.

[BUILD_PLAN.md](BUILD_PLAN.md) gives the complete staged implementation, all canonical
objects, D01–D14 and G01–G12, WP-to-stage map, test names and required handback files.
First tranche: source preservation, requirement/source admission registry, outcome-blind
split policies, operating contract and passive capture/retention safeguards. Per-task
BUILD_*_RECEIPT.md files record real execution, not intentions. Subsequent gates are
schema/ledger/restart truth; full narrative and account/opportunity capture; empirical
execution; sequential labeled episodes; inherited exports; independent certification.

The final dataset must retain source/license/time/units/null-reason/availability/
lineage/split identity; human statements, actions, recommendations and later outcomes
are distinct. No generated rationales; missing sources remain gaps. Whole-session
coverage, losses, failures, idle opportunities, partial exits/runners/re-entry and
causal rivals/memory/propagation are mandatory dimensions—not only successful buys.
