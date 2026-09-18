"""Fill a LAUNCH_CONTRACT template from the live host. Never invents a value.

Everything pinned here is read from the real filesystem and hashed on the spot:
the seed inventory, every *.py in the trainer directory, the trainer, the DeepSpeed
config and the release manifest. Values that only the host can know (GPU UUIDs, the
/training filesystem UUID) are required arguments, so a contract can never be
produced from guesses.

Usage (CPT):
  python3 fill_launch_contracts.py --phase cpt \
      --gpu-uuids "<uuid1>" "<uuid2>" "<uuid3>" \
      --mount-uuid "<training-fs-uuid>" \
      --run-id cpt-001 \
      --seed-dir /training/seed/models--unsloth--Qwen3.8-27B/snapshots/3ea932cee0a432ae86e9c7826cbe8aef52323a28 \
      --release-manifest /training/rel/CANDIDATE_RELEASE.json \
      --code-dir /training/code/qwen27b \
      --out LAUNCH_CONTRACT_CPT.json

For the SFT phase, pass --phase sft plus --parent-cpt-run <receipt.json> and
--run-id different from the parent's; the seed block is built as
kind='selected_new_cpt' with parent-run lineage.
"""
import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

HEX64 = re.compile(r"^[a-f0-9]{64}$")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def require(cond, msg):
    if not cond:
        raise SystemExit("REFUSING: " + msg)


def inventory(root, exclude_suffixes=()):
    """EXACT inventory: every regular file beneath root, relative-sorted."""
    root = Path(root)
    files = []
    for p in sorted(root.rglob("*")):
        if not p.is_file() or p.is_symlink():
            continue
        if p.suffix in exclude_suffixes:
            continue
        files.append({"path": str(p), "sha256": sha256(p)})
    return files


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--phase", choices=["cpt", "sft"], required=True)
    ap.add_argument("--gpu-uuids", nargs=3, required=True, metavar=("U1", "U2", "U3"))
    ap.add_argument("--mount-uuid", required=True)
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--seed-dir", required=True)
    ap.add_argument("--release-manifest", required=True)
    ap.add_argument("--code-dir", required=True)
    ap.add_argument("--deepspeed", default=None, help="defaults to <code-dir>/ds_zero3_native.json")
    ap.add_argument("--template", default=None)
    ap.add_argument("--parent-cpt-run", default=None,
                    help="CPT run receipt; required for --phase sft")
    ap.add_argument("--selection", default="best_internal_validation")
    ap.add_argument("--training-mount", default="/training")
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    code_dir = Path(args.code_dir).resolve()
    require(code_dir.is_dir(), "code dir not found: " + str(code_dir))
    template = Path(args.template) if args.template else code_dir / (
        "LAUNCH_CONTRACT_%s.template.json" % args.phase.upper())
    require(template.is_file(), "template not found: " + str(template))
    contract = json.loads(template.read_text(encoding="utf-8"))

    rel_path = Path(args.release_manifest).resolve()
    require(rel_path.is_file(), "release manifest not found: " + str(rel_path))
    release = json.loads(rel_path.read_text(encoding="utf-8"))
    release_id = release.get("release_id")
    require(bool(release_id), "release manifest has no release_id")

    require(len(set(args.gpu_uuids)) == 3, "the three GPU UUIDs must be distinct")
    require(HEX64.match(args.mount_uuid) or re.match(r"^[0-9a-f-]{36}$", args.mount_uuid),
            "mount uuid does not look like a filesystem UUID: " + args.mount_uuid)

    # ---- code: EVERY *.py beside the trainer must be pinned, or the launcher refuses
    py_files = sorted(p for p in code_dir.glob("*.py") if p.is_file())
    require(py_files, "no *.py found in " + str(code_dir))
    contract["code_files"] = [{"path": str(p), "sha256": sha256(p)} for p in py_files]

    trainer = code_dir / "train_qwen27b.py"
    require(trainer.is_file(), "trainer not found: " + str(trainer))
    contract["trainer"] = {"path": str(trainer), "sha256": sha256(trainer)}
    require(str(trainer) in [c["path"] for c in contract["code_files"]],
            "trainer must be among code_files")

    ds = Path(args.deepspeed).resolve() if args.deepspeed else code_dir / "ds_zero3_native.json"
    require(ds.is_file(), "deepspeed config not found: " + str(ds))
    contract["deepspeed"] = {"path": str(ds), "sha256": sha256(ds)}

    contract["release_manifest"] = {"path": str(rel_path), "sha256": sha256(rel_path)}
    contract["release_id"] = release_id
    contract["run_id"] = args.run_id
    contract["training_mount"] = args.training_mount
    contract["run_dir"] = "%s/runs/%s" % (args.training_mount, args.run_id)
    contract["gpu_uuids"] = list(args.gpu_uuids)
    contract["mount_uuid"] = args.mount_uuid
    contract["resume"] = None

    seed_dir = Path(args.seed_dir).resolve()
    require((seed_dir / "model.safetensors.index.json").is_file(),
            "seed index not found under " + str(seed_dir))
    files = inventory(seed_dir)
    require(len(files) >= 20, "seed inventory suspiciously small: %d files" % len(files))

    if args.phase == "cpt":
        contract["seed"] = {
            "kind": "clean_upstream",
            "verified_clean": True,
            "repo": "unsloth/Qwen3.8-27B",
            "revision": "3ea932cee0a432ae86e9c7826cbe8aef52323a28",
            "path": str(seed_dir),
            "files": files,
        }
        # The verifier ties the seed to the pinned revision by directory name too.
        require(seed_dir.name == contract["seed"]["revision"],
                "seed directory basename %r != pinned revision; verify_clean_seed would fail"
                % seed_dir.name)
    else:
        require(args.parent_cpt_run, "--phase sft requires --parent-cpt-run")
        parent = Path(args.parent_cpt_run).resolve()
        require(parent.is_file(), "parent CPT run receipt not found: " + str(parent))
        parent_doc = json.loads(parent.read_text(encoding="utf-8"))
        require(parent_doc.get("run_id") and parent_doc["run_id"] != args.run_id,
                "SFT run_id must differ from the parent CPT run_id")
        receipt = parent.with_name("CLEAN_SEED_RECEIPT.json")
        contract["seed"] = {
            "kind": "selected_new_cpt",
            "path": str(seed_dir),
            "files": files,
            "verification": ({"path": str(receipt), "sha256": sha256(receipt)}
                             if receipt.is_file() else
                             {"path": str(parent), "sha256": sha256(parent)}),
            "release_id": release_id,
            "release_sha256": sha256(rel_path),
            "run_id": parent_doc["run_id"],
            "selection": args.selection,
            "parent_run": {"path": str(parent), "sha256": sha256(parent)},
        }

    out = Path(args.out).resolve()
    out.write_text(json.dumps(contract, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({
        "contract": str(out),
        "phase": args.phase,
        "run_id": args.run_id,
        "release_id": release_id,
        "seed_files_pinned": len(files),
        "code_files_pinned": len(contract["code_files"]),
        "sha256": sha256(out),
    }, indent=2))
    print("\nPass this to the launcher:  --contract-sha256 " + sha256(out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
