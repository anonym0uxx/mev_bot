# Legacy Slinky economics: bounded quarantine audit

**Result: economic-field quarantine, not a rebuilt dataset.** Registry: [SLINKY_ECONOMICS_QUARANTINE.json](SLINKY_ECONOMICS_QUARANTINE.json). Durable external evidence: `D:/mev_bot-artifacts/north_star/development/slinky_economics_audit_v1/EVIDENCE_INDEX.json`.

No original files mutated/deleted, no label rewrites, no training positives, fitting, source admission, model calls, protected future data or commits. The registry is declarative: **loader enforcement is not implemented by this task**. Other stage owners must integrate its restrictions without claiming this report alone blocks a loader.

## Bounded evidence and counts

- Predetermined first 128 rows of row group 0 in compact part0000 of counterfactual, state and outcome layers; **3 parquet opens, 384 logical records, 128 joined state IDs across 5 mints**. The existing EVAL_FREEZE.json marks the source DEVELOPMENT/EXPOSED. No return-conditioned search or population scan.
- All three part0000 footers report 251,831 rows / 134 row groups; row group 0 is 1,401 rows. Arrow may decompress column pages beyond the 128 requested logical records. Only one batch per file was decoded; saved samples are used for every repeat.
- **115 eligible rows / 575 size scenarios reproduced**, 45 rows with recorded exit reserves and 70 with entry-reserve fallback. 460 nonbenchmark scenarios use mismatched token quantities. All 575 surface scenarios and 115 benchmark exact-net rows omit configured tips. 45 nonnull barrier-net rows instead reconcile tip-inclusive revenue minus cost.
- **7,432 executable assertions pass; zero failures.** These include retained surface fields, joined identities/code hash, integer-bp formula and both accounting branches. Exact defect prevalence, independent trades and affected downstream target totals remain **unknown**.
- Manifest-only inventory: **12,460 original counterfactual shard entries / 33,581,765 claimed rows** and **134 compact counterfactual entries / the same 33,581,765 claimed rows**. These are alternative representations, not additive rows. 133 compact files are in the artifact store; part0133 remains at `D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3_compact/counterfactual_trade_v3/counterfactual_trade_v3_compact_part0133.parquet`. All 134 are located with claimed byte sizes; no full-file parquet digests were recomputed. Exact per-file paths, claimed digests/counts and location checks are in `artifact_inventory.json`.
- Other compact layers are inventoried to trace joins/derivatives, not declared defective wholesale. Original shard existence/digests were inherited from `disk_manifest_v3.json`, not individually reverified. Footer digests identify sampled metadata only, **not content hashes**.

## Exact mechanism and testable real-row arithmetic

Pinned original `src/build_slinky_gold_v3.py` SHA256:
`db2530be539a1cfb03f01aa096e74a88d4eb920c458b0e13e029e25379722034`.

1. Lines 1531–1537 compute 0.5 SOL benchmark entry tokens. Lines 1555–1559 pass that single quantity to every size. `compute_multi_size_economics` line 1040 chooses this nonnull override instead of each size's `tokens_micro`; lines 1042–1048 sell the benchmark position and subtract each scenario's smaller/larger cost. Thus sz005/sz010/sz025 sell too many tokens; sz100 sells too few. sz050 is the matched-quantity control.
2. Entry helper returns tip-inclusive `total_cost` (985), sell helper tip-inclusive `net_rev` (1007), but surface net (1047) and exact benchmark net (1549) ignore those totals. Configured tips are 10,000 lamports per leg: 20,000 lamports = 0.00002 SOL omitted per round trip. Quantity/return error is distinct from this smaller accounting error.
3. Missing exit reserves fall back to entry reserves (1038–1039,1540–1541), yet a nonnull mathematical quote marks every size feasible. Seventy reproduced rows have missing exit reserves; they remain censored, not successful exits.

Observed-exit example, state `f858c631ec64b72f`, sz005 (0.05 SOL):

```text
owned entry tokens = 834307.424837375
benchmark tokens sold = 8258510.4265145
stored exit SOL = 0.40534116406604004
legacy net = exit - 0.05 - 0.0005 - (exit * 0.01)
           = 0.35078775242537963 SOL
stored return bp = int(legacy_net / 0.05 * 10000) = 70157
quote selling OWN tokens at the SAME recorded exit reserves = 0.041326097837554934 SOL
same-reserve own-quantity net, including both configured tips
           = -0.009607163140820628 SOL
```

The saved sample includes real entry and exit reserves; the isolated original helpers reproduce stored quantities and returns. **The own-quantity calculation is a diagnostic substitution, not a new label, calibrated execution, realized PnL or proof of how the path would evolve.** Ninety observed-exit size scenarios change positive→nonpositive under this diagnostic, not ninety proven bad fills/training labels.

Censored example `5328b2f924664eed`: sz005 reports 0.4295505522226798 SOL net / 85,910 bp and feasible=true, while economic_class=SKIP, exit_feasible=false, observed_through_300s=false. Its computed compressed mean return is 2.5424 (the misleadingly named `median_net_return_pct` actually takes an arithmetic mean). This is not a fabricated new positive: every legacy value is preserved and excluded from economic admission.

**Do not overattribute:** barrier-based `net_pnl_sol` line 1652 explicitly subtracts both tips and slippage; economic_class at 1663–1688 uses that barrier return, not the sz* surface. `policy_eval_v3` uses barrier net/class. Thus the manifest STRONG/GOOD/MISSED_OPPORTUNITY totals are retained as untrusted prior claims, not asserted to be precisely caused by this quantity bug or “corrected” by this audit. Entry-only features and objective outcomes are not globally invalidated or admitted here.

## Producing-code and artifact linkage

- Producer manifest identifies `pipeline_version=3.1.0`, `run_uuid=ef113bd1-f5e`, `source_hash=42132c2533b9effd`, `execution_config_hash=89207735e3c0f866`, `code_config_hash=af87ffefd9988a42`. Recomputing its exact script+schema+config recipe yields **af87ffefd9988a42**. Every sampled state carries the same code/run provenance and joins the sampled counterfactual/outcome identity.
- Manifest Git literal `fd43b8f1e9e8` is preserved, but producer/schema/wrapper paths are absent from that tree. This is a real lineage gap, not repaired by picking another commit. The matching hash is truncated to 64 bits and is a self-reported execution linkage; it does not pin wrappers, compactor or all runtime inputs. Full present source/schema digests and copies are saved.
- Compaction manifest names the original Slinky output as source; inspected compactor streams/casts/writes without recalculating economics. Its producing code hash is absent. Sample arithmetic directly demonstrates the defect in the compact output regardless of that missing compactor build pin. Relocation inventory preserves original/store locations; it is not fresh cryptographic content verification.

## Corrected builders: separate the claims

`rebuild_v3_corrected.py` SHA256 `2495f25e405b60122c43c4849ed3e7543aef070cbce77b91606d94292bacdd91` is a **LaserStream**, not Slinky, rebuild. It imports `build_laserstream_gold_v3`, reads a Phase 2 checkpoint and writes the LaserStream family. Manifest `manifest_laserstream_gold_v3.json` identifies raw_zst_capture2, run `bbaeb991-0834-4b53-8ca3-90dbb6db0a11`, execution version `lsv3-corrected-1` and 94,771,750 inherited L3 scenarios.

Its scenario loop computes entry tokens per size and feeds those tokens to sell quotes: **the inspected code does not share the Slinky benchmark-quantity override**. It reports `sim_pnl=exit_sol-size_sol`; fees are handled by quote helpers, but explicit tips/calibrated fills are not established. Do not transfer Slinky's 20,000-lamport settings to LS. The manifest lacks a producing code hash and Git object `4e97b4b13238` is unavailable. Exact current-code→output identity is **UNKNOWN**. No LaserStream rows were opened and no admission/clean-economics certification is granted.

Separately, downstream `_bp→fraction` and `sft_cross_exporter_v2` changes are not AMM repairs. `build_eval_v1_2.py:133–175` consumes the Slinky surface through `compute_slinky_cf_compressed`; `build_qwen_curriculum_v1.py:341–403,963–1031` computes compressed economics→utility→rank. `sft_cross_exporter_v2.py:269–285,378–390` still uses utility/feasibility in target gates and emits inherited scores/ranks. Source snapshots are pinned, but producing code hashes for those historical outputs are missing; historical pct-reader behavior must not be inferred from today's source alone.

Manifest-linked exposure inventory includes eval v1.2 (279 claimed records, 235 Slinky panels), SFT v2 (2,162 records, 1,050 cross records; Slinky-only count unknown), CPT v1 (7,600 records; 2,922 Slinky trajectories, 1,200 panels and 5 regime catalogs claimed), prior eval/SFT and candidate manifests. **Zero downstream rows inspected/relabelled; exact invalid targets unknown.** V2 training approval names the actual Slinky run/source/freeze. Short literal `cf97c33` provenance in other curriculum manifests is retained as an unresolved mismatch, not equated to the Slinky run UUID. Rust, retention and unrelated narrative content are not declared economically defective by association.

## Precise task consequences / outstanding work

- **S4.1 / D04 / D12:** size/profit/capacity surfaces and exact net cannot establish calibrated execution support. Arithmetic self-tests are not independent residual validation.
- **S5.1:** quarantine dependent recommended B-track action/size/utility targets; preserve observed A actions and uncertainty. No new BUY/SELL/HOLD examples are produced.
- **S5.2:** inherited classes/row totals do not establish independent economic positives, class support or distribution sufficiency.
- **S6.1–S6.2:** require transitive ancestry quarantine and exact producer pins; current registry is a durable declaration, not loader implementation.
- **S7.1–S7.2 / S8:** old GOLD/CERTIFIED/APPROVED labels are historical facts, not economic acceptance. Independent review, corrected immutable lineage and actual loader enforcement remain outstanding.

No BUILD_PLAN, master, stage tracker, integrated pipeline or other owners' files changed. Registry contains field-specific scope, exact claims, inherited-vs-measured counts, evidence references and release conditions. Full-corpus prevalence, all-original hashes and model/checkpoint ancestry are intentionally unresolved under this bounded task.

## Reproduction and verification

```bash
python D:/mev_bot-artifacts/north_star/development/slinky_economics_audit_v1/reproduce.py
```

This reruns AST-isolated pure functions against saved development samples; no new parquet reads. `audit.py metadata` regenerates bounded metadata inventory; `audit.py sample` requests one 128-row batch per three files (do not run unnecessarily). `original_integrity_check.json` verifies all pinned source/manifest bytes unchanged. `EVIDENCE_INDEX.json` pins every external evidence file; `verification.json` pins the two final worktree deliverables and records validation outcomes.
