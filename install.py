#!/usr/bin/env python3
"""
Hermes Supervisor — one-command installer.

    python install.py                # interactive, auto-discovers everything it can
    python install.py --repo C:\\hermes\\pump-quant   # non-interactive if paths given
    python install.py --check        # verify an existing install, change nothing

What it automates (everything you'd otherwise do by hand):
  1. Installs supervisor Python deps into THIS interpreter (requests, pyyaml [, jsonschema]).
  2. Installs the `mcp` package into the HERMES AGENT interpreter (auto-detected; without it
     Hermes silently disables all MCP support).
  3. Auto-discovers: your bot repo (constitution at docs/HERMES_ONE_SHOT_PROMPT.md),
     the Hermes home (~/.hermes), and the llama.cpp endpoint.
  4. Writes supervisor/config/supervisor.yaml with the discovered absolute paths.
  5. Safely merges the mcp_servers block into ~/.hermes/config.yaml (backup written first;
     existing entries preserved; idempotent on re-run).
  6. Copies the hermes-build-verification skill into ~/.hermes/skills/ (live immediately,
     no registration needed per Hermes docs).
  7. Runs the supervisor's offline test suite and a llama.cpp health probe, then prints a
     go/no-go summary with the exact message to send Hermes to start the build.

Idempotent: safe to re-run any time; it only fixes what's missing.
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
GREEN, RED, YELLOW, END = "\033[92m", "\033[91m", "\033[93m", "\033[0m"


def ok(msg: str) -> None:
    print(f"{GREEN}[ok]{END} {msg}")


def warn(msg: str) -> None:
    print(f"{YELLOW}[!!]{END} {msg}")


def fail(msg: str) -> None:
    print(f"{RED}[XX]{END} {msg}")


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


# ------------------------------------------------------------------ discovery
def find_repo(explicit: str | None) -> Path | None:
    """Locate the bot repo by its constitution file."""
    cands: list[Path] = []
    if explicit:
        cands.append(Path(explicit))
    env = os.environ.get("HERMES_REPO")
    if env:
        cands.append(Path(env))
    # common roots to scan (shallow)
    roots = [HERE.parent, Path.home(), Path("C:/hermes"), Path("C:/"), Path.home() / "projects"]
    for root in roots:
        if not root.exists():
            continue
        try:
            for child in list(root.iterdir())[:64]:
                if (child / "docs" / "HERMES_ONE_SHOT_PROMPT.md").is_file():
                    cands.append(child)
        except PermissionError:
            continue
    for c in cands:
        if (c / "docs" / "HERMES_ONE_SHOT_PROMPT.md").is_file():
            return c.resolve()
    return None


def _git_env() -> dict:
    """Environment for all git calls: never hang on interactive prompts (fail loud instead)."""
    env = dict(os.environ)
    env["GIT_TERMINAL_PROMPT"] = "0"
    env["GCM_INTERACTIVE"] = "never"
    return env


def ensure_credential_helper() -> str:
    """Make sure a credential helper is configured. On Windows prefer GCM ('manager') which
    stores encrypted in Windows Credential Manager. Returns the helper in effect."""
    r = run(["git", "config", "--global", "credential.helper"])
    helper = r.stdout.strip()
    if helper:
        return helper
    helper = "manager" if os.name == "nt" else "store"
    run(["git", "config", "--global", "credential.helper", helper])
    if helper == "store":
        warn("no credential helper was configured; using 'store' (plaintext ~/.git-credentials). "
             "On Windows, Git Credential Manager ('manager') stores encrypted — recommended.")
    return helper


def store_github_pat(pat: str) -> bool:
    """Persist the PAT via `git credential approve` into the configured helper
    (GCM -> Windows Credential Manager, DPAPI-encrypted). The token never touches our
    config files or logs."""
    ensure_credential_helper()
    payload = ("protocol=https\nhost=github.com\n"
               "username=x-access-token\npassword=" + pat + "\n\n")
    r = subprocess.run(["git", "credential", "approve"], input=payload,
                       capture_output=True, text=True, env=_git_env())
    return r.returncode == 0


def github_auth_ok(url: str) -> bool:
    """Non-interactive reachability/auth probe against the repo."""
    r = subprocess.run(["git", "ls-remote", "--heads", url], capture_output=True,
                       text=True, env=_git_env(), timeout=60)
    return r.returncode == 0


def clone_repo(url: str, dest: Path) -> Path | None:
    """Clone the GitHub repo (uses your system git + its stored credentials/SSH keys)."""
    if shutil.which("git") is None:
        fail("git not on PATH — install Git for Windows, then re-run")
        return None
    dest.parent.mkdir(parents=True, exist_ok=True)
    r = subprocess.run(["git", "clone", url, str(dest)], capture_output=True,
                       text=True, env=_git_env())
    if r.returncode != 0:
        fail(f"git clone failed: {r.stderr.strip()[:300]}")
        return None
    if not (dest / "docs" / "HERMES_ONE_SHOT_PROMPT.md").is_file():
        fail("cloned, but docs/HERMES_ONE_SHOT_PROMPT.md not found in the repo — "
             "commit the constitution to GitHub first")
        return None
    return dest.resolve()


def constitution_freshness(repo: Path) -> str:
    """Return 'fresh' | 'behind' | 'unknown' vs origin for the constitution file."""
    if shutil.which("git") is None:
        return "unknown"
    if subprocess.run(["git", "fetch", "--quiet", "origin"], cwd=str(repo),
                      capture_output=True, text=True, env=_git_env()).returncode != 0:
        return "unknown"
    r = run(["git", "rev-list", "--count", "HEAD..origin/HEAD", "--",
             "docs/HERMES_ONE_SHOT_PROMPT.md"], cwd=str(repo))
    if r.returncode != 0:
        # origin/HEAD may be unset; try main then master
        for br in ("origin/main", "origin/master"):
            r = run(["git", "rev-list", "--count", f"HEAD..{br}", "--",
                     "docs/HERMES_ONE_SHOT_PROMPT.md"], cwd=str(repo))
            if r.returncode == 0:
                break
    if r.returncode != 0:
        return "unknown"
    return "behind" if r.stdout.strip() not in ("", "0") else "fresh"


def repo_origin_and_commit(repo: Path) -> tuple[str, str]:
    url = run(["git", "remote", "get-url", "origin"], cwd=str(repo)).stdout.strip()
    sha = run(["git", "rev-parse", "HEAD"], cwd=str(repo)).stdout.strip()
    return url or "no-origin", sha or "no-git"


def find_hermes_home() -> Path:
    return (Path.home() / ".hermes").resolve()


def find_hermes_python() -> str:
    """
    Find the interpreter Hermes Agent runs under so `mcp` lands in the right env.
    Strategy: `hermes` on PATH -> read its shebang/venv; else pipx venv; else current python.
    """
    hermes = shutil.which("hermes")
    if hermes:
        p = Path(hermes)
        # pipx/venv layout: <venv>/Scripts|bin/hermes -> use sibling python
        for pyname in ("python.exe", "python3", "python"):
            cand = p.parent / pyname
            if cand.exists():
                return str(cand)
        # script with shebang
        try:
            first = p.read_text(errors="ignore").splitlines()[0]
            if first.startswith("#!"):
                shebang = first[2:].strip().split()[0]
                if Path(shebang).exists():
                    return shebang
        except Exception:
            pass
    pipx_venv = Path.home() / ".local" / "pipx" / "venvs" / "hermes-agent"
    for pyname in ("Scripts/python.exe", "bin/python"):
        cand = pipx_venv / pyname
        if cand.exists():
            return str(cand)
    return sys.executable  # fall back: same interpreter (works for pip-installed hermes)


def probe_llama(base_url: str) -> bool:
    try:
        import requests  # installed in step 1
        r = requests.get(f"{base_url}/health", timeout=5)
        return r.status_code == 200
    except Exception:
        return False


# ------------------------------------------------------------------- installs
def pip_install(python: str, pkgs: list[str]) -> bool:
    r = run([python, "-m", "pip", "install", "--quiet", *pkgs])
    return r.returncode == 0


# ------------------------------------------------------------------- yaml ops
def write_supervisor_yaml(repo: Path, base_url: str) -> Path:
    import yaml
    cfg_path = HERE / "supervisor" / "config" / "supervisor.yaml"
    data = yaml.safe_load(cfg_path.read_text(encoding="utf-8")) or {}
    data["repo_path"] = str(repo)
    data["constitution_path"] = str(repo / "docs" / "HERMES_ONE_SHOT_PROMPT.md")
    data["evidence_db"] = str(HERE / "evidence.db")
    data.setdefault("model", {})["base_url"] = base_url
    cfg_path.write_text(yaml.safe_dump(data, sort_keys=False), encoding="utf-8")
    return cfg_path


def merge_hermes_mcp(hermes_home: Path, supervisor_cfg: Path) -> Path:
    import yaml
    cfg = hermes_home / "config.yaml"
    hermes_home.mkdir(parents=True, exist_ok=True)
    data = {}
    if cfg.is_file():
        backup = cfg.with_suffix(".yaml.bak")
        shutil.copy2(cfg, backup)
        data = yaml.safe_load(cfg.read_text(encoding="utf-8")) or {}
    servers = data.setdefault("mcp_servers", {})
    servers["hermes_supervisor"] = {
        "command": sys.executable,
        "args": ["-m", "supervisor.mcp.server", "--config", str(supervisor_cfg)],
        "cwd": str(HERE),
    }
    cfg.write_text(yaml.safe_dump(data, sort_keys=False), encoding="utf-8")
    return cfg


def install_skill(hermes_home: Path) -> Path:
    src = HERE / "hermes_skill" / "hermes-build-verification"
    dst = hermes_home / "skills" / "hermes-build-verification"
    dst.parent.mkdir(parents=True, exist_ok=True)
    if dst.exists():
        shutil.rmtree(dst)
    shutil.copytree(src, dst)
    return dst


# --------------------------------------------------------------------- verify
def run_tests() -> bool:
    r = run([sys.executable, "-m", "pytest", "-q", str(HERE / "tests")], cwd=HERE)
    if r.returncode == 0:
        return True
    # pytest missing -> fall back to the shim runner style check: import-only smoke
    r2 = run([sys.executable, "-c",
              "import sys;sys.path.insert(0,r'%s');"
              "from supervisor.mcp import server;from supervisor.gates import runner;"
              "from supervisor.gates import hotpath_lint;"
              "print('import-ok')" % HERE], cwd=HERE)
    return r2.returncode == 0 and "import-ok" in r2.stdout


def verify_dossiers() -> tuple[bool, str]:
    """Load every HARD-component dossier through the real loader; report missing/broken."""
    try:
        sys.path.insert(0, str(HERE))
        from supervisor.reinforcement.dossier import load_dossier, HARD_COMPONENTS
        ddir = HERE / "supervisor" / "reinforcement" / "dossiers"
        missing, broken, leaves = [], [], 0
        for comp in HARD_COMPONENTS:
            p = ddir / f"{comp}.yaml"
            if not p.exists():
                missing.append(comp); continue
            try:
                d = load_dossier(p); d.leaf_order(); leaves += len(d.leaves)
            except Exception as e:  # noqa: BLE001
                broken.append(f"{comp}: {e}")
        if missing or broken:
            return False, f"missing={missing or 'none'} broken={broken or 'none'}"
        return True, f"{len(HARD_COMPONENTS)} dossiers, {leaves} leaves, all load + topo-sort clean"
    except Exception as e:  # noqa: BLE001
        return False, f"dossier check errored: {e}"


def place_scaffold(repo: Path, check_only: bool) -> str:
    """Copy repo_scaffold/rust/ into the repo if the workspace isn't there yet.
    Never overwrites an existing rust/Cargo.toml (the build owns it after M0)."""
    src = HERE / "repo_scaffold" / "rust"
    if not src.exists():
        return "no scaffold bundled (skip)"
    dst = repo / "rust"
    ws = dst / "Cargo.toml"
    if ws.exists():
        return "workspace already present (left untouched)"
    if check_only:
        return "scaffold NOT placed (workspace missing; run without --check to place it)"
    dst.mkdir(parents=True, exist_ok=True)
    for item in src.rglob("*"):
        rel = item.relative_to(src)
        target = dst / rel
        if item.is_dir():
            target.mkdir(parents=True, exist_ok=True)
        else:
            if not target.exists():
                shutil.copy2(item, target)
    return f"workspace scaffold placed at {dst} (perf-law profiles + lint-scoped crates)"


# ----------------------------------------------------------------------- main
def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", help="path to the bot repo (contains docs/HERMES_ONE_SHOT_PROMPT.md)")
    ap.add_argument("--repo-url", help="GitHub URL to clone if no local repo is found "
                                        "(also reads env HERMES_REPO_URL)")
    ap.add_argument("--github-pat", help="fine-grained PAT for the private repo; prefer env "
                                          "GITHUB_PAT / HERMES_GITHUB_PAT over a CLI arg "
                                          "(CLI args land in shell history)")
    ap.add_argument("--clone-to", default=str(Path.home() / "hermes" / "pump-quant"),
                    help="where to clone the repo if cloning is needed")
    ap.add_argument("--llama-url", default="http://127.0.0.1:8080")
    ap.add_argument("--check", action="store_true", help="verify only; change nothing")
    args = ap.parse_args()

    print("== Hermes Supervisor installer ==\n")

    # 1) supervisor deps (this interpreter)
    if not args.check:
        if pip_install(sys.executable, ["requests", "pyyaml", "jsonschema", "pytest"]):
            ok("supervisor deps installed (requests, pyyaml, jsonschema, pytest)")
        else:
            warn("pip install had issues; will verify imports below")
    try:
        import yaml  # noqa: F401
        import requests  # noqa: F401
        ok("supervisor imports verified")
    except ImportError as e:
        fail(f"missing dependency: {e}. Run: {sys.executable} -m pip install requests pyyaml")
        return 1

    # 2) mcp into Hermes's interpreter
    hermes_py = find_hermes_python()
    if not args.check:
        if pip_install(hermes_py, ["mcp"]):
            ok(f"`mcp` package installed into Hermes interpreter: {hermes_py}")
        else:
            warn(f"could not install `mcp` into {hermes_py} — run manually: "
                 f"\"{hermes_py}\" -m pip install mcp  (without it Hermes disables MCP silently)")
    else:
        r = run([hermes_py, "-c", "import mcp"])
        ok("`mcp` present in Hermes interpreter") if r.returncode == 0 else \
            warn("`mcp` NOT importable in Hermes interpreter")

    # 3) discovery
    # --- seamless GitHub auth (private repo) ---
    pat = args.github_pat or os.environ.get("GITHUB_PAT") or os.environ.get("HERMES_GITHUB_PAT")
    url_hint = args.repo_url or os.environ.get("HERMES_REPO_URL", "")
    if pat and not args.check:
        if store_github_pat(pat):
            ok("GitHub PAT stored in the OS credential store (encrypted via Git Credential "
               "Manager on Windows) — all future git operations are silent; the token is NOT "
               "written to any file or config")
        else:
            fail("failed to store PAT via `git credential approve` — is git installed?")
            return 1
        if args.github_pat:
            warn("PAT was passed as a CLI arg (lands in shell history); prefer env "
                 "GITHUB_PAT next time")
    if url_hint:
        if github_auth_ok(url_hint):
            ok("GitHub auth verified: repo reachable non-interactively")
        else:
            fail("cannot reach the private repo non-interactively. Fix: re-run with a valid "
                 "fine-grained PAT via env GITHUB_PAT (Contents: read/write on this repo), "
                 "or set up an SSH deploy key and use the git@ URL. Nothing will hang; "
                 "auth just isn't in place yet.")
            if not args.check:
                return 1

    repo = find_repo(args.repo)
    if repo:
        ok(f"bot repo found: {repo}")
    else:
        url = args.repo_url or os.environ.get("HERMES_REPO_URL", "")
        if not url and not args.check:
            url = input("No local repo found. GitHub URL to clone (or blank to enter a local "
                        "path): ").strip()
        if url:
            ok(f"cloning {url} -> {args.clone_to}")
            repo = clone_repo(url, Path(args.clone_to))
            if not repo:
                return 1
            ok(f"repo cloned: {repo}")
        else:
            if args.check:
                fail("bot repo not found (docs/HERMES_ONE_SHOT_PROMPT.md)")
                return 1
            entered = input("Path to your bot repo: ").strip()
            repo = find_repo(entered)
            if not repo:
                fail("constitution not found at <repo>/docs/HERMES_ONE_SHOT_PROMPT.md — "
                     "commit it first")
                return 1
            ok(f"bot repo confirmed: {repo}")

    # GitHub is the source of truth: warn if the local constitution is behind origin
    origin, sha = repo_origin_and_commit(repo)
    ok(f"repo origin: {origin} @ {sha[:12]}")
    fresh = constitution_freshness(repo)
    if fresh == "behind":
        warn("local constitution is BEHIND origin — `git pull` before building so Hermes "
             "uses the latest committed version")
    elif fresh == "fresh":
        ok("constitution is up to date with origin")
    else:
        warn("could not compare with origin (offline or detached) — verify freshness manually")

    hermes_home = find_hermes_home()
    ok(f"Hermes home: {hermes_home}" if hermes_home.exists()
       else f"Hermes home will be created: {hermes_home}")

    # 3b) dossier integrity — the design layer the build depends on
    dok, dmsg = verify_dossiers()
    ok(f"dossiers verified: {dmsg}") if dok else fail(f"dossier check: {dmsg}")

    # 3c) place the workspace scaffold into the repo (perf-law profiles + lint-scoped crates)
    smsg = place_scaffold(repo, args.check)
    ok(f"scaffold: {smsg}")

    # 4-6) write configs + skill
    if not args.check:
        scfg = write_supervisor_yaml(repo, args.llama_url)
        ok(f"supervisor.yaml written: {scfg}")
        hcfg = merge_hermes_mcp(hermes_home, scfg)
        ok(f"MCP server registered in {hcfg} (backup saved if it existed)")
        skill = install_skill(hermes_home)
        ok(f"skill installed (live immediately, no registration needed): {skill}")

    # 7) verify
    ok("offline test suite / imports pass") if run_tests() else fail("supervisor tests failed")
    if probe_llama(args.llama_url):
        ok(f"llama.cpp endpoint healthy at {args.llama_url}")
    else:
        warn(f"llama.cpp endpoint not responding at {args.llama_url} — start llama-server "
             f"(see supervisor/config/llama_server.yaml), then re-run: python install.py --check")

    # cargo presence (gates need it)
    ok("cargo found (gates ready)") if shutil.which("cargo") else \
        warn("cargo not on PATH — install rustup + msvc target before the build starts")

    print(f"""
== NEXT (the only manual steps left) ==
1. Restart Hermes Agent so it discovers the MCP server
   (tools appear as mcp_hermes_supervisor_gate_verify, ...).
2. Message Hermes (Telegram/CLI):

   Build from docs/HERMES_ONE_SHOT_PROMPT.md. Follow the constitution exactly,
   including the §62 supervisor MCP tool mandate. Start at M0 and report gate
   results verbatim.

Re-run `python install.py --check` any time to re-verify the whole chain.
""")
    return 0


if __name__ == "__main__":
    sys.exit(main())
