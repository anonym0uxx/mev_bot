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


# Deploy-CPU codegen targets this script will write. `native` is absent ON PURPOSE and is
# rejected explicitly below: constitution §66 forbids `-C target-cpu=native` on a build box,
# that ban was prose only, and this manifest field is the doorway a `native` would otherwise
# enter the build through (it is what feeds RUSTFLAGS). Anything not listed is an operator
# decision — verify with `rustc --print target-cpus` and add it here rather than bypassing.
KNOWN_TARGET_CPUS = {
    "znver5",     # EPYC 9005 / Turin — the declared deploy box (EPYC 9655P)
    "znver4",     # EPYC 9004 / Genoa — documented fallback if the toolchain lacks Zen 5
    "znver3",
    "x86-64-v3",
    "x86-64-v4",
}


def _show(path: Path) -> int:
    """Print the declaration, its hash, and the live measurement the gate compares against."""
    from supervisor.gates.build_phase import declaration_sha, deployment_declaration
    live = measure_machine()
    print(f"manifest: {path}  ({'present' if path.is_file() else 'MISSING'})")
    dep = deployment_declaration(path) if path.is_file() else None
    if dep is None:
        print("\ndeployment_host: NONE — this is why a Phase-B gate would refuse.\n"
              "  Run ON THE SERVER:  python scripts/gen_manifest.py "
              "--declare-deployment-host --target-cpu znver5")
    else:
        print("\ndeployment_host (the PINNED surface — declaration_sha hashes this whole block,\n"
              "so every key in it is a hash input; adding a comment key here breaks the pin):")
        for k in sorted(dep):
            print(f"  {k:<12}: {dep[k]}")
        print(f"  {'sha256':<12}: {declaration_sha(dep)}")
    print("\nLIVE measurement (what the gate actually compares against):")
    print(f"  machine_id  : {live['machine_id']}")
    print(f"  id_source   : {live['id_source']}")
    print(f"  cpu_model   : {live['cpu_model'] or '(unreported)'}")
    if dep is not None:
        match = bool(live["machine_id"]) and live["machine_id"] == dep.get("machine_id")
        print(f"\n  MATCH: {'YES — this IS the declared deployment host' if match else 'NO — Phase A here'}")
    if live["id_source"] == "hostname_fallback":
        print("\n!! machine identity fell back to the HOSTNAME, which is spoofable. The gate\n"
              "   refuses Phase-B certification even when the id MATCHES, so pinning this\n"
              "   leaves Phase B locked. Windows wants "
              "HKLM\\SOFTWARE\\Microsoft\\Cryptography\\MachineGuid.")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", default="infra_manifest.json")
    ap.add_argument("--declare-deployment-host", action="store_true",
                    help="write deployment_host from THIS machine's measured identity "
                         "(run on the server, once, then pin)")
    ap.add_argument("--target-cpu", default="",
                    help="record the -C target-cpu value for criterion 109 (e.g. znver5); "
                         "only meaningful with --declare-deployment-host. 'native' is REFUSED "
                         "(§66); unrecognised values are refused rather than defaulted.")
    ap.add_argument("--add-fact", nargs=2, metavar=("KEY", "VALUE"))
    ap.add_argument("--source", default="", help="provenance for --add-fact")
    ap.add_argument("--show", action="store_true",
                    help="print the declaration, its sha, and the live measurement; changes nothing")
    ap.add_argument("--force", action="store_true",
                    help="overwrite an existing deployment_host declaration — this CHANGES the "
                         "declaration hash and INVALIDATES the operator's pin, failing every "
                         "Phase-B gate closed until it is re-pinned")
    args = ap.parse_args()

    if args.show:
        return _show(Path(args.manifest))

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

        # §66: `-C target-cpu=native` is forbidden on a build box, because the artifact must be
        # reproducible on a machine other than the one that built it. This field feeds RUSTFLAGS,
        # so it is where the ban has to be enforced rather than merely written down.
        target = args.target_cpu.strip().lower()
        if target in {"native", "target-cpu=native", "-c target-cpu=native"}:
            print("[gen_manifest] REFUSED: target_cpu=native is forbidden by constitution §66. "
                  "Pin codegen to the deploy CPU's recorded feature set instead — for the "
                  "declared EPYC 9655P use --target-cpu znver5 (znver4 if the toolchain lacks "
                  "the Zen 5 model).")
            return 1
        if target and target not in KNOWN_TARGET_CPUS:
            print(f"[gen_manifest] REFUSED: unrecognised target_cpu '{target}'. Known: "
                  f"{', '.join(sorted(KNOWN_TARGET_CPUS))}. Adding one is an operator decision — "
                  "confirm `rustc --print target-cpus` lists it and add it to KNOWN_TARGET_CPUS.")
            return 1

        # Overwriting a declaration changes its hash, which is exactly what invalidates the
        # operator's pin. Refuse unless asked twice.
        existing = data.get("deployment_host")
        if isinstance(existing, dict) and existing.get("machine_id") and not args.force:
            from supervisor.gates.build_phase import declaration_sha
            print(f"[gen_manifest] a deployment_host declaration already exists "
                  f"(sha {declaration_sha(existing)[:16]}...):\n"
                  f"  machine_id : {existing.get('machine_id','')[:20]}...\n"
                  f"  target_cpu : {existing.get('target_cpu','?')}\n"
                  "REFUSING to overwrite. Doing so changes the declaration hash and INVALIDATES "
                  "the pin, failing every Phase-B gate closed until re-pinned. Re-run with "
                  "--force if that is genuinely intended, then immediately re-pin.")
            return 1

        # MINIMAL BY DESIGN. `declaration_sha` hashes this whole dict with sort_keys, so every
        # key here is a hash input. No timestamps and no comment keys: a timestamp makes the
        # declaration non-reproducible (you could never re-derive the pinned sha by re-measuring
        # this box), and a comment key breaks the pin the first time someone rewords it. Note
        # that supervisor/config/infra_manifest.example.json ships a `_how` key INSIDE
        # deployment_host — do not copy that example verbatim.
        data["deployment_host"] = {
            "machine_id": live["machine_id"],
            "cpu_model": live["cpu_model"],
            "id_source": live["id_source"],
        }
        if target:
            data["deployment_host"]["target_cpu"] = target
        # Kept OUTSIDE the hashed block so it is informational, not a hash input.
        data["deployment_host_declared_at"] = time.time()
        from supervisor.gates.build_phase import declaration_sha as _dsha
        print(f"[gen_manifest] deployment_host declared from MEASURED identity "
              f"({live['id_source']}); declaration sha256 {_dsha(data['deployment_host'])}\n"
              f"[gen_manifest] Now pin it (HUMAN-ONLY, not callable via MCP):\n"
              f"    python -m supervisor.supervise pin-manifest --manifest {path}")

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
