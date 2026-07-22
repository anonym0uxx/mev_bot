#!/usr/bin/env python3
"""
cleanup_branches.py — delete the stale build/m0 … build/mN branches (and any other listed
branches) locally and on the remote, so you're not left with a pile of unmerged junk.

Context: the overnight milestone runs created build/m0 … build/m8, each holding what turned out
to be mostly empty crates. The new leaf-by-leaf build (build_bot.py) works on its own single
branch and doesn't need these. This removes them cleanly.

Safety:
  - Never deletes the branch you're currently on, or 'main'/'master', or your --keep list.
  - --dry-run shows exactly what it would delete and touches nothing.
  - Deletes local branches with -D (force) ONLY after you confirm, since these are known-stale;
    remote deletes are attempted only with --remote.
  - Prints every action.

Usage:
    python cleanup_branches.py --repo .                 # delete local build/* branches (asks first)
    python cleanup_branches.py --repo . --dry-run       # show what would go, do nothing
    python cleanup_branches.py --repo . --remote        # also delete them on origin
    python cleanup_branches.py --repo . --pattern "build/" --keep main --keep bot-build
"""
from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path


def sh(cmd, cwd, timeout=60):
    import shutil, os
    run = list(cmd)
    r = shutil.which(run[0])
    if r:
        run[0] = r
    elif run[0] != "git":
        return subprocess.CompletedProcess(cmd, 127, "", f"not found: {run[0]}")
    try:
        if os.name == "nt":
            return subprocess.run(subprocess.list2cmdline(run), cwd=str(cwd), capture_output=True,
                                  text=True, encoding="utf-8", errors="replace", timeout=timeout,
                                  shell=True)
        return subprocess.run(run, cwd=str(cwd), capture_output=True, text=True,
                              encoding="utf-8", errors="replace", timeout=timeout)
    except subprocess.TimeoutExpired:
        return subprocess.CompletedProcess(cmd, 124, "", "timeout")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--pattern", default="build/",
                    help="delete branches whose name starts with this (default: build/)")
    ap.add_argument("--keep", action="append", default=[],
                    help="branch name to always keep (repeatable)")
    ap.add_argument("--remote", action="store_true", help="also delete matching branches on origin")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--yes", action="store_true", help="skip the confirmation prompt")
    args = ap.parse_args()
    repo = Path(args.repo).resolve()

    protected = {"main", "master"} | set(args.keep)
    cur = sh(["git", "rev-parse", "--abbrev-ref", "HEAD"], repo).stdout.strip()
    protected.add(cur)

    # local branches matching the pattern
    out = sh(["git", "branch", "--format=%(refname:short)"], repo).stdout
    local = [b.strip() for b in out.splitlines() if b.strip()]
    to_delete_local = [b for b in local if b.startswith(args.pattern) and b not in protected]

    # remote branches matching the pattern
    to_delete_remote = []
    if args.remote:
        rout = sh(["git", "branch", "-r", "--format=%(refname:short)"], repo).stdout
        for b in rout.splitlines():
            b = b.strip()
            if b.startswith(f"origin/{args.pattern}"):
                name = b[len("origin/"):]
                if name not in protected:
                    to_delete_remote.append(name)

    print("=" * 64)
    print("BRANCH CLEANUP")
    print("=" * 64)
    print(f"repo            : {repo}")
    print(f"current branch  : {cur} (protected)")
    print(f"pattern         : '{args.pattern}*'")
    print(f"always keep     : {sorted(protected)}")
    print()
    if not to_delete_local and not to_delete_remote:
        print("Nothing matches — no branches to delete.")
        return 0
    print(f"LOCAL branches to delete ({len(to_delete_local)}):")
    for b in to_delete_local:
        print(f"  - {b}")
    if args.remote:
        print(f"REMOTE (origin) branches to delete ({len(to_delete_remote)}):")
        for b in to_delete_remote:
            print(f"  - origin/{b}")
    print()

    if args.dry_run:
        print("[dry-run] nothing deleted.")
        return 0

    if not args.yes:
        print("These branches will be PERMANENTLY deleted. This cannot be undone from here")
        print("(though commits may survive in reflog for a while).")
        resp = input("Type 'delete' to proceed: ").strip().lower()
        if resp != "delete":
            print("Aborted — nothing deleted.")
            return 1

    for b in to_delete_local:
        r = sh(["git", "branch", "-D", b], repo)
        print(f"  local  {b}: {'deleted' if r.returncode == 0 else 'FAILED ' + r.stderr.strip()[:80]}")
    for b in to_delete_remote:
        r = sh(["git", "push", "origin", "--delete", b], repo)
        print(f"  remote {b}: {'deleted' if r.returncode == 0 else 'FAILED ' + r.stderr.strip()[:80]}")

    print("\nDone. Remaining branches:")
    out2 = sh(["git", "branch", "--format=%(refname:short)"], repo).stdout
    for b in out2.splitlines():
        if b.strip():
            print(f"  {b.strip()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
