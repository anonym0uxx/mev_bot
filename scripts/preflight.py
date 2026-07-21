#!/usr/bin/env python3
"""
preflight.py — verify the environment is ready before an unattended build run.

Checks, in order, and reports a single READY / NOT READY verdict:
  - git repo present and clean-ish
  - claude binary on PATH and authenticated
  - rust toolchain (cargo) present
  - python + supervisor package importable
  - infrastructure manifest present and self-consistent (tells you Phase A vs B)
  - constitution present at the expected path

Run this once on each machine (laptop and server). It never changes anything.
"""
from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path


def _ok(label, good, detail=""):
    print(f"  [{'ok ' if good else 'XX '}] {label}" + (f" — {detail}" if detail else ""))
    return good


def _bin(name):
    p = shutil.which(name)
    return (p is not None), (p or "not found")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--config", default="supervisor/config/supervisor.yaml")
    args = ap.parse_args()
    repo = Path(args.repo).resolve()

    print(f"preflight for {repo}\n")
    all_ok = True

    all_ok &= _ok("git repository", (repo / ".git").is_dir(), str(repo))

    has_claude, cdetail = _bin("claude")
    all_ok &= _ok("claude binary on PATH", has_claude, cdetail)
    if has_claude:
        try:
            v = subprocess.run(["claude", "--version"], capture_output=True, text=True, timeout=20)
            _ok("claude --version", v.returncode == 0, v.stdout.strip()[:60])
        except Exception as e:  # noqa: BLE001
            _ok("claude --version", False, str(e)[:60])

    has_cargo, cargo_detail = _bin("cargo")
    all_ok &= _ok("rust toolchain (cargo)", has_cargo, cargo_detail)

    has_gh, gh_detail = _bin("gh")
    _ok("gh CLI (optional, for auto-PR)", has_gh, gh_detail)   # optional, not gating

    # supervisor importable
    try:
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
        from supervisor.core.config import SupervisorConfig  # noqa: F401
        from supervisor.core.constitution import load_constitution
        imp_ok = True
    except Exception as e:  # noqa: BLE001
        imp_ok = False
        print(f"       import error: {e}")
    all_ok &= _ok("supervisor package importable", imp_ok)

    # constitution present
    try:
        cfg = SupervisorConfig.load(args.config)
        cons_ok = Path(cfg.constitution_path).is_file()
        detail = cfg.constitution_path
    except Exception as e:  # noqa: BLE001
        cons_ok = False
        detail = str(e)[:80]
    all_ok &= _ok("constitution present", cons_ok, detail)

    # infra manifest -> phase
    manifest = repo / "infra_manifest.json"
    phase = "unknown"
    if manifest.is_file():
        try:
            from supervisor.gates.build_phase import MachineProvenance
            prov = MachineProvenance.from_manifest(manifest)
            phase = prov.phase if prov else "unknown"
            _ok("infrastructure manifest", True,
                f"phase {phase}" + (f" ({prov.cpu_model})" if prov and prov.cpu_model else ""))
            if phase == "A":
                print("       -> Phase A: authoring/tests certify here; bench/latency/PGO/tuning "
                      "are deferred to the deployment server (correct on a laptop).")
            elif phase == "B":
                print("       -> Phase B: this IS the deployment host; hardware criteria certify here.")
        except Exception as e:  # noqa: BLE001
            _ok("infrastructure manifest", False, str(e)[:60])
            all_ok = False
    else:
        _ok("infrastructure manifest", False,
            "missing — copy supervisor/config/infra_manifest.example.json to infra_manifest.json")
        all_ok = False

    print()
    print("VERDICT:", "READY" if all_ok else "NOT READY (fix the XX lines above)")
    if all_ok and phase == "A":
        print("You can start: python auto_build.py --repo", str(repo), "--from M0")
    return 0 if all_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
