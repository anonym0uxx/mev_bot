# North Star Stage 1 evaluation boundary policy v2

## Status: POLICY FROZEN; RESERVATION PENDING

This is a tested foundation, not completed Stage 1 or a protected dataset. `EVAL_FREEZE.json` contains no selected future window, no protected entity/row IDs, and no completed development subpartitions. **The future window is NOT selected and NOT protected.** No dates, IDs, capital or strategy thresholds are invented to make the gate green. Before-fit calls against this checked-in policy fail closed.

The master v5 sections 6–8 and appendix 8 govern temporal protection; the approved audit/build plan Stage 1 resolves v6 class diagnostics versus natural economics and later operator role decisions. Qwen is trader-only; Astra develops Rust. D07–D09 narrative remains P0. No generated human labels, no padding, no live trading authority.

## Outcome-blind reservation procedure

1. Inventory metadata only: exact source/version locators and hashes, collection status, verified entity/group IDs, absolute time support, dependency edges, prior exposure/ancestry and access logs. Do not inspect returns, terminal capitalization, outcome grades, best exits, eventual winners or class balance to choose protected boundaries. Published historical results quoted in the master are already exposed, not newly sealed evidence.
2. All currently inspected legacy/Megga evidence is **DEVELOPMENT/EXPOSED**, never pristine holdout. The concrete source locators currently listed in the JSON are a partial exposure ledger, not exhaustive source certification. Descendants inherit exposure. Unregistered material quarantines by default; remapping source IDs must retain source/version/duplicate ancestry, not evade exposure.
3. Before collecting a NEW untouched acquisition window, publish a new immutable reservation version with exact source IDs, start/end UTC milliseconds, selection time, earliest permitted collection start, metadata-only selection evidence, custodian, access log, coverage requirements and stopping rule. Selection must strictly precede both collection and the window. Collection-start evidence must later reconcile with the preregistration; an already-running/debugged stream is not a fresh source merely because its output directory changes. No trailing band chosen after inspecting economics. Those fields/evidence are unresolved now; this task does not schedule capture or invent them.
4. Primary economics uses contiguous natural-prevalence sessions. No balanced/reweighted primary evaluation selected using future action labels. A separate class-diagnostic challenge pool addresses v6; it cannot masquerade as a natural economic test. Strict unseen-entity challenges and ordinary chronological deployment are separate claims. Development calibration-validation/validation IDs and challenge groups remain unresolved; no random default fractions or seed are introduced here.
5. For strict unseen-entity challenges, form connected components of verified mint/episode/content-version/duplicate/repair/retrieval dependencies before partitioning. Register exact component membership and chosen challenge IDs outcome-blind in the next version. A giant component is a measured coverage limitation, not permission to split it or relabel protected parents. Group materialization and whole-corpus anti-join counts are future work, not implemented by this small boundary API.

## Deterministic enforcement

`src/north_star/splits.py` takes metadata, not outcomes. `assign` first requires an exact registered source assignment or an applicable `[start,end)` absolute UTC source window, then applies exact entity restrictions. A known entity cannot admit an unknown source or a window-only source outside its window. It returns the sole unanimous applicable partition, otherwise QUARANTINED. Unknown/unavailable/relative-only chronology and malformed identifiers quarantine. There is no outcome argument. Verified relative ordering may still support isolated local accounting/test fixtures, never invented global timestamps.

Policy construction validates nested object/list/scalar types, including exact string identifier lists. A pending policy cannot contain protected source/entity assignments, materialized protected IDs or selected windows. It may retain existing DEVELOPMENT/EXPOSED assignments. The checked-in v2 pending policy corrects the source inventory only; enforcement semantics and unresolved reservation fields are unchanged.

`inherit` must receive the **complete transitive parent closure**, including every candidate (not just the selected mint), open position/portfolio episode, narrative version, duplicate family, retrieval source, CPT/SFT/retention ancestor and economic evidence. Any unknown, quarantined or conflicting parent poisons the context. Shared reference exceptions require exact IDs on a frozen timeless permissive-reference allowlist; the current list is empty. An empirical protected parent cannot be exempted by its name. Missing dependency discovery cannot be solved by this helper: producers must enumerate and independently audit it.

`assert_before_fit` is a mandatory precondition for teacher selection, feature/cluster fitting, execution calibration, curation, retrieval fitting and CPT/SFT/retention exports. It refuses a pending reservation, non-prior reservation timestamp, unknown stage or non-development parent closure. It is a library boundary, **not yet wired into all legacy producers**. No fitting, curation, model training or exports were performed in this tranche.

`training_interval_allowed` separately requires a valid training start through label end strictly before the earliest protected window start minus the maximum preregistered label-horizon, episode, feature and retrieval duration. Touching boundaries, gaps between protected windows, and all post-holdout training are rejected. Any unknown duration blocks chronological acceptance, not fixtures. All four production durations remain null. Callers must use both parent and interval checks; neither replaces the other or point-in-time availability validation.

## Immutability and contamination custody

Canonical SHA256: UTF-8 JSON, keys sorted, compact separators, `ensure_ascii=False`, `allow_nan=False`; omit the self-describing `policy_sha256` field from its input. Use `north_star.splits.canonical_sha256` on that stripped policy, as the checked-in policy test does; this worktree has no `policy_hash` symbol. Current policy v2 pin:

`2e192cc90ccc331202f511667a58aefef8ed9326d11f1a502cd928191378aeaa`

Superseded v1 pin, retained as history rather than silently overwritten:

`de8479ca8a255f892b71332d55ab1182e76de8e57fdf048195b50b0a25e14137`

This outcome-blind metadata correction explicitly supersedes `north_star_eval_policy_v1` with `north_star_eval_policy_v2`. Per the bounded task context and prior receipt, v1 had not been exercised for fitting, training or evaluation artifacts; reservation was pending. No fit artifact invalidation or replacement assessment window is applicable. v1 is not the pin for future use; v2 also remains pending and blocks fitting. The JSON records the prior version/hash and reason in `supersession`.

### Corrected exposure inventory and source evidence

- Historical Megga VOD: `https://www.twitch.tv/videos/2866050335`, matching `v2866050335` in `STAGE2_TWITCH_SPIKE_EVAL.md` and the operator's exact locator.
- Current Megga stream: `urn:twitch:stream:320254509148`, derived from original `D:/mev_bot-artifacts/narrative/twitch_live/CAPTURE_CONTEXT.json` fields `twitch_channel=https://www.twitch.tv/megga` and `stream_id=320254509148`.
- Current on-chain acquisition: `urn:laserstream:session:20260909_144906_000490`, derived from that same context's `laserstream_session`. Both current acquisitions are DEVELOPMENT/EXPOSED, not new untouched sources.
- Original context byte SHA256: `76d3a4fb60472bc281e5069dca3b07b1f8e236a9c711d29ebaf68963bce5f74b`. Context launch time is not a media epoch, verified co-temporality, wallet attribution, finalized coverage or rights evidence. No wallet/entity assignments or windows were introduced.
- The URNs are local exact source identifiers, not provider-resolvable links. `https://www.twitch.tv/megga#stream=320254509148` is an invented fragment identifier only, not a Twitch archive URL; it is documented here but not assigned as an additional source. Unknown aliases still quarantine until explicitly registered with provenance.
- Retained `https://www.youtube.com/watch?v=hxXZTE9wWdA` as a **separate** exposed source: direct YouTube oEmbed metadata confirmed `author_name=Megga`, `author_url=https://www.youtube.com/@Megga.` and the title listed in master v5 S3. Thus the conditional removal criterion (no source proof) was not met. This video is neither the historical Twitch VOD nor the current Twitch stream; retaining it does not imply source admission or speaker/rights certification. No media, transcripts or outcomes were read for this correction.

All three legacy source locators remain unchanged. The seven entries are a partial exposure ledger, not an exhaustive certified inventory. No unknown source receives an assignment and no protected reservation is claimed.

Before hashing, reject non-string JSON object keys at every nesting depth and Python-only container types; JSON coercion must not equate numeric keys with canonical string IDs. The constructor compares to a **separately trusted** pin and deep-copies its input. The public `sha256` property is read-only. These are API integrity protections, not a sandbox against callers deliberately modifying private Python internals. A digest adjacent to mutable JSON detects accidental edits only; it is not a signature or external custody. Pin this version in the reviewed execution manifest before use. Persist immutable config/artifact receipts from `bind_artifact`; compare actual file hashes through `verify_artifact` before reuse. The receipt binds policy, stage, fit time, parent-closure digest, artifact and configuration. Authentication of receipts and source timestamps remains an integration requirement.

Never overwrite an exercised policy/artifact version. Changes to source/entity/window membership, semantics, horizons, labels, baseline, cost/latency model, weights, prompts, candidate generator, features, retrieval/ranking or discretionary risk settings require a new version and documented invalidation. Material performance-affecting changes require a new assessment window. No residual-driven repair or checkpoint selection on sealed results. Freeze economic endpoints, baselines, uncertainty/sample sufficiency and stopping rule before assessment; these remain unresolved, not defaults.

Legacy CPT/SFT/retention/retrieval membership and pretrained outcome knowledge are incompletely audited. Identity masking is diagnostic, not decontamination. A post-model/system-freeze prospective shadow window is an additional future gate, not the currently unselected acquisition window. Shadow is not landed PnL; live orders need separate operator authorization.

## Completion gaps

Remaining: formal pre-collection reservation and custody; exhaustive exposure/ancestry registry and source hashes; development/challenge component memberships; whole-corpus overlap/giant-component/quarantine counts; producer precondition integration; measured/approved embargo and economic limits; independent audit. Passing synthetic fixtures proves software branches, not source rights, dataset coverage, protected custody, economic performance or completed freeze of unknown IDs.
