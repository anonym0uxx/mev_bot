#!/bin/bash
# Test matrix for pre-push guard v2. Five cases, all required.
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

echo "===================================="
echo "RESULTS: $PASS passed, $FAIL failed"
echo "===================================="

cd /d/repos/mev_bot
rm -rf "$TMPDIR"
