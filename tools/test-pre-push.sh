#!/bin/bash
# Test matrix for pre-push guard v4. Seven cases, all required.
# Each case reports: exit code + stderr content.
set -u
GUARD_ABS="$1"
TMPDIR=$(mktemp -d)
PASS=0
FAIL=0
# Test credential: assembled from variables so no single source line
# contains a credential-shaped literal (which the guard would flag).
_p1="9999999999"
_p2="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123"
FAKE_SECRET="${_p1}:${_p2}"
ZERO="0000000000000000000000000000000000000000"

# Create a fresh test repo
new_repo() {
  local name="$1"
  local repo="$TMPDIR/$name"
  rm -rf "$repo"
  mkdir -p "$repo/.githooks"
  cp "$GUARD_ABS" "$repo/.githooks/pre-push"
  cd "$repo"
  git init -q
  git config user.email "test@test.com"
  git config user.name "test"
  git config core.hooksPath ".githooks"
}

# Run the guard with simulated stdin
run_guard() {
  local local_sha="$1"
  local remote_sha="$2"
  printf "refs/heads/test %s refs/remotes/origin/test %s\n" "$local_sha" "$remote_sha" \
    | bash .githooks/pre-push 2>&1 1>/dev/null
  return $?
}

report() {
  local num="$1" desc="$2" expect="$3" exit_code="$4" stderr="$5"
  if [ "$expect" = "BLOCK" ]; then
    if [ "$exit_code" -eq 1 ] && echo "$stderr" | grep -q "BLOCKED"; then
      echo "CASE $num: $desc — PASS (exit=$exit_code, BLOCKED on stderr)"
      PASS=$((PASS+1))
    else
      echo "CASE $num: $desc — FAIL (expected BLOCK, got exit=$exit_code, stderr=$stderr)"
      FAIL=$((FAIL+1))
    fi
  elif [ "$expect" = "WARN" ]; then
    if [ "$exit_code" -eq 0 ] && echo "$stderr" | grep -q "WARNING"; then
      echo "CASE $num: $desc — PASS (exit=$exit_code, WARNING on stderr, not blocked)"
      PASS=$((PASS+1))
    else
      echo "CASE $num: $desc — FAIL (expected WARN exit=0, got exit=$exit_code, stderr=$stderr)"
      FAIL=$((FAIL+1))
    fi
  else
    if [ "$exit_code" -eq 0 ] && [ -z "$stderr" ]; then
      echo "CASE $num: $desc — PASS (exit=0, clean stderr)"
      PASS=$((PASS+1))
    else
      echo "CASE $num: $desc — FAIL (expected exit=0 clean, got exit=$exit_code, stderr=$stderr)"
      FAIL=$((FAIL+1))
    fi
  fi
  echo ""
}

# ─── Case 1: remote_sha zeros, credential in a new commit → BLOCK ───
new_repo "case1"
echo "clean" > f1.txt; git add f1.txt; git commit -q -m "clean base"
echo "HELIUS_API_KEY=$FAKE_SECRET" > secret.txt; git add secret.txt; git commit -q -m "add credential"
stderr=$(run_guard "$(git rev-parse HEAD)" "$ZERO")
report 1 "remote_sha zeros, credential in new commit" "BLOCK" $? "$stderr"

# ─── Case 2: remote_sha zeros, all new commits clean → PASS ───
new_repo "case2"
echo "clean" > f1.txt; git add f1.txt; git commit -q -m "clean A"
echo "more clean" > f2.txt; git add f2.txt; git commit -q -m "clean B"
stderr=$(run_guard "$(git rev-parse HEAD)" "$ZERO")
report 2 "remote_sha zeros, all new commits clean" "PASS" $? "$stderr"

# ─── Case 3: remote_sha real, credential in range → BLOCK ───
new_repo "case3"
echo "clean" > f1.txt; git add f1.txt; git commit -q -m "base"
base_sha=$(git rev-parse HEAD)
echo "HELIUS_API_KEY=$FAKE_SECRET" > secret.txt; git add secret.txt; git commit -q -m "add credential"
stderr=$(run_guard "$(git rev-parse HEAD)" "$base_sha")
report 3 "remote_sha real, credential in range" "BLOCK" $? "$stderr"

# ─── Case 4: remote_sha real, clean range → PASS ───
new_repo "case4"
echo "clean" > f1.txt; git add f1.txt; git commit -q -m "base"
base_sha=$(git rev-parse HEAD)
echo "more clean" > f2.txt; git add f2.txt; git commit -q -m "clean on top"
stderr=$(run_guard "$(git rev-parse HEAD)" "$base_sha")
report 4 "remote_sha real, clean range" "PASS" $? "$stderr"

# ─── Case 5: credential in TREE at tip, NOT in any new commit's diff → BLOCK ───
# Credential committed long ago, never touched. New commits don't touch it.
# Diff-only scanning misses it; the tree scan catches it.
new_repo "case5"
echo "HELIUS_API_KEY=$FAKE_SECRET" > config.txt; git add config.txt; git commit -q -m "initial with credential"
echo "clean feature" > feature.txt; git add feature.txt; git commit -q -m "clean feature on top"
stderr=$(run_guard "$(git rev-parse HEAD)" "$ZERO")
report 5 "credential in tree, not in new commit diff" "BLOCK" $? "$stderr"

# ─── Case 6: allowlisted GUID AND planted credential on SAME line → BLOCK ───
# Positive control for the allowlist itself: blank_benign must blank the
# GUID but NOT the credential on the same line. If grep -v were used
# instead of blank_benign, this line would be suppressed entirely.
new_repo "case6"
# Build the credential from parts so the test script itself is not flagged.
echo "GUID=258EAFA5-E914-47DA-95CA-C5AB0DC85B11 HELIUS_API_KEY=${_p1}:${_p2}" > combo.txt
git add combo.txt; git commit -q -m "benign GUID + credential on same line"
stderr=$(run_guard "$(git rev-parse HEAD)" "$ZERO")
report 6 "allowlisted GUID + credential on same line" "BLOCK" $? "$stderr"

# ─── Case 7: credential ALREADY reachable from a remote → WARN, exit 0 ───
# Positive control for v4 --not --remotes: a commit already reachable from
# a remote ref must NOT block. It must emit a WARNING naming the SHA and exit 0.
# Simulates: credential commit was pushed to a task branch (origin/task4),
# now pushing main which contains the same commit. The commit is already
# reachable from origin/task4, so --not --remotes excludes it from the
# "new publication" set. The tree at tip does NOT contain the credential
# (it was scrubbed in a later commit), so the tree scan does not fire.
new_repo "case7"
git branch -m test  # rename default branch to test so push works
echo "clean" > f1.txt; git add f1.txt; git commit -q -m "clean base"

# Create a bare remote
remote_repo="$TMPDIR/case7_remote"
rm -rf "$remote_repo"
git init -q --bare "$remote_repo"
git remote add origin "$remote_repo" 2>/dev/null

# Add credential, push to remote on branch "test" (simulating task branch push)
echo "HELIUS_API_KEY=$FAKE_SECRET" > secret.txt; git add secret.txt
git commit -q -m "add credential"
# Bypass the guard for the setup push — we NEED the credential on the remote
# to simulate the "already published" scenario. The guard correctly blocks
# this push; we override with PQ_ALLOW_SECRET=1 as the operator would.
PQ_ALLOW_SECRET=1 git push -q origin test 2>/dev/null

# Fetch so remote-tracking ref exists for --not --remotes
git fetch -q origin 2>/dev/null

# Now scrub the credential and commit on top
echo "HELIUS_API_KEY=\${HELIUS_API_KEY}" > secret.txt; git add secret.txt
git commit -q -m "scrub credential"

# Push the scrub commit too so origin/test is up to date
PQ_ALLOW_SECRET=1 git push -q origin test 2>/dev/null
git fetch -q origin 2>/dev/null

# Now simulate pushing a NEW branch "main" that contains both commits.
# The remote has origin/test but NOT origin/main.
# --not --remotes: both commits are reachable from origin/test, so NEITHER
# is "new". The guard should WARN on the credential commit and NOT block.
# The tree at tip has the scrubbed version, so the tree scan is clean.
# We pass remote_sha as ZERO (new branch) to simulate first push of main.
stderr=$(run_guard "$(git rev-parse HEAD)" "$ZERO")
report 7 "credential already reachable from remote (scrubbed from tree)" "WARN" $? "$stderr"

echo "===================================="
echo "RESULTS: $PASS passed, $FAIL failed"
echo "===================================="

cd /d/repos/mev_bot
rm -rf "$TMPDIR"
