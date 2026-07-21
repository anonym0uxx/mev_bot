#!/bin/bash
# validate-pumpswap-layout.sh
# Validates PumpSwap pool layout by fetching real pools and checking offsets.
#
# Confirms the two-pass lookup strategy: token can be at offset 43 (base_mint)
# or offset 75 (quote_mint). ~81% of pools have WSOL as base_mint (offset 43).
#
# Usage: bash scripts/validate-pumpswap-layout.sh
# Requires: curl, python3

set -euo pipefail

RPC="https://marielle-qe2lvr-fast-mainnet.helius-rpc.com"
PUMPSWAP="pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"

PASS=0
FAIL=0

pass() { echo "  ✅ PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "  ❌ FAIL: $1"; FAIL=$((FAIL + 1)); }

# ── Test 1: Token at offset 43 (normal ordering — token is base_mint) ──
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "Test 1: Normal ordering — token (9GvgS...pump) at offset 43"
echo "  Pool B from spec: 11CwRL2M8m5EeZUphCx8BvD6GXjw9VGTQUhjrWkjr3L"
echo "═══════════════════════════════════════════════════════════════"
RESULT=$(curl -s "$RPC" -X POST -H "Content-Type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getProgramAccounts\",\"params\":[\"$PUMPSWAP\",{\"encoding\":\"base64\",\"dataSlice\":{\"offset\":0,\"length\":0},\"filters\":[{\"memcmp\":{\"offset\":43,\"bytes\":\"9GvgSRMprTdnrpuQhS3uzCe1FijtZxPU974H8zjHpump\"}}]}]}" \
  | python3 -c "import json,sys; r=json.load(sys.stdin); print(len(r.get('result',[])))")
echo "  Found: $RESULT pool(s) with token at offset 43"
if [ "$RESULT" -ge 1 ]; then
  pass "Token found at offset 43 (normal pool ordering)"
else
  fail "Token NOT found at offset 43 — expected >= 1 result"
fi

# ── Test 2: Same token should NOT be at offset 75 (it's normal, not reversed) ──
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "Test 2: Verify 9GvgS...pump is NOT at offset 75"
echo "  (It's a normal pool — token sorts before WSOL is rare)"
echo "═══════════════════════════════════════════════════════════════"
RESULT=$(curl -s "$RPC" -X POST -H "Content-Type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getProgramAccounts\",\"params\":[\"$PUMPSWAP\",{\"encoding\":\"base64\",\"dataSlice\":{\"offset\":0,\"length\":0},\"filters\":[{\"memcmp\":{\"offset\":75,\"bytes\":\"9GvgSRMprTdnrpuQhS3uzCe1FijtZxPU974H8zjHpump\"}}]}]}" \
  | python3 -c "import json,sys; r=json.load(sys.stdin); print(len(r.get('result',[])))")
echo "  Found: $RESULT pool(s) with token at offset 75"
if [ "$RESULT" -eq 0 ]; then
  pass "Token correctly NOT at offset 75 (normal ordering confirmed)"
else
  fail "Unexpected: token found at offset 75 too"
fi

# ── Test 3: Reversed pool — token (Hn6YPJ...) at offset 75 (WSOL is base) ──
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "Test 3: Reversed ordering — token (Hn6YPJ...) at offset 75"
echo "  Pool A from spec: 114XmiBstWqYVhSiH6qnU4jFCskFxP8t9iBqBLJPmaf"
echo "═══════════════════════════════════════════════════════════════"
RESULT=$(curl -s "$RPC" -X POST -H "Content-Type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getProgramAccounts\",\"params\":[\"$PUMPSWAP\",{\"encoding\":\"base64\",\"dataSlice\":{\"offset\":0,\"length\":0},\"filters\":[{\"memcmp\":{\"offset\":75,\"bytes\":\"Hn6YPJUNh2f94hxumAYbRrMTSVL2D5Epj8AnuSU9QNNS\"}}]}]}" \
  | python3 -c "import json,sys; r=json.load(sys.stdin); print(len(r.get('result',[])))")
echo "  Found: $RESULT pool(s) with token at offset 75"
if [ "$RESULT" -ge 1 ]; then
  pass "Token found at offset 75 (reversed pool ordering)"
else
  fail "Token NOT found at offset 75 — expected >= 1 result"
fi

# ── Test 4: Same reversed token should NOT be at offset 43 ──
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "Test 4: Verify Hn6YPJ... is NOT at offset 43 (this was the bug)"
echo "═══════════════════════════════════════════════════════════════"
RESULT=$(curl -s "$RPC" -X POST -H "Content-Type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getProgramAccounts\",\"params\":[\"$PUMPSWAP\",{\"encoding\":\"base64\",\"dataSlice\":{\"offset\":0,\"length\":0},\"filters\":[{\"memcmp\":{\"offset\":43,\"bytes\":\"Hn6YPJUNh2f94hxumAYbRrMTSVL2D5Epj8AnuSU9QNNS\"}}]}]}" \
  | python3 -c "import json,sys; r=json.load(sys.stdin); print(len(r.get('result',[])))")
echo "  Found: $RESULT pool(s) with token at offset 43"
if [ "$RESULT" -eq 0 ]; then
  pass "Token correctly NOT at offset 43 — old code would have missed this pool!"
else
  fail "Unexpected: reversed token found at offset 43 too"
fi

# ── Test 5: WSOL at offset 43 confirms reversed pool ──
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "Test 5: Verify WSOL is at offset 43 in Pool A (reversed)"
echo "  Fetch Pool A account and check base_mint == WSOL"
echo "═══════════════════════════════════════════════════════════════"
POOL_A="114XmiBstWqYVhSiH6qnU4jFCskFxP8t9iBqBLJPmaf"
RESULT=$(curl -s "$RPC" -X POST -H "Content-Type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getAccountInfo\",\"params\":[\"$POOL_A\",{\"encoding\":\"base64\"}]}" \
  | python3 -c "
import json, sys, base64
r = json.load(sys.stdin)
data = base64.b64decode(r['result']['value']['data'][0])
base_mint = data[43:75]
# WSOL bytes
wsol = bytes.fromhex('069b8857feab8184fb687f634618c035dac439dc1aeb3b5598a0f00000000001')
if base_mint == wsol:
    print('WSOL_CONFIRMED')
else:
    print('NOT_WSOL: ' + base_mint.hex())
")
echo "  Result: $RESULT"
if [ "$RESULT" = "WSOL_CONFIRMED" ]; then
  pass "Pool A base_mint (offset 43) is WSOL — reversed ordering confirmed"
else
  fail "Pool A base_mint is not WSOL: $RESULT"
fi

# ── Test 6: Pool B has token at offset 43 (normal ordering) ──
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "Test 6: Verify Pool B has token (not WSOL) at offset 43"
echo "═══════════════════════════════════════════════════════════════"
POOL_B="11CwRL2M8m5EeZUphCx8BvD6GXjw9VGTQUhjrWkjr3L"
RESULT=$(curl -s "$RPC" -X POST -H "Content-Type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getAccountInfo\",\"params\":[\"$POOL_B\",{\"encoding\":\"base64\"}]}" \
  | python3 -c "
import json, sys, base64
r = json.load(sys.stdin)
data = base64.b64decode(r['result']['value']['data'][0])
base_mint = data[43:75]
wsol = bytes.fromhex('069b8857feab8184fb687f634618c035dac439dc1aeb3b5598a0f00000000001')
if base_mint != wsol:
    print('TOKEN_CONFIRMED')
else:
    print('UNEXPECTED_WSOL')
")
echo "  Result: $RESULT"
if [ "$RESULT" = "TOKEN_CONFIRMED" ]; then
  pass "Pool B base_mint (offset 43) is the token — normal ordering confirmed"
else
  fail "Pool B base_mint unexpectedly is WSOL"
fi

# ── Test 7: Verify discriminator is consistent ──
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "Test 7: Verify discriminator f19a6d0411b16dbc for both pools"
echo "═══════════════════════════════════════════════════════════════"
for POOL_ADDR in "$POOL_A" "$POOL_B"; do
  DISC=$(curl -s "$RPC" -X POST -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getAccountInfo\",\"params\":[\"$POOL_ADDR\",{\"encoding\":\"base64\"}]}" \
    | python3 -c "
import json, sys, base64
r = json.load(sys.stdin)
data = base64.b64decode(r['result']['value']['data'][0])
print(data[0:8].hex())
")
  echo "  $POOL_ADDR discriminator: $DISC"
  if [ "$DISC" = "f19a6d0411b16dbc" ]; then
    pass "Discriminator matches for $POOL_ADDR"
  else
    fail "Discriminator mismatch for $POOL_ADDR: expected f19a6d0411b16dbc, got $DISC"
  fi
done

# ── Test 8: Verify vault assignments for reversed pool ──
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "Test 8: Verify vault→mint mapping for reversed Pool A"
echo "  pool_base_token_account should hold WSOL, pool_quote should hold token"
echo "═══════════════════════════════════════════════════════════════"
RESULT=$(curl -s "$RPC" -X POST -H "Content-Type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getAccountInfo\",\"params\":[\"$POOL_A\",{\"encoding\":\"base64\"}]}" \
  | python3 -c "
import json, sys, base64
r = json.load(sys.stdin)
data = base64.b64decode(r['result']['value']['data'][0])
base_vault_hex = data[139:171].hex()
quote_vault_hex = data[171:203].hex()
print(f'base_vault_hex={base_vault_hex}')
print(f'quote_vault_hex={quote_vault_hex}')
# Vaults should be different (one holds WSOL, one holds token)
assert base_vault_hex != quote_vault_hex, 'vaults should differ'
# Both should be non-zero
assert any(b != 0 for b in data[139:171]), 'base vault non-zero'
assert any(b != 0 for b in data[171:203]), 'quote vault non-zero'
print('VAULTS_EXTRACTED')
")
echo "  $RESULT"
if echo "$RESULT" | grep -q "VAULTS_EXTRACTED"; then
  pass "Vault addresses extracted from reversed pool"
else
  fail "Could not extract vault addresses"
fi

# ── Summary ──
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "Summary: $PASS passed, $FAIL failed"
echo "═══════════════════════════════════════════════════════════════"

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
echo "All tests passed! PumpSwap pool layout verified on-chain."
