# Item 5 — On-Chain Layout Verification

**Date:** 2026-08-02  
**Method:** Query Helius RPC for real mainnet pump.fun and PumpSwap transactions, decode instruction account lists, diff positionally against builder output (`venue_accounts.rs`).

---

## Venue 1: pump.fun Bonding Curve (buy/sell)

### Builder Output

| Function | Accounts | Last Position |
|---|---|---|
| `pump_buy_accounts` | 17 | `[16]` bonding_curve_v2 (ro) — comment: "must be last" |
| `pump_sell_accounts` (non-cashback) | 15 | `[14]` bonding_curve_v2 (ro) |
| `pump_sell_accounts` (cashback) | 16 | `[15]` bonding_curve_v2 (ro) |

Builder comment (line 298): `bonding_curve_v2` "is not in the IDL's named list (it rides as a remaining account) and **must be last**."

### Real Mainnet Transactions

**Sample:** 42 pump.fun buy/sell instructions decoded across slots 436828370–436836103.

| Instruction | Builder Count | Real Count | Delta | Match? |
|---|---|---|---|---|
| BUY | 17 | 18 | +1 | ✗ NEVER |
| SELL (non-cashback) | 15 | 16 | +1 | ✗ NEVER |
| SELL (cashback) | 16 | 17 | +1 | ✗ NEVER |

**Zero transactions match the builder's account count.** Every single real buy has 18 accounts; every real sell has 16 or 17. The delta is consistently +1.

### The Extra Account

- **Position:** After `bonding_curve_v2` (the builder's declared "last" account)
- **Writability:** WRITABLE
- **Owner:** `pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ` (FEE_PROGRAM_ID)
- **Address:** Varies per transaction — PDA derived by the fee program, not a constant
- **Observed addresses:** `5YxQFdt...`, `5cjcW9w...`, `9M4giFF...`, `5eHhjP8...` (all owned by FEE_PROGRAM_ID)

**The builder's claim that `bonding_curve_v2` "must be last" is incorrect.** On chain, one more writable fee-program PDA follows it.

### Impact

Pump.fun buy/sell instructions submitted with the builder's 17/15/16 account lists will be **rejected by the runtime** — the on-chain program requires the additional fee-program writable account at the tail.

---

## Venue 2: PumpSwap AMM (buy/sell)

### Builder Output

| Function | Accounts | Structure |
|---|---|---|
| `pumpswap_buy_accounts` | 23 | 19-account prefix + 4 buy-only ([19..22]) |
| `pumpswap_sell_accounts` | 21 | 19-account prefix + 2 sell-only ([19..20]) |

Builder comments: "23 / 21 accounts before the remaining-accounts tail."

### Real Mainnet Transactions

**Sample:** 109 PumpSwap buy instructions, 212 sell instructions decoded.

| Instruction | Builder Count | Real Count Distribution | Delta |
|---|---|---|---|
| BUY | 23 | 25 (70 txs), 26 (38 txs), 27 (1 tx) | +2 to +4 |
| SELL | 21 | 23 (175 txs), 24 (34 txs), 26 (3 txs) | +2 to +5 |

**Zero transactions match the builder's account count.**

### The Extra Accounts

PumpSwap transactions use Address Lookup Tables (ALTs), and the extra accounts are ALT-resolved. The two consistent extras beyond the builder's layout appear to be additional fee/protocol accounts. The variation in count (25 vs 26 vs 27 for buy) suggests some accounts are conditionally present (e.g., cashback-specific or referral-specific).

---

## Verdict

**The builder does NOT match chain for either venue.**

| Venue | Builder → Real | Status |
|---|---|---|
| pump.fun buy | 17 → 18 | ✗ FAIL |
| pump.fun sell (non-cashback) | 15 → 16 | ✗ FAIL |
| pump.fun sell (cashback) | 16 → 17 | ✗ FAIL |
| PumpSwap buy | 23 → 25-27 | ✗ FAIL |
| PumpSwap sell | 21 → 23-26 | ✗ FAIL |

**Both gates for live capital arming are affected:**
- Item 5 gate (builder matches chain): **FAIL** — builder must be updated before junction work
- `ex_promotion_gate`: independent of item 5, but live arming requires BOTH gates

---

## Required Fix (pump.fun)

The builder must append one more `AccountMeta::w(...)` after `bonding_curve_v2` in both `pump_buy_accounts` and `pump_sell_accounts`. This account is a PDA derived by the fee program — its derivation seeds need to be determined (likely involving the user and/or mint). Until the derivation is known, the builder cannot produce a submittable transaction.

The comment "must be last" on `bonding_curve_v2` must be corrected — it is second-to-last.

## Required Fix (PumpSwap)

The builder must account for the 2-5 remaining-accounts tail. The builder comments already acknowledge this tail exists ("before any remaining-accounts tail"), but the builder does not produce it. For a submittable transaction, the tail must be populated.

---

## Data Provenance

- RPC endpoint: `https://mainnet.helius-rpc.com/?api-key=<redacted>`
- API key fingerprint: `0937be0e`
- Program IDs queried: `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` (pump.fun), `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` (PumpSwap)
- Buy discriminator: `[102, 6, 61, 18, 1, 218, 235, 234]`
- Sell discriminator: `[51, 230, 133, 164, 1, 127, 131, 173]`
- Decoding: base58 decode of instruction `data` field, first 8 bytes = discriminator
- Account flags decoded from message header (numRequiredSignatures, numReadonlySignedAccounts, numReadonlyUnsignedAccounts)
