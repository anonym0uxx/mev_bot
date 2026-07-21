#!/usr/bin/env python3
"""
scaffold_workspace.py — create the Rust Cargo workspace the leaf build needs, with crate names
that match what the materialized dossier tests expect.

Why this is needed: the leaf builder (build_bot.py) drives Claude Code to implement one function
at a time and then runs `cargo build` / `cargo test`. That only works if a real Cargo workspace
exists — a root Cargo.toml, member crates, and each crate's src/lib.rs with module structure.
The earlier milestone runs left empty comment-only crates under old names (hot-core, enrich, ...)
and NO workspace Cargo.toml, while materialize_tests.py writes tests into pump-quant-core,
pump-quant-strategy, pump-quant-evaluator. Nothing compiled because there was no foundation and
the crate names didn't even match.

This script creates a clean, COMPILING workspace under rust/ with exactly the three crates the
tests target, each with:
  - a Cargo.toml (workspace-inherited edition, overflow-checks where money math lives),
  - a src/lib.rs that declares the module structure and re-exports, so Claude has real files to
    slot functions into and `cargo build` succeeds on an empty-but-valid skeleton.

It is idempotent and non-destructive by default: it will not overwrite a crate's lib.rs if that
file already contains real code (so re-running after leaves are built is safe). Pass --force to
regenerate the skeleton lib.rs files (they only contain module declarations, never leaf logic).

Crate layout (matches materialize_tests.py COMPONENT_CRATE):
  pump-quant-core       <- reducer, shred, replay, lockfree, fixedpoint  (HOT: pure, no async)
  pump-quant-strategy   <- scalp_position, exit_ladder, economic_gate, safety_integrity
  pump-quant-evaluator  <- evaluator_stats

Usage:
    python scaffold_workspace.py --repo .
    python scaffold_workspace.py --repo . --force     # regenerate skeleton lib.rs files
"""
from __future__ import annotations

import argparse
from pathlib import Path

# crate -> (list of module names it will contain). Modules map to dossier components; each
# becomes a `pub mod <m>;` in lib.rs and a src/<m>.rs stub the leaves fill in.
CRATES = {
    "pump-quant-core": {
        "modules": ["reducer", "shred", "replay", "lockfree", "fixedpoint"],
        "overflow_checks": True,   # fixedpoint money math must not silently wrap
        "hot": True,
    },
    "pump-quant-strategy": {
        "modules": ["scalp_position", "exit_ladder", "economic_gate", "safety_integrity"],
        "overflow_checks": True,   # economic_gate is money math
        "hot": True,
    },
    "pump-quant-evaluator": {
        "modules": ["evaluator_stats"],
        "overflow_checks": True,   # the frozen accountant: integer lamports only
        "hot": False,
    },
}

ROOT_CARGO = """# Hermes bot workspace. The §24 performance-engineering law (criterion 109) is encoded as
# build configuration so it is compiled in rather than remembered. Phase-A (laptop) builds use
# the dev/portable profile; the release profile's deploy-CPU pinning and PGO are the server's job.
[workspace]
resolver = "2"
members = [
    "crates/pump-quant-core",
    "crates/pump-quant-strategy",
    "crates/pump-quant-evaluator",
]

[workspace.package]
edition = "2021"
rust-version = "1.85"

[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
debug = false
overflow-checks = false

# money/fixed-point crates keep overflow checks in release — silent money wrap is prohibited.
[profile.release.package.pump-quant-core]
overflow-checks = true
[profile.release.package.pump-quant-strategy]
overflow-checks = true
[profile.release.package.pump-quant-evaluator]
overflow-checks = true

[profile.dev]
opt-level = 1
debug = 1
incremental = true

[profile.bench]
inherits = "release"
debug = 1

# NOTE (§24(a)): -C target-cpu is NOT set here — it is injected via RUSTFLAGS from the
# infrastructure manifest's deploy-CPU entry on the server. Never `native` on the build box.
"""


def crate_cargo(name: str) -> str:
    return f"""[package]
name = "{name}"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true

[dependencies]
# Populated per the constitution's dependency discipline: narrow solana component crates
# (never the monolith), pruned feature sets, duplicates gated by `cargo tree -d` in CI.

[dev-dependencies]
# test-only deps for the dossier property tests live here.
"""


def crate_lib(name: str, modules: list[str], hot: bool) -> str:
    hot_note = ("// HOT crate: the hotpath_lint gate bans async/await, tokio, floats outside "
                "boundary adapters,\n// syscall clocks, sleeps, allocating macros, and panic "
                "paths in these modules.\n") if hot else ""
    mod_lines = "\n".join(f"pub mod {m};" for m in modules)
    return (f"//! {name} — workspace crate. Modules below are filled in leaf-by-leaf by the "
            f"build;\n//! each module's functions are implemented against locked dossier "
            f"property tests.\n{hot_note}\n{mod_lines}\n")


def module_stub(module: str) -> str:
    """A minimal but VALID Rust module file. It compiles (so `cargo build` succeeds on the
    skeleton) but contains no leaf logic — the build fills these in. We add a single private
    no-op so the file is valid and not comment-only, without pretending to implement anything."""
    return (f"//! {module} — implemented leaf-by-leaf against the dossier property tests.\n"
            f"//! Functions are added here by the build; this skeleton only establishes the "
            f"module.\n\n"
            f"// module scaffold marker (keeps the file valid before leaves land)\n"
            f"#[allow(dead_code)]\nconst _MODULE_SCAFFOLD: () = ();\n")


def has_real_code(path: Path) -> bool:
    """True if a lib.rs/module already has real (non-comment, non-scaffold) code we shouldn't
    clobber."""
    if not path.is_file():
        return False
    import re
    src = path.read_text(encoding="utf-8", errors="ignore")
    src = re.sub(r"/\*.*?\*/", "", src, flags=re.S)
    src = re.sub(r"^\s*//.*$", "", src, flags=re.M)
    # ignore the scaffold marker line and mod declarations when deciding "real code"
    src = src.replace("const _MODULE_SCAFFOLD: () = ();", "")
    src = re.sub(r"^\s*pub mod \w+;\s*$", "", src, flags=re.M)
    src = re.sub(r"^\s*#\[allow\(dead_code\)\]\s*$", "", src, flags=re.M)
    return bool(src.strip())


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--force", action="store_true",
                    help="regenerate skeleton lib.rs/module files even if present (never touches real code)")
    args = ap.parse_args()
    repo = Path(args.repo).resolve()
    rust = repo / "rust"
    rust.mkdir(exist_ok=True)

    # root workspace Cargo.toml
    root_toml = rust / "Cargo.toml"
    if not root_toml.is_file() or args.force:
        root_toml.write_text(ROOT_CARGO, encoding="utf-8")
        print(f"  wrote {root_toml.relative_to(repo)}")
    else:
        print(f"  kept  {root_toml.relative_to(repo)} (exists)")

    created, kept = 0, 0
    for crate, spec in CRATES.items():
        cdir = rust / "crates" / crate
        (cdir / "src").mkdir(parents=True, exist_ok=True)
        (cdir / "tests").mkdir(parents=True, exist_ok=True)

        # crate Cargo.toml
        ctoml = cdir / "Cargo.toml"
        if not ctoml.is_file() or args.force:
            ctoml.write_text(crate_cargo(crate), encoding="utf-8")
            created += 1
        else:
            kept += 1

        # lib.rs (module declarations) — regenerate only if it has no real code
        lib = cdir / "src" / "lib.rs"
        if not lib.is_file() or args.force or not has_real_code(lib):
            lib.write_text(crate_lib(crate, spec["modules"], spec["hot"]), encoding="utf-8")
            created += 1
        else:
            kept += 1
            # even if lib.rs has real code, make sure every expected module is declared
            src = lib.read_text(encoding="utf-8", errors="ignore")
            missing = [m for m in spec["modules"] if f"pub mod {m};" not in src]
            if missing:
                src = src.rstrip() + "\n" + "\n".join(f"pub mod {m};" for m in missing) + "\n"
                lib.write_text(src, encoding="utf-8")
                print(f"    + added missing module decls to {crate}/src/lib.rs: {missing}")

        # module stub files — only create if absent or empty; NEVER clobber real leaf code
        for m in spec["modules"]:
            mfile = cdir / "src" / f"{m}.rs"
            if not mfile.is_file():
                mfile.write_text(module_stub(m), encoding="utf-8")
                created += 1
            elif args.force and not has_real_code(mfile):
                mfile.write_text(module_stub(m), encoding="utf-8")

    print(f"\nworkspace ready under rust/  (created/updated: {created}, kept: {kept})")
    print("crates:", ", ".join(CRATES.keys()))
    print("\nThe leaf build can now compile: `cargo build` has a valid workspace to build against,")
    print("and each dossier component has a module file for its functions to be implemented in.")
    print("Run:  python build_bot.py --repo <path> --max-usd 60 --max-hours 10 --push --finalize")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
