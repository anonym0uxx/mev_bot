# DeepSpeed Handoff — Qwen3.8-27B North Star Trading Brain (CPT + SFT, NATIVE Linux)

Status: **HANDOFF DOCUMENT ONLY.** This file documents an already-hardened trainer +
launcher. It is not approval, not a launch, and not evidence of native acceptance.

Authoritative code references (read to produce this document; every flag, requirement
and refusal below is taken from the code, not invented):

- `tools/training/qwen27b/launch_native.py` — native launcher (`validate_contract`, `validate_host`, `validate_shared`, `execute`, `runtime_probe`, `main`)
- `tools/training/qwen27b/train_qwen27b.py` — trainer (`main`, `step_plan`, `WeightedLossTrainer`)
- `tools/training/qwen27b/release_data.py` — shared admission/seed/request validator (`verify_admission`, `verify_seed`, `verify_training_request`, `load_release`, `exact_inventory`, `record_groups`)
- `tools/training/qwen27b/verify_clean_seed.py` — raw-seed independent re-derivation
- `.../astra_review_v1/repair_v2/`: `RELEASE_SCHEMA.json`, `RELEASE_COMPATIBILITY.md`, `launcher_contract.md`, `CANDIDATE_RELEASE.json`, `CPT_LOADER_DRYRUN.json`, `SFT_LOADER_DRYRUN.json`, `launcher_report.json`, `HOST_READINESS.json`, `TRAINER_INTERFACE.json`

---

## 1. Purpose and scope

This document is the single authoritative handoff for running **full-parameter BF16
DeepSpeed ZeRO-3 training** of Qwen3.8-27B for the North Star trading brain on a
**native Linux (dual-boot Ubuntu) host**, in two phases:

- **Phase 1 (CPT)** — factual continued pre-training from the clean raw upstream base.
- **Phase 2 (SFT)** — supervised instruction/attribution fine-tuning initialized from the
  **selected Phase 1 CPT weights**.

Both phases run **natively on Linux** (never WSL, never `/dev/dxg`). The launcher
enforces native Linux in `collect_host()` and re-asserts it in `validate_host()`:
`host['system'] == 'Linux'`, `release` must not match `microsoft|wsl`, and `/dev/dxg`
must not exist.

## 2. The one-model North Star goal

Train **one** trader model end to end: a single Qwen3.8-27B whose CPT phase supplies
factual market/episode grounding and whose SFT phase supplies the supervision geometry
for observed-action attribution. CPT and SFT are phases of the same model lineage, not
two models. SFT must initialize from the **selected** CPT weights of the **same release**
(not from any earlier checkpoint), with a **fresh optimizer/scheduler** (structural
guarantee: `train_qwen27b.py` loads model weights only via `--init_from`; the SFT run is
a separate process).

Selection rule (both phases, per the trainer docstring): the selected checkpoint is the
one with the **strongest internal validation**, via `load_best_model_at_end` +
`metric_for_best_model="eval_loss"`, never automatically the last. Internal validation is
~2.5% held out, **group-disjoint** (mint / trajectory / repair-chain / source-doc), and is
**NOT** the frozen `qwen_eval_v1.1`. Full history is appended to `<out_dir>/val_history.jsonl`.

## 3. Exact phase order

1. **Phase 1 — CPT**, native Linux. Clean raw upstream seed only.
2. **Phase 2 — SFT**, native Linux. Selected Phase 1 CPT weights only.

Phase 2 must not start until Phase 1 has a completed, verification-receipted parent run
and a selected, consolidated CPT weight directory (see §9).

## 4. Verified raw seed identity (what must be proven before CPT)

The CPT seed is the **clean RAW upstream** Qwen snapshot. It is identified by **path,
revision, shard inventory, index size and tokenizer digests** — never by path alone.
`verify_clean_seed.py` re-derives all of this on the Linux host and exits non-zero if any
check fails. A path check is explicitly not treated as evidence.

- **Repo:** `unsloth/Qwen3.8-27B`
- **Revision:** `3ea932cee0a432ae86e9c7826cbe8aef52323a28`
- **Linux snapshot path (shape):**
  `/home/alon/.cache/huggingface/hub/models--unsloth--Qwen3.8-27B/snapshots/3ea932cee0a432ae86e9c7826cbe8aef52323a28`
- **Shards:** 18/18 `model-*-of-00018.safetensors` present, plus `model.safetensors.index.json`
- **Index metadata `total_size`:** `55562855904`
- **Sum of shard bytes:** `55563006776` (`+150872` vs the index = safetensors header overhead; `verify_clean_seed.py` requires `actual >= declared`)

### 4.1 Confirmed tokenizer / template / config digests (sha256)

| file | sha256 |
|---|---|
| `tokenizer.json` | `0997f410c57a1f4e53b09e4be8f4a172d90edd9564368fb0847030937229b9f3` |
| `tokenizer_config.json` | `2e6eac2825dcd97362f8910ca75f6cb0405dd142e732386eb725af57926c91e5` |
| `config.json` | `191e0af232104ed8b65258cf3fb2b842e288008baca7633c11b82a1ac7203aab` |
| `chat_template.jinja` | `12827f24b742ea4e80cdc12dbcf9622227056b9f797252a3149263d4f9aaadce` |
| `vocab.json` | `ce99b4cb2983d118806ce0a8b777a35b093e2000a503ebde25853284c9dfa003` |
| `merges.txt` | `a9d356d7bdf1ef4949e3e748e95b8e10ad9d4e2e838eddc38a0a7b6b94d1db8d` |

Per-shard sha256 hashes are also captured into the receipt by
`verify_clean_seed.py --out <receipt.json>` (`receipt['shard_sha256']`), together with
`shard_bytes_total`, `index_total_size`, `index_sha256` and `shard_inventory_complete`.

### 4.2 Raw-seed verification command (read-only)

```bash
python -B verify_clean_seed.py \
  --seed /home/alon/.cache/huggingface/hub/models--unsloth--Qwen3.8-27B/snapshots/3ea932cee0a432ae86e9c7826cbe8aef52323a28 \
  --prior-checkpoint "/mnt/d/linux_export/checkpoint/cpt_final_bf16" \
  --prior-checkpoint "<ABS_PATH_OLD_WSL_SFT_CHECKPOINT>" \
  --out /training/release/clean_seed_receipt.json
```

Exit `0` = raw base proven; exit `2` = contaminated. `clean_seed_receipt.json` is what the
CPT launch contract pins as `seed.verification`.

## 5. The prior bad-data lineage is REFUSED (the guarantee)

Two lineages must never be used and must never be the seed:

- Legacy CPT checkpoint: `D:/linux_export/checkpoint/cpt_final_bf16` (Windows-side path).
- The old WSL SFT checkpoint.

Guarantees, enforced by code (not by convention):

- `verify_clean_seed.py` refuses any seed whose path contains the forbidden markers
  `cpt_final_bf16`, `qwen_sft_v2`, `qwen_sft_v1`, `sft_final`, and refuses a seed that
  resolves to, or nests inside, a declared `--prior-checkpoint`.
- `release_data.verify_seed()` requires, for **CPT**, exactly
  `seed['kind'] == 'clean_upstream'` **and** `seed['verified_clean'] is True` **and**
  `seed['repo'] == 'unsloth/Qwen3.8-27B'` **and** `seed['revision'] == '3ea932cee0a432ae86e9c7826cbe8aef52323a28'`.
  Anything else raises `Wrong clean upstream seed identity`.
- `release_data.verify_seed()` requires, for **SFT**, exactly
  `seed['kind'] == 'selected_new_cpt'` with `release_id`/`release_sha256` of **this same
  release**, `selection == 'best_internal_validation'`, a non-empty `run_id`, and a
  `parent_run` binding whose receipt has `phase=='cpt'`, `completed is True`,
  `clean_upstream is True`, and matching `run_id`/`release_id`/`release_sha256`.
  Anything else raises `Selected NEW CPT lineage required`.
- `launch_native.validate_contract()` independently re-checks the same CPT/SFT seed kinds
  and, for SFT, `seed['run_id'] != c['run_id']` (SFT cannot reuse the parent run identity).

Old CPT/SFT weights therefore cannot substitute for either seed.

## 6. Hardware and host prerequisites checklist (native Ubuntu boot)

### 6.0 Decision basis — native dual-boot Ubuntu, NOT WSL

This is a decision, not a preference. Three independent reasons, each verified on
this machine:

1. **The hardened launcher hard-blocks WSL.** `launch_native.validate_host()`
   refuses any kernel release matching `microsoft|wsl` and refuses a host where
   `/dev/dxg` exists. Running the real launch under WSL would require defeating
   that guard, which is the exact class of bypass this repair exists to prevent.

2. **The previous WSL training filesystem sat on the wrong disk.** Verified Windows
   layout:

   - Disk **0** = `CT4000T710SSD8`, serial `...25375338A05C`, 3726 GB — the
     operator-pinned training disk.
   - Disk **1** = `CT4000T710SSD8`, serial `...253753246786`, 3726 GB = `D:` Data,
     with only **199.5 GB free**.

   The old WSL training volume was `D:\wsl\training_ext4.vhdx`, i.e. on the Data
   disk the contract states "is never a training target". Even if WSL were
   permitted, that setup fails the disk-identity gate — and `D:` free space is far
   below the required 1 500 000 000 000-byte floor.

3. **Native Disk 0 already has the partitions this needs.** Verified layout:

   - P1 EFI System 0.2 GB · P2 MSR 0.02 GB · P3 Windows `C:` 1676 GB ·
     P4 Recovery 0.86 GB · P5 `F:` 4 GB
   - P6 **100 GB**, GPT type `0fc63daf-8483-4772-8e79-3d69d8477de4` (Linux filesystem)
   - P7 **32 GB**, GPT type `0657fd6d-a4ab-43c4-84e5-0933c84b4f4f` (Linux swap)
   - P8 **1912 GB**, GPT type `0fc63daf-8483-4772-8e79-3d69d8477de4` (Linux filesystem)
     → the natural `/training` candidate, comfortably above the 1.5 TB floor.

The hardware was already partitioned for native training. WSL was the detour.

Two consequences to carry into the checklist below and into §10:

- **The raw seed is not on the native filesystem.** The verified 18-shard raw
  base lives in the WSL VHDX at
  `/home/alon/.cache/huggingface/hub/models--unsloth--Qwen3.8-27B/snapshots/3ea932cee0a432ae86e9c7826cbe8aef52323a28`.
  That path does not exist on the native boot. The native host must already hold
  an equivalent copy, or receive a verified transfer, and `verify_clean_seed.py`
  must be run there to prove it before any launch.
- **The verified Python stack is the WSL venv** `~/qwen-train`
  (torch 2.9.1+cu128, transformers 5.16.1, deepspeed 0.18.2). The native venv must
  be checked independently; `build_linux_release_manifest.py` records the host
  runtime it finds and refuses to guess.


From `launch_native.validate_host()` and `expected_native_disk` requirements. All of
these are **preflight constraints**, re-checked after the loader/AIO probes:

- [ ] Host is **native Ubuntu** (dual-boot). `platform.system()=='Linux'`, kernel release
      not matching `microsoft|wsl`, and `/dev/dxg` **absent**.
- [ ] **Exactly three** GPUs, UUIDs pinned in the contract (`gpu_uuids`, 3 distinct), host
      GPU set == contract set, and **each with `memory_mib >= 95000`** (RTX PRO 6000 ≥ 95000 MiB).
- [ ] No other trainer process and **no GPU compute processes** running
      (`nvidia-smi --query-compute-apps=pid` empty; `/proc` scan for `train_qwen27b.py` empty).
- [ ] `/training` present as a **writable ext4** filesystem, `FSROOT == /`, mount `uuid` ==
      contract `mount_uuid`, `rw` without `ro`, and **no nested mounts** under it.
- [ ] Training disk serial is **exactly** `25375338A05C`. The Data disk `253753246786` is
      **never** a training target. Device names are not identity. Disk→partition ancestry
      must be unambiguous (`['disk','part']`, one chain).
- [ ] RAM: `MemAvailable >= min_available_ram_bytes` (floor **128000000000 bytes**), and
      after reserving that budget the **remaining available must be ≥ ceil(12% of MemTotal)**
      (`reserve = (MemTotal*12+99)//100`). Budget is a preflight constraint, not a measured
      peak guarantee.
- [ ] Free training disk `>= 1500000000000` bytes (**≥ 1.5 TB**).
- [ ] Global lock path `/training/.qwen27b.launch.lock` is not a symlink; fresh run dir does
      **not** already exist.
- [ ] `deepspeed` **installed and importable in the native venv** (currently NOT installed
      in the CPU venv — see §10).

## 7. DeepSpeed configuration (native)

Pinned native config: `tools/training/qwen27b/ds_zero3_native.json` (contract `deepspeed`).
`validate_contract` refuses anything else:

- `zero_optimization.stage == 3`, **no** `offload_param`, `stage3_gather_16bit_weights_on_model_save == false`
- `offload_optimizer.device == 'nvme'`, `offload_optimizer.buffer_count >= 4`, `nvme_path` inside the training mount (`/training/offload`)
- `bf16.enabled == true`
- batch geometry exactly `(train_batch_size, train_micro_batch_size_per_gpu, gradient_accumulation_steps) == (24, 1, 8)`

The trainer hard-asserts at startup (`train_qwen27b.py`): `world_size == 3`,
`deepspeed_plugin.zero_stage == 3`, `average_tokens_across_devices == True`, and
`trainer.model_accepts_loss_kwargs` (global token-normalized loss actually active).
Launch uses `accelerate.commands.launch --use_deepspeed --num_processes 3`.

## 8. Executable contracts (what the launcher requires)

The launcher CLI requires **all** of `--phase`, `--release_manifest`, `--launch-contract`,
`--contract-sha256`. The launch contract is JSON matching the **real** schema enforced by
`validate_contract()`: `schema_version == 'qwen27b_launch_v2'`, `phase`, `run_id`
(`[A-Za-z0-9][A-Za-z0-9_.-]{0,95}`), `release_id`, `release_manifest {path,sha256}`,
`code_files[]` (every `*.py` in the trainer's directory, incl. `launch_native.py`,
`train_qwen27b.py`, `release_data.py`), `trainer {path,sha256}`, `deepspeed {path,sha256}`,
`training_mount`, `run_dir` (basename == `run_id`, inside `training_mount`), `gpu_uuids[3]`,
`min_gpu_memory_mib`, `min_available_ram_bytes`, `min_free_disk_bytes`, `mount_uuid`,
`disk_serial`, `seed {...}`, and optional `resume {...}`.

Templates written alongside this doc (placeholders for hashes/absolute paths):

- `LAUNCH_CONTRACT_CPT.template.json`
- `LAUNCH_CONTRACT_SFT.template.json`

The SFT template carries the `selected_new_cpt` lineage fields and `parent_run`.

### 8.1 Focused tokenizer note — pin the effective fingerprint

The **effective tokenizer fingerprint** all dryruns report is
`93214b2d6d9f99f2d152ce44b8dad5be7b5cdf610b96f261c458eacb7e6d54ae`
(`tokenizer_effective_sha256`; equals `release['manifest']['tokenizer']['effective_sha256']`).
`verify_tokenizer()` recomputes it from backend tokenizer state + chat template +
special tokens + padding/truncation sides + model_max_length and **refuses** on mismatch.
The dryrun JSONs (`training_authorized:false, model_loaded:false, training_launched:false`)
are loader/geometry evidence only — they never authorize training.

## 9. Step geometry (from the actual dryrun JSONs)

Source: `CPT_LOADER_DRYRUN.json`, `SFT_LOADER_DRYRUN.json` (release_id `astra_support_repair_v2`,
manifest_sha256 `3c19197147a6dc0ec9d9407d69035d2b5b2587c6b7c92c499d827e74514ac7bd`).

### Phase 1 — CPT

| quantity | value |
|---|---|
| train records | 3,576 |
| validation records | 402 |
| dataset_items (train / val) | 294 / 34 |
| raw_tokens (train) | 1,200,806 |
| shifted loss tokens (train / val) | 1,200,512 / 136,216 |
| tail_tokens (train / val) | 678 / 1,082 |
| steps_per_epoch | 13 |
| max_window_ep | 1.75 |
| total_steps_max | 23 |
| warmup_steps | 1 |
| mark_every | 3 |
| marks_steps | 3, 6, 9, 12, 15, 18, 21, 23 |
| marks_epochs | 0.230769 … 1.769231 |
| LR / warmup ratio / wd | 4e-6 / 0.04 / 0.10 |
| seq len, packing | 4096, EOS-packed |
| eval_on_start | true |

### Phase 2 — SFT

| quantity | value |
|---|---|
| train records | 3,999 |
| validation records | 384 |
| dataset_items (train / val) | 3,999 / 384 |
| raw_tokens (train / val) | 10,482,041 / 1,008,750 |
| supervised (shifted loss) tokens (train) | 1,020,695 |
| supervised (shifted loss) tokens (val) | 98,409 |
| steps_per_epoch | 167 |
| max_window_ep | 1.25 |
| total_steps_max | 209 |
| warmup_steps | 7 |
| mark_every | 41 |
| marks_steps | 41, 82, 123, 164, 205, 209 |
| marks_epochs | 0.245509 … 1.251497 |
| LR / warmup ratio / wd | 2e-6 / 0.03 / 0.01 |
| seq len, packing | ≤12288, UNPACKED, dynamic padding, prompts masked −100 |
| eval_on_start | false (dxg-RAM saving; native may keep false) |
| SFT task targets | `observed_action_attribution: 1.0` |

SFT supervision geometry (weights are train-only inverse semantic-token mass; **no
outcome/economics weighting**): `observed_action_attribution::EXIT_RECORDED` 0.5131,
`::ADD_RECORDED` 0.6567, `::REDUCE_RECORDED` 2.4866, `::BUY_RECORDED` 7.9333.

Note: `EXACT_STEP_PLAN.json` in the repo is an **older** plan (CPT 92/161, SFT 95/119) and
does not match the current post-holdout dryruns. Use the values above.

## 10. Known blockers (must be cleared before real training)

1. **Tokenizer path is a WINDOWS path (primary blocker).** `CANDIDATE_RELEASE.json` pins
   `tokenizer.path` to `C:/Users/Alon/.cache/huggingface/hub/models--unsloth--Qwen3.8-27B/snapshots/3ea932cee0a432ae86e9c7826cbe8aef52323a28`.
   That path does not exist on native Linux, so `load_release()` would raise at
   `sha256(snapshot/name)`. **A Linux-bound manifest rebuild is REQUIRED before real
   training**, using the same pinned revision and the Linux snapshot path
   (`/home/alon/.cache/huggingface/hub/models--unsloth--Qwen3.8-27B/snapshots/3ea932cee0a432ae86e9c7826cbe8aef52323a28`).
2. **Naive manifest rebuild silently changes the pinned file set.** The Windows manifest
   pins `preprocessor_config.json` and `video_preprocessor_config.json`, which are
   **absent on the Linux snapshot**. Rebuilding must pin only the **text-tokenization set
   that exists on BOTH sides**: `tokenizer.json`, `tokenizer_config.json`,
   `chat_template.jinja` (required by `load_release`) plus `config.json`, `vocab.json`,
   `merges.txt` as pinned by `verify_clean_seed.py`. Pinning a missing Linux file fails
   closed; **omitting** it must be a deliberate, reviewed change, not a silent side effect.
   The pinned revision, `template_kwargs {'enable_thinking': false}`,
   `mask_policy 'assistant_body_only_full_render_offsets_v1'` and the effective
   fingerprint `93214b2d…d54ae` must be preserved.
3. **The current candidate release cannot authorize training.** `CANDIDATE_RELEASE.json` is
   `status:"CANDIDATE"`, `training_authorized:false`, `scope:"source_support_only"`, all five
   admission gates `false`, no `approved_by`, no admission `evidence` binding. Real training
   requires `status:"APPROVED"`, `training_authorized:true`, `scope:"north_star_trader"`,
   `approved_by`, all gates `true`, a SHA256-bound `admission.evidence`, and an
   independently supplied `QWEN_RELEASE_ACCEPTANCE_SHA256` (env or contract arg). Candidate
   dryruns never authorize training.
4. **deepspeed is NOT installed in the CPU venv**, and the current host is **Windows**, so
   **native acceptance is UNVERIFIED**. All existing tests are Windows subprocess tests over
   labeled fixtures; they do not certify native GPU/NVMe/AIO, distributed loss,
   full-parameter training, or checkpoint restore.
5. **SFT depends on Phase 1 output that does not exist yet.** SFT requires a completed CPT
   run, an SFT seed receipt (`selected_new_cpt`), a selected & consolidated BF16 CPT weight
   directory (`seed.path`), and a `parent_run` receipt. None of these exist on Linux today.
6. **Resume scheduler layout must be reconciled** with native actual DeepSpeed checkpoint
   layout before resume is accepted (per `launcher_contract.md`): a resume needs
   `checkpoint-[N]/trainer_state.json` with matching `global_step`, three
   `*_optim_states.pt`, model states, `scheduler.pt`, and `rng_state_{0,1,2}.pth`.
7. **Loss/governance guards are asserted at runtime and will refuse to start** if unmet:
   world_size ≠ 3, ZeRO stage ≠ 3, `model_accepts_loss_kwargs` False, first GA-window
   weighted denominator non-finite/zero, −100/pad leaking into the CE numerator, or first
   logged loss non-finite/zero.

## 11. Exact command lines

Define once on the Linux host:

```
QDIR=/home/alon/qwen27b                              # deployed code dir
RELEASE=/training/release/RELEASE_MANIFEST.json      # APPROVED, Linux-bound
CPT_CONTRACT=/training/release/LAUNCH_CONTRACT_CPT.json
SFT_CONTRACT=/training/release/LAUNCH_CONTRACT_SFT.json
CPT_CSHA=$(sha256sum $CPT_CONTRACT | cut -d' ' -f1)
SFT_CSHA=$(sha256sum $SFT_CONTRACT | cut -d' ' -f1)
```

### Phase 1 — CPT preflight (read-only, no spawn)

```bash
cd $QDIR
python -B launch_native.py \
  --phase cpt \
  --release_manifest $RELEASE \
  --launch-contract $CPT_CONTRACT \
  --contract-sha256 $CPT_CSHA \
  --dry-run
```

### Phase 1 — CPT execute (operator-authorized spawn)

```bash
cd $QDIR
python -B launch_native.py \
  --phase cpt \
  --release_manifest $RELEASE \
  --launch-contract $CPT_CONTRACT \
  --contract-sha256 $CPT_CSHA \
  --execute
```

### Phase 2 — SFT preflight (read-only)

```bash
cd $QDIR
python -B launch_native.py \
  --phase sft \
  --release_manifest $RELEASE \
  --launch-contract $SFT_CONTRACT \
  --contract-sha256 $SFT_CSHA \
  --dry-run
```

### Phase 2 — SFT execute

```bash
cd $QDIR
python -B launch_native.py \
  --phase sft \
  --release_manifest $RELEASE \
  --launch-contract $SFT_CONTRACT \
  --contract-sha256 $SFT_CSHA \
  --execute
```

### Phase 2 — SFT resume (requires an explicit `resume` object in the contract)

```bash
cd $QDIR
python -B launch_native.py \
  --phase sft \
  --release_manifest $RELEASE \
  --launch-contract $SFT_CONTRACT \
  --contract-sha256 $SFT_CSHA \
  --resume \
  --execute
```

`--resume` requires `contract.resume` (see §12) and `--resume_from_checkpoint` must equal
the pinned checkpoint inside `run_dir/<release_id>_sft/`. The same shape works for CPT
resume with `--phase cpt` and the CPT contract. There is **no latest-checkpoint discovery**.

### Direct trainer preflight (already runs inside launcher `runtime_probe`; for transparency)

```bash
cd $QDIR
python -B train_qwen27b.py \
  --phase cpt \
  --release_manifest $RELEASE \
  --out_root <RUN_DIR_FROM_CONTRACT> \
  --init_from <SEED_PATH_FROM_CONTRACT> \
  --launch_contract $CPT_CONTRACT \
  --contract_sha256 $CPT_CSHA \
  --preflight_only
```

### The accelerate command the launcher generates (from `preflight()['command']`)

```
<python> -B -m accelerate.commands.launch \
  --use_deepspeed --num_processes 3 --num_machines 1 --machine_rank 0 \
  --deepspeed_config_file <DS_CONFIG_ABS> <TRAINER_ABS> \
  --phase <cpt|sft> --release_manifest <RELEASE_ABS> \
  --init_from <SEED_ABS> --out_root <RUN_DIR> \
  --launch_contract <CONTRACT_ABS> --contract_sha256 <CONTRACT_SHA>
```

Execute also: runs the trainer's real `--preflight_only` and `--dry_run` with that exact
argv, **then** loads the DeepSpeed AsyncIO op (`AsyncIOBuilder().load()`), **then** re-hashes
all identities and re-probes host capacity **before** spawning.

## 12. Resume object shape (contract)

```json
"resume": {
  "path": "/training/runs/<SFT_RUN_ID>/<release_id>_sft/checkpoint-<N>",
  "files": [ { "path": "<ABS>/...", "sha256": "<64hex>" }, ... ],
  "complete": true,
  "run_identity": { "path": "/training/runs/<SFT_RUN_ID>/run_identity.json", "sha256": "<64hex>" }
}
```

`files` must be the **exact** inventory of `checkpoint-<N>`; the checkpoint dir name must
match `checkpoint-[1-9][0-9]*`, `trainer_state.json` `global_step` must equal `N`, and the
three-rank optimizer + model + `scheduler.pt` + `rng_state_{0,1,2}.pth` set must be present.

## 13. What is NOT authorized (explicit)

This document authorizes **nothing**. Specifically, none of the following is permitted
without a separate, explicit operator directive and an APPROVED `north_star_trader` release:

- No training of either phase (no CPT, no SFT, no smoke training).
- No installing, upgrading or modifying `deepspeed`, torch, transformers, accelerate or CUDA.
- No rebooting, no mounting/formatting `/training`, no filesystem changes.
- No approving, editing or "fixing" the CANDIDATE release or its admission gates.
- No loading model weights, no consolidating checkpoints, no deleting checkpoints.
- No treating a passing dryrun, unit test, or this document as training approval.
- No using the legacy bad-data lineage (`cpt_final_bf16`, old WSL SFT) as a seed.

---

## 14. Verified evidence (what is already proven, as of 2026-09-11)

| claim | evidence | status |
|---|---|---|
| Raw upstream seed is intact and complete | `CLEAN_SEED_RECEIPT.json` — 18/18 shards, 55,563,006,776 bytes, index `total_size` 55,562,855,904, `raw_base_selected: true`, `failures: []`, exit 0 | **PROVEN** |
| Tokenizer/template identity matches the Windows-verified pin | tokenizer.json `0997f410…`, tokenizer_config.json `2e6eac28…`, config.json `191e0af2…`, chat_template.jinja `12827f24…`, vocab.json `ce99b4cb…`, merges.txt `a9d356d7…` — all matched inside Linux | **PROVEN** |
| Prior bad-data checkpoint is present but NOT the seed | `/mnt/d/linux_export/checkpoint/cpt_final_bf16` exists; receipt records it under `prior_checkpoints_present_but_not_selected`; launcher refuses `kind != clean_upstream` | **PROVEN (refusal)** |
| Linux-bound release manifest binds and verifies | `build_linux_release_manifest.py` → `CANDIDATE_RELEASE_LINUX.json`, `binding_failures: []`, `clean_seed: true` sourced from the receipt hash, exit 0 | **PROVEN** |
| Both phases load end-to-end on Linux | `CPT_LOADER_DRYRUN_LINUX.json` (3,576/402 records, 294 CPT blocks, total_steps_max 23) and `SFT_LOADER_DRYRUN_LINUX.json` (3,999/384 records, 1,020,695 supervised tokens, total_steps_max 209) — both exit 0, `model_loaded: false`, `training_launched: false` | **PROVEN (loader only)** |
| Tokenizer effective fingerprint is stable across hosts | `93214b2d6d9f99f2d152ce44b8dad5be7b5cdf610b96f261c458eacb7e6d54ae` on both Windows and Linux bindings | **PROVEN** |
| Class-balance gate (V6) | `datasets/north_star_v1/CLASS_BALANCE_AUDIT.json` | **FAIL — see below** |
| Native GPU/NVMe/AIO, distributed loss, checkpoint restore | none | **UNVERIFIED** |

### Outstanding blocker carried into this handoff: the V6 class-balance gate FAILS

`CLASS_BALANCE_AUDIT.json` verdict: **FAIL**. Failing checks:

- train `buy` — 3.1508% label share, below the 5% floor (126 genuine examples)
- validation `buy` — 4.4271% share **and** only 17 genuine examples, below the 100 floor
- validation `reduce` — 32 genuine examples, below the 100 floor

Zero duplicate prompt hashes were found in either split, so no padding is propping
these numbers up — they are simply the real observed distribution. Per V6 §2.3 the
remedy is **not** padding: it is upweighting the genuine minority records in the
loss (the trainer already equalises per-class supervised-token mass — `BUY_RECORDED`
weight 7.93 vs `EXIT_RECORDED` 0.51, each normalised to 255,173.75 supervised
tokens) plus expanding collection toward qualified/graduation candidates where buys
are more common.

Two separate points must not be conflated:

- The current candidate families are **source-attribution** tasks
  (`policy_supervision: false`). They are not BUY/SELL/SKIP/WATCH decision policy,
  so the V6 decision-class gate cannot be satisfied by them regardless of the mix.
- Loss upweighting does not create decision-policy supervision either; it only
  balances the classes that exist.

**Do not start SFT against a decision-policy objective until the V6 gate passes on
a genuine decision-label corpus.**

### 14.1 Verified constants to fill into both contracts

These were read from this machine and can be copied directly into the `FILL_*`
placeholders. Anything not listed here is genuinely unknown and must be measured
on the native boot.

- `gpu_uuids` (all three present and matching the launcher's expectations):
  - `GPU-6a6dc2e1-6763-3a07-4335-029c6f005d78`
  - `GPU-647392e7-6de8-1ac3-2a20-5913606f857e`
  - `GPU-46acf8c5-7725-7aee-7661-3523c5aafba7`
- `disk_serial`: `25375338A05C` (already correct in the templates — do not change)
- `min_gpu_memory_mib`: `95000`, `min_available_ram_bytes`: `128000000000`,
  `min_free_disk_bytes`: `1500000000000` (floors — the launcher rejects lowering them)
- tokenizer `effective_sha256`: `93214b2d6d9f99f2d152ce44b8dad5be7b5cdf610b96f261c458eacb7e6d54ae`
- Linux-bound release manifest already produced:
  `…/astra_review_v1/repair_v2/CANDIDATE_RELEASE_LINUX.json`
  (sha256 `a59f3a574bbeefd33e2c393d94ca833bde2aa55966b09b5b87bf08fa14477f6a` at build time —
  re-hash on the native host, since `--accept-host-runtime` bakes in the host runtime)
- `mount_uuid`: **unknown** — measure on the native boot with
  `findmnt -no UUID /training`
- `training_mount`: `/training` (requires P8, the 1912 GB Linux filesystem on Disk 0,
  formatted ext4 and mounted there — not yet verified)

Both contracts also need an APPROVED `north_star_trader` release before they can
pass: the launcher requires `status: APPROVED`, `training_authorized: true`,
`scope: north_star_trader`, `admission.approved_by`, `admission.clean_seed: true`,
and all five admission gates true. The current release is `CANDIDATE` /
`source_support_only` with every gate false, so it is correctly rejected — and
must stay that way until the V6 gate and the remaining evidence gates actually pass.

---

## 15. Decision-task SFT and the policy wall (this session)

### 15.1 The attribution SFT could not teach deciding

`observed_sft_*` put the answer inside its own prompt: 100% of 5,805 training records
carry `SOURCE ACTION LABEL: <action>` in the prompt and 100% of targets restate it
(`SOURCE ATTRIBUTION: <ACTION>_RECORDED`). Training on it teaches transcription.

### 15.2 The decision family

`decision_sft_{train,validation,protected_eval,quarantine}.jsonl` — built by
`astra_review_v1/build_decision_sft.py` as a pure transformation of the verified
support export. Prompt holds only strictly-prior observables; target gives a rationale
grounded in those observables, then a final `DECISION: <action>`.

56,264 records kept from 59,725 (3,461 withheld: 1,477 same-second contradictions,
1,984 redundant identical prompts). Verified by `verify_decision_sft.py`
(`status: verified`, 0 failures).

Emitted classes: BUY, ADD, REDUCE, EXIT — all clear V6 in train and in protected_eval.
SKIP and WATCH are **not sourced** and are not fabricated.

### 15.3 Binding

- `CANDIDATE_RELEASE.json` (`astra_north_star_v3`): cpt factual + sft = decision family.
- `CANDIDATE_RELEASE_CPT_ONLY.json`: cpt only; loads clean (3,638 / 408).

### 15.4 BLOCKER: two deliberate walls

1. `release_data.py:59` — `task_bucket` refuses every non-support task:
   `Policy tasks disabled until immutable per-record action/economic/causal evidence is implemented`
2. `release_data.py:383` — `require(scope != 'source_support_only' or task in SUPPORT_TASKS)`
   → `Policy in support release`

`load_release` validates every phase, so a policy SFT in the manifest blocks CPT too,
which is why the CPT-only manifest exists.

Diagnostic result: with **only** these two walls neutralised in memory, the full
manifest loads and passes every integrity check — frozen split, mint identity,
exclusion-union contamination, duplicate-prompt, source-group disjointness,
task_targets match. **The data is sound; the policy gate is the only blocker.**

Evidence against the wall's own requirement:

| requirement | status |
|---|---|
| immutable per-record action | present (signature, block second, strict pre-cutoff, movement, provenance hashes) |
| economic | **ABSENT** — no fills, costs, latency, capacity |
| causal | partial — strict pre-event cutoff, anchor and equal-second excluded |

Lifting either wall is an authority decision. Both are left closed.
