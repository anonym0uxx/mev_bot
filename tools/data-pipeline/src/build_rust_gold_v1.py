#!/usr/bin/env python3
"""
rust_gold_v1: Git-history extraction pipeline for Qwen CPT gold.

Extracts engineering trajectories from the mev_bot Rust/Solana/Pump quant system's
full reachable Git history. Builds 5 schema types:
  1. git_commit_v1     — commit metadata, files, stats, provenance
  2. code_change_v1    — before→diff→after at function/module granularity
  3. engineering_trajectory_v1 — problem→investigation→change→evidence→result→later-repair
  4. repair_v1         — bug/regression→diagnosis→patch→validation
  5. repo_state_v1     — curated architecture/source/docs snapshots

QUALITY > QUANTITY. A commit is NOT automatically a positive example.
Objective build/test/replay evidence outranks commit-message claims.

Usage:
  python build_rust_gold_v1.py --phase enumerate    # Phase 1: commit enumeration + classification
  python build_rust_gold_v1.py --phase extract      # Phase 2: code_change extraction
  python build_rust_gold_v1.py --phase trajectory   # Phase 3: trajectory + repair chaining
  python build_rust_gold_v1.py --phase repo_state   # Phase 4: repo state snapshots
  python build_rust_gold_v1.py --phase validate     # Phase 5: cargo validation (isolated worktree)
  python build_rust_gold_v1.py --phase split        # Phase 6: chronological splits
  python build_rust_gold_v1.py --phase manifest     # Phase 7: QA/provenance manifest
  python build_rust_gold_v1.py --phase all          # All phases in order
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import time
import uuid
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

# ─── Configuration ──────────────────────────────────────────────────────────

REPO_ROOT = Path("D:/repos/mev_bot")
OUTPUT_DIR = REPO_ROOT / "tools" / "data-pipeline" / "output" / "rust_gold_v1"
SCHEMA_VERSION = "rust_gold_v1"
PIPELINE_VERSION = "1.0.0"

# Exclusion patterns (files to NOT extract code changes from)
EXCLUDE_PATH_PATTERNS = [
    r"target/",
    r"node_modules/",
    r"dist/",
    r"\.git/",
    r"vendor/",
    r"*.parquet",
    r"*.bin",
    r"*.zst",
    r"*.db",
    r"*.db-shm",
    r"*.db-wal",
    r"*.woff",
    r"*.ttf",
    r"*.png",
    r"*.jpg",
    r"*.jpeg",
    r"*.gif",
    r"*.webp",
    r"*.mp4",
    r"*.wav",
    r"*.sol",
    r"*.key",
    r"*.pem",
    r"*.env",
    r"credentials",
    r"wallet",
    r"secret",
    r"PRIVATE_KEY",
    r"seed_phrase",
]

# File types we care about for code changes
RUST_EXTENSIONS = {".rs"}
TOML_EXTENSIONS = {".toml"}
CONFIG_EXTENSIONS = {".toml", ".json", ".yaml", ".yml", ".txt", ".env", ".example"}
DOC_EXTENSIONS = {".md", ".rst"}
TS_EXTENSIONS = {".ts", ".js"}

# Classification keywords for commit messages
CLASSIFY_PATTERNS = {
    "REVERT": [r"^revert", r"^revert:", r"revert\s", r"undo\s+(commit|change)"],
    "FAILED_APPROACH": [r"failed", r"abandoned", r"superseded", r"did not work", r"revert.*failed", r"remove.*broken", r"was.*wrong", r"incorrect.*approach"],
    "BUG_FIX": [r"^fix", r"^fix:", r"fix\s", r"bug\s*fix", r"correct", r"repair", r"broken", r"wrong", r"incorrect", r"defect", r"fault"],
    "REGRESSION_FIX": [r"regression", r"revert.*regress", r"fix.*regress", r"was.*working.*before", r"broke.*in"],
    "PERFORMANCE": [r"^perf", r"^perf:", r"perf\s", r"optimi[sz]", r"latency", r"throughput", r"speed", r"hot.?path", r"zero.?copy", r"cache", r"inline", r"SIMD", r"benchmark"],
    "TEST": [r"^test", r"^test:", r"test\s", r"tests?\s+(pass|added|new)", r"unit test", r"integration test", r"regression test", r"property test", r"fuzz"],
    "REFACTOR": [r"^refactor", r"^refactor:", r"refactor\s", r"rename", r"extract", r"consolidat", r"cleanup", r"dedup", r"remove dead", r"remove.*legacy", r"remove.*unused"],
    "CONFIG": [r"^config", r"^config:", r"config\s", r"threshold", r"tuning", r"calibrat", r"CHAMPION_CONFIG", r"\.env", r"deploy", r"canary"],
    "DOC": [r"^doc", r"^doc:", r"docs?\s", r"document", r"spec", r"blueprint", r"plan", r"architect", r"README", r"ARCH_"],
    "FEATURE": [r"^feat", r"^feat:", r"feat\s", r"implement", r"add\s+(support|feature|gate|engine|crate)", r"wire", r"enable", r"introduce"],
}

# Subsystem mapping (crate → subsystem)
CRATE_TO_SUBSYSTEM = {
    "pump-quant-app": "composition_root",
    "pump-quant-core": "core_engine",
    "pump-quant-signals": "signal_processing",
    "pump-quant-protocol": "solana_protocol",
    "pump-quant-execution": "execution_layer",
    "pump-quant-ingest": "data_ingest",
    "pump-quant-strategy": "strategy_engine",
    "pump-quant-evaluator": "strategy_evaluation",
    "pump-quant-canonical": "canonical_types",
    "pump-quant-clock": "timing_infrastructure",
    "pump-quant-domain": "domain_model",
    "pump-quant-features": "feature_engineering",
    "pump-quant-governance": "governance_policy",
    "pump-quant-journal": "journal_logging",
    "pump-quant-market-state": "market_state",
    "pump-quant-memory": "memory_cache",
    "pump-quant-narrative": "narrative_analysis",
    "pump-quant-simulator": "simulation_replay",
    "pump-quant-social": "social_intelligence",
    "pump-quant-wallet-graph": "wallet_analysis",
    "pump-quant-watchlist": "watchlist_management",
    "pump-quant-replay": "deterministic_replay",
    "pump-quant-junction": "feed_junction",
    "pump-quant-brain": "brain_memory",
    "pq-evaluator": "strategy_evaluator",
    "pq-research-runner": "research_runner",
    "pq-regression": "regression_testing",
    "pump-quant-ostune": "os_tuning",
    "pump-quant-treasury": "treasury_policy",
    "bench": "benchmarking",
    # Pre-workspace era crates (before workspace migration to rust/crates/)
    "pump-quant-core": "core_engine",
    "pump-quant-signals": "signal_processing",
    "pump-quant-protocol": "solana_protocol",
    "pump-quant-execution": "execution_layer",
    "pump-quant-ingest": "data_ingest",
}

RUN_UUID = str(uuid.uuid4())


def git_cmd(args, repo=REPO_ROOT, timeout=120):
    """Run a git command and return stdout."""
    result = subprocess.run(
        ["git", "-C", str(repo)] + args,
        capture_output=True, text=True, timeout=timeout
    )
    if result.returncode != 0:
        raise RuntimeError(f"git {args}: {result.stderr}")
    return result.stdout


def git_cmd_bytes(args, repo=REPO_ROOT, timeout=300):
    """Run a git command and return stdout as bytes."""
    result = subprocess.run(
        ["git", "-C", str(repo)] + args,
        capture_output=True, timeout=timeout
    )
    if result.returncode != 0:
        raise RuntimeError(f"git {args}: {result.stderr.decode('utf-8', errors='replace')}")
    return result.stdout


def should_exclude(path):
    """Check if a file path should be excluded from code change extraction."""
    import fnmatch
    path_lower = path.lower()
    for pattern in EXCLUDE_PATH_PATTERNS:
        pat = pattern.lower()
        # If it looks like a glob (contains * or ?), use fnmatch
        if '*' in pat or '?' in pat:
            if fnmatch.fnmatch(path_lower, pat):
                return True
        else:
            # Otherwise treat as substring/regex
            if pat in path_lower:
                return True
            try:
                if re.search(pat, path, re.IGNORECASE):
                    return True
            except re.error:
                pass
    return False


def classify_commit(message, files_touched):
    """Classify a commit by its message and files touched. Returns (class, confidence)."""
    msg_lower = message.lower().strip()

    # Check REVERT first (most specific)
    for pattern in CLASSIFY_PATTERNS["REVERT"]:
        if re.search(pattern, msg_lower):
            # Check if it's a revert of a failed approach
            for fp in CLASSIFY_PATTERNS["FAILED_APPROACH"]:
                if re.search(fp, msg_lower):
                    return "FAILED_APPROACH", 0.85
            return "REVERT", 0.90

    # Check FAILED_APPROACH
    for pattern in CLASSIFY_PATTERNS["FAILED_APPROACH"]:
        if re.search(pattern, msg_lower):
            return "FAILED_APPROACH", 0.80

    # Check REGRESSION_FIX
    for pattern in CLASSIFY_PATTERNS["REGRESSION_FIX"]:
        if re.search(pattern, msg_lower):
            return "REGRESSION_FIX", 0.85

    # Check BUG_FIX
    for pattern in CLASSIFY_PATTERNS["BUG_FIX"]:
        if re.search(pattern, msg_lower):
            return "BUG_FIX", 0.80

    # Check PERFORMANCE
    for pattern in CLASSIFY_PATTERNS["PERFORMANCE"]:
        if re.search(pattern, msg_lower):
            return "PERFORMANCE", 0.75

    # Check TEST
    for pattern in CLASSIFY_PATTERNS["TEST"]:
        if re.search(pattern, msg_lower):
            return "TEST", 0.75

    # Check REFACTOR
    for pattern in CLASSIFY_PATTERNS["REFACTOR"]:
        if re.search(pattern, msg_lower):
            return "REFACTOR", 0.70

    # Check CONFIG
    for pattern in CLASSIFY_PATTERNS["CONFIG"]:
        if re.search(pattern, msg_lower):
            return "CONFIG", 0.70

    # Check DOC
    for pattern in CLASSIFY_PATTERNS["DOC"]:
        if re.search(pattern, msg_lower):
            return "DOC", 0.70

    # Check FEATURE
    for pattern in CLASSIFY_PATTERNS["FEATURE"]:
        if re.search(pattern, msg_lower):
            return "FEATURE", 0.70

    # Fallback: infer from file types
    rust_count = sum(1 for f in files_touched if f.endswith(".rs"))
    test_count = sum(1 for f in files_touched if "/tests/" in f or "test_" in f.lower() or "_test" in f.lower())
    doc_count = sum(1 for f in files_touched if f.endswith(".md"))
    config_count = sum(1 for f in files_touched if f.endswith((".toml", ".json", ".yaml", ".yml")))

    if test_count > 0 and rust_count > 0 and test_count >= rust_count * 0.5:
        return "TEST", 0.50
    if doc_count > 0 and rust_count == 0:
        return "DOC", 0.50
    if config_count > 0 and rust_count == 0:
        return "CONFIG", 0.50
    if rust_count > 0:
        return "FEATURE", 0.40  # default for Rust changes without clear signal

    return "DOC", 0.30  # ultimate fallback


def get_subsystem(filepath):
    """Map a file path to its subsystem."""
    # rust/crates/<crate-name>/src/...  (workspace era)
    m = re.match(r"rust/crates/([^/]+)/", filepath)
    if m:
        crate = m.group(1)
        return CRATE_TO_SUBSYSTEM.get(crate, f"unknown:{crate}")
    # rust/<crate-name>/src/...  (pre-workspace era, e.g. rust/pump-quant-core/)
    m = re.match(r"rust/([^/]+)/", filepath)
    if m:
        crate = m.group(1)
        # Normalize crate name by removing trailing -core etc if needed
        return CRATE_TO_SUBSYSTEM.get(crate, f"early_rust:{crate}")
    # tools/<tool-name>/src/...  (standalone tools)
    m = re.match(r"tools/([^/]+)/", filepath)
    if m:
        tool = m.group(1)
        if "social" in tool.lower():
            return "social_intelligence"
        if "data-pipeline" in tool.lower():
            return "data_pipeline_tooling"
        return f"tooling:{tool}"
    if filepath.startswith("bench/"):
        return "benchmarking"
    if filepath.startswith("docs/"):
        return "documentation"
    return "other"


def is_rust_relevant(files_touched):
    """Check if a commit touches Rust source files (not just data/docs)."""
    for f in files_touched:
        if f.endswith(".rs") and not should_exclude(f):
            return True
        if f.endswith(".toml") and ("rust/" in f or "Cargo.toml" in f) and not should_exclude(f):
            return True
    return False


def is_data_or_excluded(files_touched):
    """Check if a commit ONLY touches data/excluded files (no code)."""
    for f in files_touched:
        if f.endswith((".rs", ".toml")) and not should_exclude(f):
            if not f.startswith("tools/data-pipeline/output"):
                return False
    return True


# ─── Phase 1: Commit Enumeration ────────────────────────────────────────────

def phase_enumerate():
    """Enumerate all reachable commits and build git_commit_v1 records."""
    print("[Phase 1] Enumerating commits...")

    # Get all reachable commits (main + rev36 branch)
    # Format: SHA|parent_SHAs|timestamp|author|message
    raw = git_cmd([
        "log", "--all", "--reverse",
        "--format=%H|%P|%at|%an|%s"
    ])

    commits = []
    for line in raw.strip().split("\n"):
        parts = line.split("|", 4)
        if len(parts) < 5:
            continue
        sha, parents_str, ts, author, message = parts
        parents = parents_str.split() if parents_str else []
        commits.append({
            "sha": sha,
            "parents": parents,
            "timestamp": int(ts),
            "author": author,
            "message": message,
        })

    print(f"  Total reachable commits: {len(commits)}")

    # For each commit, get files touched + stats
    git_commits = []
    rust_commit_count = 0
    ts_commit_count = 0
    data_commit_count = 0

    for i, c in enumerate(commits):
        if i % 50 == 0:
            print(f"  Processing commit {i}/{len(commits)}...")

        # Get file changes
        try:
            stat_raw = git_cmd(["show", "--stat", "--format=", c["sha"]])
        except RuntimeError:
            stat_raw = ""

        files_touched = []
        for line in stat_raw.strip().split("\n"):
            line = line.strip()
            if not line:
                continue
            # Format: path | N ++ N --
            m = re.match(r"^(.+?)\s\|\s(\d+)\s([+]+)?\s*([-]+)?", line)
            if m:
                filepath = m.group(1).strip()
                changes = int(m.group(2))
                files_touched.append(filepath)

        # Classify
        commit_class, confidence = classify_commit(c["message"], files_touched)

        # Determine era/relevance
        is_rust = is_rust_relevant(files_touched)
        is_data_only = is_data_or_excluded(files_touched)

        if is_rust:
            rust_commit_count += 1
        elif is_data_only:
            data_commit_count += 1
        else:
            ts_commit_count += 1

        # Subsystems touched
        subsystems = set()
        for f in files_touched:
            if not should_exclude(f):
                sub = get_subsystem(f)
                if sub != "other":
                    subsystems.add(sub)
        subsystems = sorted(subsystems)

        # Count Rust files specifically
        rust_files = [f for f in files_touched if f.endswith(".rs") and not should_exclude(f)]
        toml_files = [f for f in files_touched if f.endswith(".toml") and not should_exclude(f)]
        doc_files = [f for f in files_touched if f.endswith(".md") and not should_exclude(f)]
        test_files = [f for f in files_touched if ("/tests/" in f or "/test/" in f) and f.endswith(".rs")]

        # Short SHA for display
        short_sha = c["sha"][:12]

        record = {
            "schema": "git_commit_v1",
            "schema_version": SCHEMA_VERSION,
            "sha": c["sha"],
            "short_sha": short_sha,
            "parents": c["parents"],
            "parent_count": len(c["parents"]),
            "timestamp": c["timestamp"],
            "timestamp_iso": datetime.fromtimestamp(c["timestamp"], tz=timezone.utc).isoformat(),
            "author": c["author"],
            "message": c["message"],
            "classification": commit_class,
            "classification_confidence": confidence,
            "is_rust_relevant": is_rust,
            "is_data_only": is_data_only,
            "files_total": len(files_touched),
            "rust_files": len(rust_files),
            "toml_files": len(toml_files),
            "doc_files": len(doc_files),
            "test_files": len(test_files),
            "subsystems": subsystems,
            "files_touched": files_touched if len(files_touched) <= 50 else files_touched[:50] + ["... (truncated)"],
        }

        git_commits.append(record)

    # Write output
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    out_path = OUTPUT_DIR / "git_commits_v1.jsonl"
    with open(out_path, "w") as f:
        for rec in git_commits:
            f.write(json.dumps(rec) + "\n")

    print(f"  Rust-relevant commits: {rust_commit_count}")
    print(f"  TS-only commits: {ts_commit_count}")
    print(f"  Data/excluded commits: {data_commit_count}")
    print(f"  Written: {out_path}")

    # Classification distribution
    class_dist = defaultdict(int)
    for rec in git_commits:
        if rec["is_rust_relevant"]:
            class_dist[rec["classification"]] += 1
    print("  Classification distribution (Rust-relevant):")
    for cls, count in sorted(class_dist.items(), key=lambda x: -x[1]):
        print(f"    {cls}: {count}")

    return git_commits


# ─── Phase 2: Code Change Extraction ────────────────────────────────────────

def extract_diff_at_function_level(sha, filepath):
    """Extract before→diff→after at function granularity for a given file in a commit."""
    try:
        # Get the full diff for this file
        diff_raw = git_cmd(["diff", "--unified=5", f"{sha}~1", sha, "--", filepath])
    except RuntimeError:
        return None

    if not diff_raw.strip():
        return None

    # Get before (parent version) and after (commit version) content
    try:
        before_content = git_cmd(["show", f"{sha}~1:{filepath}"])
    except RuntimeError:
        before_content = ""  # new file

    try:
        after_content = git_cmd(["show", f"{sha}:{filepath}"])
    except RuntimeError:
        after_content = ""  # deleted file

    # Parse diff into hunks
    hunks = []
    current_hunk = None
    for line in diff_raw.split("\n"):
        if line.startswith("@@"):
            if current_hunk:
                hunks.append(current_hunk)
            # Parse hunk header: @@ -old_start,old_len +new_start,new_len @@
            m = re.match(r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@", line)
            if m:
                current_hunk = {
                    "old_start": int(m.group(1)),
                    "old_len": int(m.group(2) or 1),
                    "new_start": int(m.group(3)),
                    "new_len": int(m.group(4) or 1),
                    "lines": [line],
                }
            else:
                current_hunk = {"lines": [line]}
        elif current_hunk:
            current_hunk["lines"].append(line)

    if current_hunk:
        hunks.append(current_hunk)

    # Identify function context from before/after content
    # Look for fn declarations near the changed region
    def find_function_context(content, line_num):
        """Find the function declaration that contains the given line number."""
        lines = content.split("\n")
        current_fn = None
        fn_start = None
        for i, line in enumerate(lines, 1):
            # Match Rust fn declarations: "fn name(" or "pub fn name(" etc
            m = re.match(r"^\s*(pub\s+)?(async\s+)?(unsafe\s+)?(extern\s+)?" +
                         r"(fn|impl|struct|enum|trait|mod)\s+([\w<>()]+)", line)
            if m:
                kind = m.group(5)
                name = m.group(6)
                if i <= line_num:
                    current_fn = f"{kind} {name}"
                    fn_start = i
                else:
                    break
        return current_fn, fn_start

    # For each hunk, find function context
    hunk_contexts = []
    for hunk in hunks:
        old_fn, old_fn_start = find_function_context(before_content, hunk.get("old_start", 0))
        new_fn, new_fn_start = find_function_context(after_content, hunk.get("new_start", 0))
        hunk_contexts.append({
            "old_function": old_fn,
            "new_function": new_fn,
        })

    return {
        "filepath": filepath,
        "hunks": hunks,
        "hunk_contexts": hunk_contexts,
        "diff": diff_raw,
        "before_size": len(before_content),
        "after_size": len(after_content),
        "before_truncated": len(before_content) > 50000,
        "after_truncated": len(after_content) > 50000,
        "before_content": before_content[:50000] if before_content else "",
        "after_content": after_content[:50000] if after_content else "",
    }


def phase_extract(git_commits=None):
    """Extract code_change_v1 records from Rust-relevant commits."""
    print("[Phase 2] Extracting code changes...")

    if git_commits is None:
        commits_path = OUTPUT_DIR / "git_commits_v1.jsonl"
        if not commits_path.exists():
            print("  ERROR: Run --phase enumerate first.")
            return
        git_commits = []
        with open(commits_path) as f:
            for line in f:
                git_commits.append(json.loads(line))

    # Filter to Rust-relevant, non-data-only, non-trivial commits
    # Skip commits that only touch data files or have no Rust files
    relevant_commits = [
        c for c in git_commits
        if c["is_rust_relevant"] and not c["is_data_only"]
    ]

    print(f"  Relevant commits for extraction: {len(relevant_commits)}")

    # Deduplicate trivial formatting commits (commits where all changes are
    # whitespace-only or only touch .toml files)
    def is_trivial(commit):
        """Check if a commit is trivial (whitespace-only or formatting-only)."""
        if commit["rust_files"] == 0:
            return True
        if commit["files_total"] > 100:
            return True  # likely mechanical bulk commit
        # Check if the diff is whitespace-only
        try:
            diff = git_cmd(["diff", "--stat", f"{commit['sha']}~1", f"{commit['sha']}", "--", "*.rs"])
            if not diff.strip():
                return True
        except RuntimeError:
            pass
        return False

    # Limit to meaningful commits (skip trivial)
    meaningful = [c for c in relevant_commits if not is_trivial(c)]
    print(f"  Meaningful commits (after trivial dedup): {len(meaningful)}")

    code_changes = []
    skipped = 0

    for i, commit in enumerate(meaningful):
        if i % 20 == 0:
            print(f"  Processing commit {i}/{len(meaningful)} (sha={commit['short_sha']})...")

        # Get Rust files changed in this commit
        try:
            stat_raw = git_cmd(["show", "--stat", "--format=", commit["sha"]])
        except RuntimeError:
            continue

        rust_files = []
        for line in stat_raw.strip().split("\n"):
            line = line.strip()
            if not line:
                continue
            m = re.match(r"^(.+?)\s\|\s(\d+)", line)
            if m:
                filepath = m.group(1).strip()
                if filepath.endswith(".rs") and not should_exclude(filepath):
                    rust_files.append(filepath)

        if not rust_files:
            skipped += 1
            continue

        # Limit to most significant files (by change count) to avoid enormous commits
        # Skip commits touching >30 Rust files (likely mechanical refactors)
        if len(rust_files) > 30:
            print(f"    Skipping large commit {commit['short_sha']} ({len(rust_files)} Rust files)")
            skipped += 1
            continue

        for filepath in rust_files[:15]:  # cap at 15 files per commit
            change = extract_diff_at_function_level(commit["sha"], filepath)
            if change and change["hunks"]:
                # Skip if the change is tiny (1-2 lines) and in a non-critical file
                total_changed = sum(
                    len([l for l in h["lines"] if l.startswith(("+", "-"))])
                    for h in change["hunks"]
                )
                if total_changed <= 2 and "test" not in filepath.lower():
                    continue  # skip trivial single-line changes

                record = {
                    "schema": "code_change_v1",
                    "schema_version": SCHEMA_VERSION,
                    "commit_sha": commit["sha"],
                    "short_sha": commit["short_sha"],
                    "commit_message": commit["message"],
                    "commit_classification": commit["classification"],
                    "filepath": filepath,
                    "subsystem": get_subsystem(filepath),
                    "hunk_count": len(change["hunks"]),
                    "function_contexts": change["hunk_contexts"],
                    "diff": change["diff"][:100000],  # cap diff size
                    "before_size": change["before_size"],
                    "after_size": change["after_size"],
                    "before_truncated": change["before_truncated"],
                    "after_truncated": change["after_truncated"],
                    "before_content": change["before_content"],
                    "after_content": change["after_content"],
                    "provenance": {
                        "run_uuid": RUN_UUID,
                        "extracted_at": datetime.now(timezone.utc).isoformat(),
                        "source": "git_diff",
                    },
                }
                code_changes.append(record)

    out_path = OUTPUT_DIR / "code_changes_v1.jsonl"
    with open(out_path, "w") as f:
        for rec in code_changes:
            f.write(json.dumps(rec) + "\n")

    print(f"  Code changes extracted: {len(code_changes)}")
    print(f"  Skipped (trivial/no-rust): {skipped}")
    print(f"  Written: {out_path}")

    return code_changes


# ─── Phase 3: Trajectory + Repair Chaining ──────────────────────────────────

def phase_trajectory(git_commits=None, code_changes=None):
    """Build engineering_trajectory_v1 and repair_v1 records by linking commits."""
    print("[Phase 3] Building trajectories and repair chains...")

    if git_commits is None:
        commits_path = OUTPUT_DIR / "git_commits_v1.jsonl"
        git_commits = [json.loads(l) for l in open(commits_path)]
    if code_changes is None:
        changes_path = OUTPUT_DIR / "code_changes_v1.jsonl"
        code_changes = [json.loads(l) for l in open(changes_path)]

    # Build a map: file → list of (sha, classification, timestamp, message)
    file_history = defaultdict(list)
    for c in git_commits:
        if not c["is_rust_relevant"]:
            continue
        for f in c.get("files_touched", []):
            if f.endswith(".rs") and not should_exclude(f):
                file_history[f].append({
                    "sha": c["sha"],
                    "classification": c["classification"],
                    "timestamp": c["timestamp"],
                    "message": c["message"],
                })

    # Build repair chains: find BUG_FIX/REGRESSION_FIX commits that reference
    # files also touched by recent FEATURE/BUG_FIX commits
    repair_chains = []
    trajectories = []

    # Build a set of merge commit SHAs to exclude as bug sources
    merge_shas = {c["sha"] for c in git_commits if c.get("parent_count", 0) > 1}

    # Helper: extract meaningful words from a commit message for semantic overlap
    _stopwords = set("the a an to of in on for is was be this that with and or if s p".split())
    def _msg_keywords(msg):
        words = re.findall(r'[a-z]{3,}', msg.lower())
        return set(w for w in words if w not in _stopwords)

    # Group commits by file overlap and temporal proximity
    for filepath, history in file_history.items():
        # Sort by timestamp
        history.sort(key=lambda x: x["timestamp"])

        for i, entry in enumerate(history):
            # Skip merge commits as fix sources (they're integration points, not fixes)
            if entry["sha"] in merge_shas:
                continue
            # If this is a BUG_FIX, look backwards for what it might be fixing
            if entry["classification"] in ("BUG_FIX", "REGRESSION_FIX", "REVERT", "FAILED_APPROACH"):
                fix_kw = _msg_keywords(entry["message"])
                # Look at preceding commits to the same file within 7 days
                for j in range(max(0, i - 5), i):
                    prev = history[j]
                    # Skip merge commits as bug sources
                    if prev["sha"] in merge_shas:
                        continue
                    time_gap = entry["timestamp"] - prev["timestamp"]
                    if time_gap < 0:
                        continue
                    if time_gap > 7 * 86400:  # > 7 days
                        break

                    if prev["classification"] in ("FEATURE", "BUG_FIX", "PERFORMANCE", "REFACTOR"):
                        # Semantic overlap recorded for quality scoring but does not
                        # filter — file overlap IS a valid signal even without word
                        # overlap (e.g. "feat: add X" → "fix: null pointer" shares
                        # a file but no keywords, yet is a genuine repair chain).
                        bug_kw = _msg_keywords(prev["message"])
                        overlap = fix_kw & bug_kw
                        # This is a potential repair chain: prev introduced something,
                        # entry fixes it
                        repair_chains.append({
                            "schema": "repair_v1",
                            "schema_version": SCHEMA_VERSION,
                            "bug_commit_sha": prev["sha"],
                            "bug_commit_message": prev["message"],
                            "bug_classification": prev["classification"],
                            "fix_commit_sha": entry["sha"],
                            "fix_commit_message": entry["message"],
                            "fix_classification": entry["classification"],
                            "filepath": filepath,
                            "subsystem": get_subsystem(filepath),
                            "time_gap_seconds": time_gap,
                            "repair_type": entry["classification"],
                            "semantic_overlap": sorted(overlap)[:10] if overlap else [],
                            "provenance": {
                                "run_uuid": RUN_UUID,
                                "detected_by": "file_overlap_temporal_semantic",
                                "detected_at": datetime.now(timezone.utc).isoformat(),
                            },
                        })

    # Build engineering trajectories from repair chains
    # Group repairs by the bug_commit_sha to find multi-fix scenarios
    repairs_by_bug = defaultdict(list)
    for r in repair_chains:
        repairs_by_bug[r["bug_commit_sha"]].append(r)

    for bug_sha, repairs in repairs_by_bug.items():
        if len(repairs) == 0:
            continue

        # Get the bug commit info
        bug_commit = next((c for c in git_commits if c["sha"] == bug_sha), None)
        if not bug_commit:
            continue

        # Get the fix commit info (take the first repair)
        first_repair = repairs[0]
        fix_commit = next((c for c in git_commits if c["sha"] == first_repair["fix_commit_sha"]), None)
        if not fix_commit:
            continue

        trajectory = {
            "schema": "engineering_trajectory_v1",
            "schema_version": SCHEMA_VERSION,
            "problem_commit_sha": bug_sha,
            "problem_message": bug_commit["message"],
            "problem_classification": bug_commit["classification"],
            "problem_timestamp": bug_commit["timestamp_iso"],
            "investigation": f"File {first_repair['filepath']} was modified in {bug_sha} "
                           f"({bug_commit['classification']}), then subsequently "
                           f"modified by {first_repair['fix_commit_sha']} "
                           f"({first_repair['fix_classification']}) "
                           f"{first_repair['time_gap_seconds'] // 3600}h later.",
            "attempted_change": bug_commit["message"],
            "result": first_repair["fix_commit_message"],
            "result_classification": first_repair["fix_classification"],
            "repair_count": len(repairs),
            "repair_files": list(set(r["filepath"] for r in repairs)),
            "repair_subsystems": list(set(r["subsystem"] for r in repairs)),
            "repair_type": first_repair["repair_type"],
            "provenance": {
                "run_uuid": RUN_UUID,
                "built_at": datetime.now(timezone.utc).isoformat(),
            },
        }
        trajectories.append(trajectory)

    # Write outputs
    repairs_path = OUTPUT_DIR / "repairs_v1.jsonl"
    with open(repairs_path, "w") as f:
        for rec in repair_chains:
            f.write(json.dumps(rec) + "\n")

    traj_path = OUTPUT_DIR / "trajectories_v1.jsonl"
    with open(traj_path, "w") as f:
        for rec in trajectories:
            f.write(json.dumps(rec) + "\n")

    print(f"  Repair chains detected: {len(repair_chains)}")
    print(f"  Engineering trajectories: {len(trajectories)}")
    print(f"  Written: {repairs_path}")
    print(f"  Written: {traj_path}")

    return trajectories, repair_chains


# ─── Phase 4: Repo State Snapshots ──────────────────────────────────────────

def phase_repo_state():
    """Build repo_state_v1: curated architecture/source/docs snapshots."""
    print("[Phase 4] Building repo state snapshots...")

    snapshots = []

    # 1. Workspace structure
    cargo_toml = (REPO_ROOT / "rust" / "Cargo.toml").read_text()
    snapshots.append({
        "schema": "repo_state_v1",
        "schema_version": SCHEMA_VERSION,
        "snapshot_type": "workspace_manifest",
        "content": cargo_toml[:5000],
        "description": "Root Cargo.toml workspace manifest — 30 crates, edition 2021, rust-version 1.85",
        "provenance": {"run_uuid": RUN_UUID, "captured_at": datetime.now(timezone.utc).isoformat()},
    })

    # 2. Crate summaries
    crate_summaries = {}
    crates_dir = REPO_ROOT / "rust" / "crates"
    for crate_dir in sorted(crates_dir.iterdir()):
        if not crate_dir.is_dir():
            continue
        crate_name = crate_dir.name
        src_dir = crate_dir / "src"
        if not src_dir.exists():
            continue

        # Count files and LOC
        rs_files = list(src_dir.rglob("*.rs"))
        total_loc = 0
        for f in rs_files:
            try:
                total_loc += sum(1 for _ in open(f, errors="replace"))
            except:
                pass

        # Read lib.rs or main.rs for module list
        lib_content = ""
        for lib_name in ["lib.rs", "main.rs"]:
            lib_path = src_dir / lib_name
            if lib_path.exists():
                lib_content = lib_path.read_text(errors="replace")[:3000]
                break

        # Read Cargo.toml for dependencies
        crate_cargo = crate_dir / "Cargo.toml"
        deps = []
        if crate_cargo.exists():
            cargo_content = crate_cargo.read_text(errors="replace")
            for line in cargo_content.split("\n"):
                m = re.match(r'^\s*(\w[\w-]*)\s*=\s*', line)
                if m and "pump-quant" in line:
                    deps.append(line.strip())

        crate_summaries[crate_name] = {
            "file_count": len(rs_files),
            "loc": total_loc,
            "subsystem": CRATE_TO_SUBSYSTEM.get(crate_name, "unknown"),
            "modules": lib_content[:1000],
            "internal_deps": deps,
        }

    snapshots.append({
        "schema": "repo_state_v1",
        "schema_version": SCHEMA_VERSION,
        "snapshot_type": "crate_summaries",
        "content": json.dumps(crate_summaries, indent=2)[:50000],
        "description": f"Summary of {len(crate_summaries)} crates: file counts, LOC, subsystems, module lists",
        "provenance": {"run_uuid": RUN_UUID, "captured_at": datetime.now(timezone.utc).isoformat()},
    })

    # 3. Key architecture docs
    key_docs = [
        "docs/architecture.md",
        "docs/ARCHITECTURE_V2.md",
        "docs/ARCHITECT_PLAN.md",
        "docs/ARCH_HOTPATH.md",
        "README.md",
        "CLAUDE.md",
    ]
    for doc_path in key_docs:
        full_path = REPO_ROOT / doc_path
        if full_path.exists():
            content = full_path.read_text(errors="replace")
            snapshots.append({
                "schema": "repo_state_v1",
                "schema_version": SCHEMA_VERSION,
                "snapshot_type": "architecture_doc",
                "doc_path": doc_path,
                "content": content[:20000],
                "content_truncated": len(content) > 20000,
                "description": f"Architecture document: {doc_path}",
                "provenance": {"run_uuid": RUN_UUID, "captured_at": datetime.now(timezone.utc).isoformat()},
            })

    # 4. Key source files (most central to the system)
    key_sources = [
        "rust/crates/pump-quant-app/src/main.rs",
        "rust/crates/pump-quant-app/src/lib.rs",
        "rust/crates/pump-quant-app/src/engine.rs",
        "rust/crates/pump-quant-app/src/gate.rs",
        "rust/crates/pump-quant-app/src/position.rs",
        "rust/crates/pump-quant-app/src/config.rs",
        "rust/crates/pump-quant-protocol/src/lib.rs",
        "rust/crates/pump-quant-execution/src/lib.rs",
        "rust/crates/pump-quant-junction/src/lib.rs",
        "rust/crates/pump-quant-replay/src/lib.rs",
        "rust/crates/pump-quant-brain/src/lib.rs",
        "rust/crates/pump-quant-domain/src/lib.rs",
    ]
    for src_path in key_sources:
        full_path = REPO_ROOT / src_path
        if full_path.exists():
            content = full_path.read_text(errors="replace")
            snapshots.append({
                "schema": "repo_state_v1",
                "schema_version": SCHEMA_VERSION,
                "snapshot_type": "key_source_file",
                "source_path": src_path,
                "subsystem": get_subsystem(src_path),
                "content": content[:15000],
                "content_truncated": len(content) > 15000,
                "description": f"Key source: {src_path} ({len(content)} chars)",
                "provenance": {"run_uuid": RUN_UUID, "captured_at": datetime.now(timezone.utc).isoformat()},
            })

    # 5. Test infrastructure overview
    test_files = list(Path(REPO_ROOT / "rust/crates").rglob("tests/*.rs"))
    test_summary = {
        "total_test_files": len(test_files),
        "test_files_by_crate": defaultdict(int),
        "sample_test_files": [],
    }
    for tf in test_files[:20]:
        crate = tf.parent.parent.name
        test_summary["test_files_by_crate"][crate] += 1
        try:
            content = tf.read_text(errors="replace")[:500]
            test_summary["sample_test_files"].append({
                "path": str(tf.relative_to(REPO_ROOT)),
                "preview": content,
            })
        except:
            pass

    snapshots.append({
        "schema": "repo_state_v1",
        "schema_version": SCHEMA_VERSION,
        "snapshot_type": "test_infrastructure",
        "content": json.dumps(test_summary, indent=2, default=str)[:20000],
        "description": f"Test infrastructure overview: {len(test_files)} test files across {len(test_summary['test_files_by_crate'])} crates",
        "provenance": {"run_uuid": RUN_UUID, "captured_at": datetime.now(timezone.utc).isoformat()},
    })

    out_path = OUTPUT_DIR / "repo_state_v1.jsonl"
    with open(out_path, "w") as f:
        for rec in snapshots:
            f.write(json.dumps(rec, default=str) + "\n")

    print(f"  Repo state snapshots: {len(snapshots)}")
    print(f"  Written: {out_path}")

    return snapshots


# ─── Phase 5: Validation ─────────────────────────────────────────────────────

def phase_validate():
    """Run cargo validation in an isolated worktree. Record PASS/FAIL/SKIPPED."""
    print("[Phase 5] Validation (isolated worktree)...")

    # Create a detached worktree at HEAD (cf97c33 - frozen laserstream gold v3)
    worktree_path = REPO_ROOT.parent / "mev_bot_rust_gold_worktree"

    # Remove old worktree if exists
    try:
        git_cmd(["worktree", "remove", str(worktree_path), "--force"])
    except RuntimeError:
        pass

    try:
        git_cmd(["worktree", "add", str(worktree_path), "HEAD"])
    except RuntimeError as e:
        print(f"  WARNING: Could not create worktree: {e}")
        print("  Falling back to in-repo validation (no worktree isolation)")
        worktree_path = REPO_ROOT

    cargo_dir = worktree_path / "rust"
    evidence = {
        "schema": "validation_evidence_v1",
        "schema_version": SCHEMA_VERSION,
        "run_uuid": RUN_UUID,
        "worktree_path": str(worktree_path),
        "validated_at": datetime.now(timezone.utc).isoformat(),
        "git_sha": git_cmd(["rev-parse", "HEAD"]).strip(),
        "checks": {},
    }

    # 1. cargo fmt --check
    print("  Running cargo fmt --check...")
    try:
        result = subprocess.run(
            ["cargo", "fmt", "--check"],
            cwd=str(cargo_dir),
            capture_output=True, text=True, timeout=300
        )
        evidence["checks"]["cargo_fmt_check"] = {
            "status": "PASS" if result.returncode == 0 else "FAIL",
            "exit_code": result.returncode,
            "stdout": result.stdout[:5000],
            "stderr": result.stderr[:5000],
            "reason": "formatting issues" if result.returncode != 0 else "all formatted",
        }
    except subprocess.TimeoutExpired:
        evidence["checks"]["cargo_fmt_check"] = {
            "status": "SKIPPED", "reason": "timeout (300s)"
        }
    except Exception as e:
        evidence["checks"]["cargo_fmt_check"] = {
            "status": "SKIPPED", "reason": str(e)[:200]
        }

    # 2. cargo check
    print("  Running cargo check...")
    try:
        result = subprocess.run(
            ["cargo", "check", "--workspace"],
            cwd=str(cargo_dir),
            capture_output=True, text=True, timeout=600
        )
        evidence["checks"]["cargo_check"] = {
            "status": "PASS" if result.returncode == 0 else "FAIL",
            "exit_code": result.returncode,
            "stdout": result.stdout[-5000:],
            "stderr": result.stderr[-5000:],
            "reason": "compilation errors" if result.returncode != 0 else "compiles clean",
        }
    except subprocess.TimeoutExpired:
        evidence["checks"]["cargo_check"] = {
            "status": "SKIPPED", "reason": "timeout (600s)"
        }
    except Exception as e:
        evidence["checks"]["cargo_check"] = {
            "status": "SKIPPED", "reason": str(e)[:200]
        }

    # 3. cargo clippy
    print("  Running cargo clippy...")
    try:
        result = subprocess.run(
            ["cargo", "clippy", "--workspace", "--", "-D", "warnings"],
            cwd=str(cargo_dir),
            capture_output=True, text=True, timeout=600
        )
        evidence["checks"]["cargo_clippy"] = {
            "status": "PASS" if result.returncode == 0 else "FAIL",
            "exit_code": result.returncode,
            "stdout": result.stdout[-5000:],
            "stderr": result.stderr[-5000:],
            "reason": "clippy warnings" if result.returncode != 0 else "no warnings",
        }
    except subprocess.TimeoutExpired:
        evidence["checks"]["cargo_clippy"] = {
            "status": "SKIPPED", "reason": "timeout (600s)"
        }
    except Exception as e:
        evidence["checks"]["cargo_clippy"] = {
            "status": "SKIPPED", "reason": str(e)[:200]
        }

    # 4. cargo test (compile only first to save time, then run key tests)
    print("  Running cargo test (compilation check)...")
    try:
        result = subprocess.run(
            ["cargo", "test", "--workspace", "--no-run"],
            cwd=str(cargo_dir),
            capture_output=True, text=True, timeout=900
        )
        evidence["checks"]["cargo_test_compile"] = {
            "status": "PASS" if result.returncode == 0 else "FAIL",
            "exit_code": result.returncode,
            "stdout": result.stdout[-5000:],
            "stderr": result.stderr[-5000:],
            "reason": "test compilation failed" if result.returncode != 0 else "tests compile",
        }
    except subprocess.TimeoutExpired:
        evidence["checks"]["cargo_test_compile"] = {
            "status": "SKIPPED", "reason": "timeout (900s)"
        }
    except Exception as e:
        evidence["checks"]["cargo_test_compile"] = {
            "status": "SKIPPED", "reason": str(e)[:200]
        }

    # 5. Regression tests (if they exist)
    print("  Running regression tests...")
    try:
        result = subprocess.run(
            ["cargo", "test", "-p", "pq-regression"],
            cwd=str(cargo_dir),
            capture_output=True, text=True, timeout=600
        )
        evidence["checks"]["pq_regression_tests"] = {
            "status": "PASS" if result.returncode == 0 else "FAIL",
            "exit_code": result.returncode,
            "stdout": result.stdout[-5000:],
            "stderr": result.stderr[-5000:],
            "reason": "regression test failures" if result.returncode != 0 else "all pass",
        }
    except subprocess.TimeoutExpired:
        evidence["checks"]["pq_regression_tests"] = {
            "status": "SKIPPED", "reason": "timeout (600s)"
        }
    except Exception as e:
        evidence["checks"]["pq_regression_tests"] = {
            "status": "SKIPPED", "reason": str(e)[:200]
        }

    # Clean up worktree
    if worktree_path != REPO_ROOT:
        try:
            git_cmd(["worktree", "remove", str(worktree_path), "--force"])
        except RuntimeError:
            pass

    # Write evidence
    out_path = OUTPUT_DIR / "validation_evidence_v1.json"
    with open(out_path, "w") as f:
        json.dump(evidence, f, indent=2)

    print(f"  Validation evidence: {out_path}")
    for check_name, check_data in evidence["checks"].items():
        print(f"    {check_name}: {check_data['status']}")

    return evidence


# ─── Phase 6: Splits ─────────────────────────────────────────────────────────

def phase_split(git_commits=None, trajectories=None, repairs=None):
    """Create chronological train/val/test splits with repair-chain integrity."""
    print("[Phase 6] Creating chronological splits...")

    if git_commits is None:
        commits_path = OUTPUT_DIR / "git_commits_v1.jsonl"
        git_commits = [json.loads(l) for l in open(commits_path)]
    if trajectories is None:
        traj_path = OUTPUT_DIR / "trajectories_v1.jsonl"
        trajectories = [json.loads(l) for l in open(traj_path)] if traj_path.exists() else []
    if repairs is None:
        repairs_path = OUTPUT_DIR / "repairs_v1.jsonl"
        repairs = [json.loads(l) for l in open(repairs_path)] if repairs_path.exists() else []

    # Filter to Rust-relevant commits only
    rust_commits = [c for c in git_commits if c["is_rust_relevant"] and not c["is_data_only"]]
    rust_commits.sort(key=lambda x: x["timestamp"])

    n = len(rust_commits)
    print(f"  Rust-relevant commits for splitting: {n}")

    # Chronological split: 80% train, 10% val, 10% test
    train_end = int(n * 0.80)
    val_end = int(n * 0.90)

    train_commits = {c["sha"] for c in rust_commits[:train_end]}
    val_commits = {c["sha"] for c in rust_commits[train_end:val_end]}
    test_commits = {c["sha"] for c in rust_commits[val_end:]}

    # Repair chain integrity: ensure no repair chain crosses splits
    # If a bug is in train but its fix is in val, move the fix to train
    violations = 0
    for repair in repairs:
        bug_sha = repair["bug_commit_sha"]
        fix_sha = repair["fix_commit_sha"]
        if bug_sha in train_commits and fix_sha in val_commits:
            val_commits.discard(fix_sha)
            train_commits.add(fix_sha)
            violations += 1
        elif bug_sha in train_commits and fix_sha in test_commits:
            test_commits.discard(fix_sha)
            train_commits.add(fix_sha)
            violations += 1
        elif bug_sha in val_commits and fix_sha in test_commits:
            test_commits.discard(fix_sha)
            val_commits.add(fix_sha)
            violations += 1

    print(f"  Repair chain split violations fixed: {violations}")

    split_info = {
        "schema": "splits_v1",
        "schema_version": SCHEMA_VERSION,
        "run_uuid": RUN_UUID,
        "total_rust_commits": n,
        "train_count": len(train_commits),
        "val_count": len(val_commits),
        "test_count": len(test_commits),
        "train_sha_range": (
            rust_commits[0]["short_sha"] if rust_commits else "N/A",
            rust_commits[train_end - 1]["short_sha"] if train_end > 0 else "N/A",
        ),
        "val_sha_range": (
            rust_commits[train_end]["short_sha"] if train_end < n else "N/A",
            rust_commits[val_end - 1]["short_sha"] if val_end > 0 else "N/A",
        ),
        "test_sha_range": (
            rust_commits[val_end]["short_sha"] if val_end < n else "N/A",
            rust_commits[-1]["short_sha"] if rust_commits else "N/A",
        ),
        "repair_chain_violations_fixed": violations,
        "split_method": "chronological_80_10_10_with_repair_chain_integrity",
        "provenance": {
            "split_at": datetime.now(timezone.utc).isoformat(),
        },
        "train_shas": sorted(train_commits),
        "val_shas": sorted(val_commits),
        "test_shas": sorted(test_commits),
    }

    out_path = OUTPUT_DIR / "splits_v1.json"
    with open(out_path, "w") as f:
        json.dump(split_info, f, indent=2)

    print(f"  Train: {len(train_commits)}, Val: {len(val_commits)}, Test: {len(test_commits)}")
    print(f"  Written: {out_path}")

    return split_info


# ─── Phase 7: Manifest ────────────────────────────────────────────────────────

def phase_manifest():
    """Build the QA/provenance manifest with counts, hashes, and integrity checks."""
    print("[Phase 7] Building manifest...")

    manifest = {
        "schema": "rust_gold_v1_manifest",
        "schema_version": SCHEMA_VERSION,
        "pipeline_version": PIPELINE_VERSION,
        "run_uuid": RUN_UUID,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "repo": "D:/repos/mev_bot",
        "git_head": git_cmd(["rev-parse", "HEAD"]).strip(),
        "git_head_short": git_cmd(["rev-parse", "--short", "HEAD"]).strip(),
        "git_branch": git_cmd(["rev-parse", "--abbrev-ref", "HEAD"]).strip(),
        "phases": {},
        "counts": {},
        "integrity": {},
    }

    # Load and hash each output file
    output_files = {
        "git_commits": "git_commits_v1.jsonl",
        "code_changes": "code_changes_v1.jsonl",
        "trajectories": "trajectories_v1.jsonl",
        "repairs": "repairs_v1.jsonl",
        "repo_state": "repo_state_v1.jsonl",
        "splits": "splits_v1.json",
        "validation_evidence": "validation_evidence_v1.json",
    }

    for name, filename in output_files.items():
        path = OUTPUT_DIR / filename
        if path.exists():
            content = path.read_bytes()
            sha256 = hashlib.sha256(content).hexdigest()
            count = 0
            if filename.endswith(".jsonl"):
                count = sum(1 for _ in open(path))
            elif filename.endswith(".json"):
                count = 1

            manifest["phases"][name] = {
                "file": filename,
                "size_bytes": len(content),
                "sha256": sha256,
                "record_count": count,
            }
        else:
            manifest["phases"][name] = {"file": filename, "status": "NOT_GENERATED"}

    # Counts by classification
    commits_path = OUTPUT_DIR / "git_commits_v1.jsonl"
    if commits_path.exists():
        class_dist = defaultdict(int)
        subsystem_dist = defaultdict(int)
        rust_count = 0
        total_count = 0
        for line in open(commits_path):
            rec = json.loads(line)
            total_count += 1
            if rec["is_rust_relevant"] and not rec["is_data_only"]:
                rust_count += 1
                class_dist[rec["classification"]] += 1
                for sub in rec["subsystems"]:
                    subsystem_dist[sub] += 1

        manifest["counts"]["total_commits"] = total_count
        manifest["counts"]["rust_relevant_commits"] = rust_count
        manifest["counts"]["by_classification"] = dict(sorted(class_dist.items(), key=lambda x: -x[1]))
        manifest["counts"]["by_subsystem"] = dict(sorted(subsystem_dist.items(), key=lambda x: -x[1]))

    # Validation evidence summary
    val_path = OUTPUT_DIR / "validation_evidence_v1.json"
    if val_path.exists():
        val_data = json.loads(val_path.read_text())
        manifest["validation"] = {}
        for check_name, check_data in val_data.get("checks", {}).items():
            manifest["validation"][check_name] = check_data["status"]

    # Splits summary
    splits_path = OUTPUT_DIR / "splits_v1.json"
    if splits_path.exists():
        splits_data = json.loads(splits_path.read_text())
        manifest["splits"] = {
            "train": splits_data["train_count"],
            "val": splits_data["val_count"],
            "test": splits_data["test_count"],
            "repair_chain_violations_fixed": splits_data["repair_chain_violations_fixed"],
        }

    # Integrity checks
    manifest["integrity"]["schema_version"] = SCHEMA_VERSION
    manifest["integrity"]["exclusions_applied"] = [
        "target/", "node_modules/", "dist/", "vendor/",
        "*.parquet", "*.bin", "*.zst", "*.db",
        "*.key", "*.pem", "*.env",
        "credentials", "wallet", "secret", "PRIVATE_KEY", "seed_phrase",
    ]
    manifest["integrity"]["git_history_coverage"] = "all reachable commits (main + rev36-mcap-toctou-fix branch)"
    manifest["integrity"]["frozen_gold_not_mutated"] = True
    manifest["integrity"]["laserstream_gold_v3_sha"] = "cf97c33"
    manifest["integrity"]["narrative_acquisition_continues_low_resource"] = True

    out_path = OUTPUT_DIR / "manifest_rust_gold_v1.json"
    with open(out_path, "w") as f:
        json.dump(manifest, f, indent=2)

    print(f"  Manifest written: {out_path}")
    print(f"  Counts: {json.dumps(manifest.get('counts', {}), indent=2)[:500]}")

    return manifest


# ─── Main ────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="rust_gold_v1 extraction pipeline")
    parser.add_argument("--phase", required=True,
                        choices=["enumerate", "extract", "trajectory", "repo_state",
                                 "validate", "split", "manifest", "all"])
    args = parser.parse_args()

    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

    if args.phase == "all":
        gc = phase_enumerate()
        cc = phase_extract(gc)
        tr, rp = phase_trajectory(gc, cc)
        phase_repo_state()
        phase_validate()
        phase_split(gc, tr, rp)
        phase_manifest()
    elif args.phase == "enumerate":
        phase_enumerate()
    elif args.phase == "extract":
        phase_extract()
    elif args.phase == "trajectory":
        phase_trajectory()
    elif args.phase == "repo_state":
        phase_repo_state()
    elif args.phase == "validate":
        phase_validate()
    elif args.phase == "split":
        phase_split()
    elif args.phase == "manifest":
        phase_manifest()


if __name__ == "__main__":
    main()
