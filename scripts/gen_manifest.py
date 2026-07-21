#!/usr/bin/env python3
"""
gen_manifest.py — create or refresh infra_manifest.json by MEASURING this machine.

Safe for ANY invoker — the operator, Claude Code, or Hermes — because nothing here is
security-critical: the phase gate measures the live machine itself at check time and verifies
the deployment_host declaration against the operator's pin. This script just makes the file
convenient and truthful.

    python scripts/gen_manifest.py                        # refresh informational fields
    python scripts/gen_manifest.py --declare-deployment-host
        # ON THE SERVER, ONCE: writes deployment_host from MEASURED values (never hand-typed),
        # then the operator pins it: hermes-supervise pin-manifest

    python scripts/gen_manifest.py --add-fact key value --source "helius dashboard 2026-07"
        # append a provenance-stamped infrastructure fact (agent-writable lane)

Behavior:
  - `current_machine` is informational only (the gate ignores it) and is always refreshed
    from live measurement.
  - `deployment_host` is written ONLY with --declare-deployment-host, and only from measured
    values. Re-running with the flag on a different machine changes the declaration — which
    breaks the operator's pin, so the gate fails closed until a human re-pins. Tamper-evident
    by construction.
  - `facts` is an append-only list of {key, value, source, at} records.
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path


def _find_measure():
    """Locate supervisor.gates.build_phase.measure_machine from any placement (scaffold,
    assembled repo, or package root); fall back to an inline measurement so this script
    works standalone. The inline copy mirrors build_phase.measure_machine — if you change
    one, change both."""
    here = Path(__file__).resolve()
    for base in [here.parent.parent, *here.parents]:
        if (base / "supervisor" / "gates" / "build_phase.py").is_file():
            sys.path.insert(0, str(base))
            try:
                from supervisor.gates.build_phase import measure_machine  # type: ignore
                return measure_machine
            except Exception:  # noqa: BLE001
                break
    # ---- inline fallback (keep in sync with build_phase.measure_machine) ----
    import platform
    import subprocess

    def measure_machine() -> dict:
        machine_id, id_source = "", "none"
        system = platform.system().lower()
        if system == "windows":
            try:
                import winreg  # type: ignore
                with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE,
                                    r"SOFTWARE\\Microsoft\\Cryptography") as k:
                    machine_id, _ = winreg.QueryValueEx(k, "MachineGuid")
                    id_source = "windows_machine_guid"
            except Exception:  # noqa: BLE001
                try:
                    p = subprocess.run(["wmic", "csproduct", "get", "uuid"],
                                       capture_output=True, text=True, timeout=10)
                    lines = [l.strip() for l in p.stdout.splitlines() if l.strip()]
                    if len(lines) >= 2:
                        machine_id, id_source = lines[1], "wmic_uuid"
                except Exception:  # noqa: BLE001
                    pass
        else:
            try:
                mid = Path("/etc/machine-id")
                if mid.is_file():
                    machine_id = mid.read_text(encoding="utf-8").strip()
                    id_source = "etc_machine_id"
            except OSError:
                pass
        if not machine_id:
            machine_id, id_source = platform.node(), "hostname_fallback"
        cpu_model = platform.processor() or ""
        if not cpu_model and system != "windows":
            try:
                for line in Path("/proc/cpuinfo").read_text().splitlines():
                    if line.lower().startswith("model name"):
                        cpu_model = line.split(":", 1)[1].strip()
                        break
            except OSError:
                pass
        return {"machine_id": machine_id.strip(), "id_source": id_source,
                "cpu_model": cpu_model.strip()}
    return measure_machine


measure_machine = _find_measure()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", default="infra_manifest.json")
    ap.add_argument("--declare-deployment-host", action="store_true",
                    help="write deployment_host from THIS machine's measured identity "
                         "(run on the server, once, then pin)")
    ap.add_argument("--target-cpu", default="",
                    help="record the -C target-cpu value for criterion 109 (e.g. znver5); "
                         "only meaningful with --declare-deployment-host")
    ap.add_argument("--add-fact", nargs=2, metavar=("KEY", "VALUE"))
    ap.add_argument("--source", default="", help="provenance for --add-fact")
    args = ap.parse_args()

    path = Path(args.manifest)
    data: dict = {}
    if path.is_file():
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            print(f"[gen_manifest] {path} is not valid JSON; refusing to overwrite it")
            return 1

    live = measure_machine()
    data["current_machine"] = {**live, "refreshed_at": time.time(),
                               "_note": "informational only — the phase gate measures live"}

    if args.declare_deployment_host:
        if live["id_source"] == "hostname_fallback":
            print("[gen_manifest] REFUSING to declare deployment_host from a hostname "
                  "fallback — install a proper machine-id source first (spoofable identity "
                  "must not become the declaration).")
            return 1
        data["deployment_host"] = {
            "machine_id": live["machine_id"],
            "cpu_model": live["cpu_model"],
            "id_source": live["id_source"],
            "declared_at": time.time(),
        }
        if args.target_cpu:
            data["deployment_host"]["target_cpu"] = args.target_cpu
        print(f"[gen_manifest] deployment_host declared from MEASURED identity "
              f"({live['id_source']}). Now pin it:  hermes-supervise pin-manifest "
              f"--manifest {path}")

    if args.add_fact:
        key, value = args.add_fact
        data.setdefault("facts", []).append({
            "key": key, "value": value, "source": args.source or "unspecified",
            "at": time.time()})
        print(f"[gen_manifest] fact appended: {key}")

    path.write_text(json.dumps(data, indent=2), encoding="utf-8")
    print(f"[gen_manifest] wrote {path} (machine {live['machine_id'][:12]}... "
          f"via {live['id_source']})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
