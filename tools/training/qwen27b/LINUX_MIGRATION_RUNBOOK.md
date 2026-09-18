# Native Qwen27B launcher — blocked until independent approval

No training is authorized by this file. Historical WSL commands and old checkpoints
are audit material only; originals are preserved under `audit_legacy_do_not_run/`.
Do not run historical scripts to install, mount, migrate, clean, or launch.

## Read-only entry point

`bootstrap_linux.sh` is a preflight wrapper only: no installation, filesystem change,
mount, reboot, checkpoint selection, or automatic launch. All launcher wrappers require
explicit `--release_manifest`, `--launch-contract`, and `--contract-sha256`; launch
wrappers fix CPT/SFT, and resume wrappers additionally require an explicit resume object.
Default and `--dry-run` never spawn training or compile AIO. `--execute` is a separate,
operator-authorized action, not permission granted by a passing unit test.

## Immutable identities

Release and launch contract use SHA256-pinned absolute paths. Code (including launcher,
trainer and shared release validator), all data/tokenizer bindings, complete seed files,
DeepSpeed config, seed receipt and resume identity must match. The shared trainer
admission validator checks independently supplied `QWEN_RELEASE_ACCEPTANCE_SHA256`,
exact release payload, typed gate receipts and their population/runtime/subject bindings.
Candidate source-support releases are loader/audit inputs only, never trading approval.

Fresh CPT requires a verified local clean upstream seed. Fresh SFT requires selected
new CPT weights from the same release, best-internal-validation selection and parent
run lineage; old CPT/SFT weights cannot substitute. Run directories are fresh and unique.
Resume requires an explicitly pinned complete same-run, same-release checkpoint with
model, three-rank optimizer, scheduler and RNG state; no latest-checkpoint discovery.
Native DeepSpeed checkpoint layout/restoration still requires native acceptance.

Launcher supplies trainer `--launch_contract`, `--contract_sha256`, `--init_from`,
`--out_root` and explicit `--resume_from_checkpoint` when requested. Execute first
runs actual trainer `--preflight_only` with that argv, then loader `--dry_run`, then
AIO load. A boolean trainer-verification receipt does not replace executable checks.
Missing/incompatible trainer interfaces fail closed. Neither probe loads model weights.

## Native host requirements

- Native Linux only; WSL, `/dev/dxg`, wrong/busy GPUs or an existing trainer block launch.
- Exactly three pinned GPU UUIDs with at least 95000 MiB each; batch 24 / microbatch 1 /
  accumulation 8, ZeRO-3, BF16, NVMe optimizer offload, no parameter offload or gathered save.
- `/training` (or explicit contract mount) must be a writable ext4 filesystem, FSROOT `/`,
  matching UUID, no nested mounts and unambiguous physical disk -> partition ancestry.
- Training disk serial is fixed to operator-pinned `25375338A05C` (Windows/Linux).
  `253753246786` is the Data disk and is never a training target. Device names are not identity.
- Available RAM must cover the declared training budget (minimum 128000000000 bytes)
  **plus ceil(12% of MemTotal)** left reserved. This is a preflight constraint, not a
  measured peak-memory guarantee. Free training disk floor is 1500000000000 bytes.
- Global lock, all identities and capacity are checked again after loader/AIO probes.

## Verification status

Windows subprocess tests validate failure paths and dispatch using labeled fixtures.
They do not certify native GPU/NVMe/AIO, distributed loss, full-parameter training,
checkpoint restore, or any release approval. See `launcher_report.json` in the repair
artifact directory for hashes, test outputs and remaining blockers. No installation,
mounting, reboot, deletion, training or approval is part of this repair.
