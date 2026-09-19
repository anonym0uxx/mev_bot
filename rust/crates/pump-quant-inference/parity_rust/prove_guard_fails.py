#!/usr/bin/env python3
"""Prove the M9 parity guard FAILS under the old (defective) inventory base.

A guard that has only ever passed is untested. This plants the defect the guard
exists to catch -- `management_base(ADD)` resolving against INVENTORY instead of
the account's capital -- asserts the parity test fails and names the documented
~4x numbers, then restores the source byte-identically and asserts it passes.

Run from anywhere; paths resolve relative to this file.
"""
import hashlib
import os
import pathlib
import re
import subprocess

HERE = pathlib.Path(__file__).resolve()
ROOT = HERE.parent.parent.parent.parent.parent  # .../<repo>/rust/crates/<crate>/parity_rust
F = ROOT / "rust/crates/pump-quant-inference/src/seam.rs"
CARGO = os.environ.get("CARGO", "cargo")

env = dict(os.environ)
env.setdefault("CARGO_TARGET_DIR", str(pathlib.Path.home() / ".cache/mev_target_gh"))

orig = F.read_text()
h0 = hashlib.sha256(orig.encode()).hexdigest()
anchor = "Action::Add => Some(ManagementBase::AccountCapital)"
assert orig.count(anchor) == 1, "anchor not unique -- refusing to guess"
F.write_text(orig.replace(anchor, "Action::Add => Some(ManagementBase::Inventory)"))


def run(label):
    p = subprocess.run(
        [CARGO, "test", "--offline", "-j4", "-p", "pump-quant-inference",
         "--test", "management_parity"],
        cwd=str(ROOT / "rust"), capture_output=True, text=True, env=env, timeout=560,
    )
    out = p.stdout + p.stderr
    print(f"[{label}] exit={p.returncode}")
    for ln in out.splitlines():
        if re.search(r"test result:|seam disagrees|panicked|left:|right:", ln):
            print("   ", ln.strip()[:160])
    return p.returncode, out


rc, out = run("planted inventory base (must FAIL)")
assert rc != 0, "the guard did NOT fail under the defect -> it proves nothing"
assert "505000000" in out and "125000000" in out, (
    "failure did not surface the documented 4.04x numbers"
)
print("   -> guard fails under the defect and names 505000000 vs 125000000  OK")

F.write_text(orig)
h1 = hashlib.sha256(F.read_text().encode()).hexdigest()
print("reverted byte-identical:", h0 == h1, f"({h0[:16]})")
assert h0 == h1

rc, _ = run("restored account-capital base (must PASS)")
assert rc == 0, "restored tree does not pass"
print("   -> restored tree passes")
