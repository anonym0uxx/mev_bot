"""
Build-phase provenance — mechanical enforcement of the §9.5 two-phase build boundary
(criterion 113).

TRUST MODEL (v2 — hardened after the "can agents write the manifest?" question):
    The original design compared two strings inside a JSON file. That made the file itself
    security-critical: any agent that could edit it could forge deployment-host provenance
    and certify hardware criteria from a laptop. v2 removes that dependency entirely:

      1. WHERE AM I is MEASURED, never read from a file. The gate fingerprints the live
         machine (OS machine GUID + CPU model) at check time. Editing a file cannot change
         what machine you are actually on.
      2. WHICH MACHINE IS THE SERVER is a one-time operator DECLARATION in the manifest's
         `deployment_host` block, protected by a HUMAN-ONLY hash pin in the evidence store
         (mirroring pin-evaluator). If the block changes after pinning, the hash mismatches
         and Phase-B certification fails closed until the operator re-pins.
      3. Everything else in the manifest — the `facts` ledger — is deliberately AGENT-WRITABLE
         (Claude Code may generate/refresh it; Hermes appends provenance-stamped facts via the
         record_infra_fact tool) because the gate does not trust it for certification.

    Net effect: agents can create, move, copy, and append to the manifest freely, and none of
    it can unlock Phase B. Only being physically on the pinned machine can.
"""
from __future__ import annotations

import hashlib
import json
import platform
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

# criteria that may only be certified on the deployment hardware
PHASE_B_EXCLUSIVE_CRITERIA = {103, 109}   # latency budgets + Rust perf law (hardware-measured)

# gate names that imply hardware-specific measurement
PHASE_B_GATES = {"bench", "latency", "pgo", "tuning", "endpoint_warmth"}


# ------------------------------------------------------------- live measurement
def measure_machine() -> dict[str, str]:
    """Fingerprint the machine we are ACTUALLY running on. File-independent by design.

    Windows: MachineGuid from the registry (stable across reboots/renames).
    Linux:   /etc/machine-id.
    Fallback: hostname (weak — flagged so the gate can treat it conservatively).
    """
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
    if not cpu_model and platform.system().lower() != "windows":
        try:
            for line in Path("/proc/cpuinfo").read_text().splitlines():
                if line.lower().startswith("model name"):
                    cpu_model = line.split(":", 1)[1].strip()
                    break
        except OSError:
            pass

    return {"machine_id": machine_id.strip(), "id_source": id_source,
            "cpu_model": cpu_model.strip()}


# ------------------------------------------------------------- declaration + pin
def deployment_declaration(manifest_path: str | Path) -> Optional[dict[str, Any]]:
    """The operator's `deployment_host` block, or None."""
    try:
        p = Path(manifest_path)
        if not p.is_file():
            return None
        data = json.loads(p.read_text(encoding="utf-8"))
        dep = data.get("deployment_host")
        return dep if isinstance(dep, dict) and dep.get("machine_id") else None
    except (OSError, json.JSONDecodeError):
        return None


def declaration_sha(dep: dict[str, Any]) -> str:
    """Hash of ONLY the deployment_host block (canonical JSON), so appending facts or
    refreshing informational fields elsewhere in the manifest never invalidates the pin."""
    canon = json.dumps(dep, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canon.encode("utf-8")).hexdigest()


@dataclass
class PhaseCheckResult:
    ok: bool
    reason: str
    phase: str
    detail: dict[str, Any] = field(default_factory=dict)


def criterion_is_phase_b(criterion: int) -> bool:
    return criterion in PHASE_B_EXCLUSIVE_CRITERIA


def gate_is_phase_b(gate_name: str) -> bool:
    g = (gate_name or "").lower()
    return any(k in g for k in PHASE_B_GATES)


def check_phase_provenance(gate_name: str,
                           criteria_touched: list[int],
                           manifest_path: str | Path,
                           pinned_sha: str = "",
                           _measure=measure_machine) -> PhaseCheckResult:
    """Phase-B-exclusive work requires: a declared deployment host (manifest), a matching
    operator pin when one exists in the store, and — decisively — the LIVE-MEASURED machine
    being that host. Phase-A work passes anywhere. `_measure` is injectable for tests only.
    """
    needs_hw = gate_is_phase_b(gate_name) or any(
        criterion_is_phase_b(c) for c in criteria_touched)

    live = _measure()

    if not needs_hw:
        return PhaseCheckResult(
            True, "phase-A-permissible work (no deployment-hardware requirement)",
            "A", {"needs_hardware": False, "live_machine": live["machine_id"][:16]})

    dep = deployment_declaration(manifest_path)
    if dep is None:
        return PhaseCheckResult(
            False,
            "Phase-B-exclusive gate requires a manifest with an operator-declared "
            "deployment_host (§9.5, criterion 113); none found. Generate one with "
            "scripts/gen_manifest.py --declare-deployment-host ON THE SERVER, then pin it "
            "(hermes-supervise pin-manifest).",
            "unknown", {"needs_hardware": True, "declaration_found": False})

    sha = declaration_sha(dep)
    if pinned_sha and sha != pinned_sha:
        return PhaseCheckResult(
            False,
            "deployment_host declaration does not match the operator's pin — the manifest "
            "was changed after pinning. If the change is legitimate, re-pin via "
            "`hermes-supervise pin-manifest`; otherwise this is tampering. Failing closed.",
            "unknown",
            {"needs_hardware": True, "declaration_sha": sha[:16],
             "pinned_sha": pinned_sha[:16], "pin_mismatch": True})

    if live["machine_id"] and live["machine_id"] == dep.get("machine_id"):
        weak = live["id_source"] == "hostname_fallback"
        return PhaseCheckResult(
            not weak,
            ("deployment-host identity confirmed by LIVE measurement "
             f"({live['id_source']}); Phase-B certification permitted"
             if not weak else
             "live measurement fell back to hostname, which is spoofable — install a proper "
             "machine-id source before certifying Phase-B work"),
            "B" if not weak else "unknown",
            {"needs_hardware": True, "live_machine": live["machine_id"][:16],
             "id_source": live["id_source"], "cpu_model": live["cpu_model"],
             "pin_verified": bool(pinned_sha)})

    return PhaseCheckResult(
        False,
        f"Phase-B-exclusive gate '{gate_name}' (criteria "
        f"{sorted(c for c in criteria_touched if criterion_is_phase_b(c))}) may be certified "
        "only on the deployment host, and the LIVE-MEASURED machine "
        f"('{live['cpu_model'] or live['machine_id'][:12] or 'unidentified'}') is not it. "
        "This is Phase A: author the code here; its hardware measurement and completion "
        "happen on the server. Failing closed. (Editing the manifest cannot change this — "
        "identity is measured, not read.)",
        "A",
        {"needs_hardware": True, "live_machine": live["machine_id"][:16],
         "id_source": live["id_source"], "declared": str(dep.get("machine_id"))[:16]})


# Backwards-compatible shim used by preflight (informational only, never for certification)
@dataclass
class MachineProvenance:
    machine_id: str = ""
    cpu_model: str = ""
    cpu_features: list[str] = field(default_factory=list)
    is_deployment_host: bool = False
    phase: str = "A"

    @classmethod
    def from_manifest(cls, manifest_path: str | Path) -> Optional["MachineProvenance"]:
        dep = deployment_declaration(manifest_path)
        live = measure_machine()
        if dep is None:
            return cls(machine_id=live["machine_id"], cpu_model=live["cpu_model"])
        is_dep = bool(live["machine_id"]) and live["machine_id"] == dep.get("machine_id") \
            and live["id_source"] != "hostname_fallback"
        return cls(machine_id=live["machine_id"], cpu_model=live["cpu_model"],
                   cpu_features=list(dep.get("cpu_features", [])),
                   is_deployment_host=is_dep, phase="B" if is_dep else "A")
