# North Star — Stage 2: D13 Protocol & Numeracy Reference

Status: **source-verified corpus.** Every formula and constant is lifted from the Rust
source, not reconstructed. Purpose: the protocol/numeracy knowledge the Qwen trading
brain must hold (bonding-curve math, fees, graduation, units) so it reasons about
entries/exits in exact lamports, never in floats.

Sources: `rust/crates/pump-quant-protocol/src/{curve,decode,errors,venue_accounts,
pumpswap_ix,registry}.rs`, `rust/crates/pump-quant-app/src/curve_state.rs`.

## 1. Units (numeracy)

- **1 SOL = 1,000,000,000 lamports (1e9).** All reserves, amounts, and PnL are integer
  `u64` lamports. Never `f64`.
- **1 token = 1,000,000,000,000,000 base units (1e15)** — pump.fun tokens have 15 decimals.
  The supply is `1e15` == 1 billion display tokens.
- **Prices / market cap:** `market_cap_lamports = k / MCAP_DIVISOR`, and
  `MCAP_DIVISOR_LAMPORTS = 32_190_000_000` (== `30e9 · 1.073`).

## 2. Bonding curve (constant-product AMM)

Invariant: **`k = virtual_sol × virtual_token`** (integer, u128-widened).

Seeded constants (curve_state.rs §80–98):
| Constant | Value (lamports / raw units) | Meaning |
|---|---|---|
| `LAUNCH_VSOL` | 30_000_000_000 | virtual SOL at launch (30 SOL) |
| `GRADUATION_VSOL` | 115_005_359_056 | virtual SOL when last real token sells (~115.01 SOL) |
| `MAX_CURVE_REAL_SOL` | 85_005_359_056 | graduation raise (85.005 SOL) — `graduation − launch` |
| `INITIAL_VIRTUAL_TOKENS` | 1_073_000_000_000_000 | virtual token reserve at launch (1.073e15) |
| `INITIAL_REAL_TOKENS` | 793_100_000_000_000 | real tokens at launch |

`real_sol = virtual_sol − 30 SOL` — the SOL actually escrowed / payable out.

### Buy math (1% fee) — `pump_amount_out`
```
fee        = sol_in * 100 / 10_000            // 1% buy fee
sol_in_net = sol_in - fee
k          = virtual_sol * virtual_token
new_vsol   = virtual_sol + sol_in_net
new_vtoken = k / new_vsol
tokens_out = virtual_token - new_vtoken
```

### Generic constant-product swap (post-migration PumpSwap) — `pumpswap_amount_out`
```
amount_in_net = amount_in * (10_000 - fee_bps) / 10_000
amount_out    = reserve_out * amount_in_net / (reserve_in + amount_in_net)
```

## 3. Graduation → migration

- Curve completes when `virtual_sol >= GRADUATION_VSOL` (115.005 SOL) — the migration
  threshold, equivalent to **85.005 SOL raised**, the figure the ecosystem quotes.
- On completion, pump.fun's `migrate` instruction creates the **PumpSwap** AMM pool
  (constant-product, `6EF8rrect…`). Migrate account indices: mint = 2, pool = 9.
- `PUMP_MIGRATE_DISCRIMINATOR = [155,234,231,146,236,158,162,30]` (sha256("global:migrate")[..8]).

## 4. Program identity

| Venue | Program |
|---|---|
| pump.fun bonding curve | `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` |
| PumpSwap AMM | (const in venue_accounts.rs) |
| Token (SPL) | `Tokenkeg...` (const) |
| **Token-2022** | `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb` — **pump.fun uses Token-2022**, not SPL |
| Fee / System / ATA / ComputeBudget | (consts in venue_accounts.rs) |

Holder queries on pump.fun tokens MUST use the Token-2022 program, not SPL Token.

## 5. Error codes (pump.fun)

- `6004` MintDoesNotMatchBondingCurve — mint/curve pairing wrong.
- `6005` BondingCurveComplete — curve already migrated.
- `6006` BondingCurveNotComplete — action requires a completed curve.
- `FailureClass6::RouteError` covers MintDoesNotMatchBondingCurve + InvalidPoolTokenAccounts.

## 6. Operational conventions

- **Integer-only (§22):** all math u128-widened and checked; overflow / divide-by-zero
  return `None`, never panic or wrap.
- **No float price impact:** `f64` `priceImpactPct` intentionally not ported — it never
  controlled an outcome.
- **Virtual vs real reserves:** virtual sets the price curve; real is the SOL actually
  escrowed. `real_sol_for(vsol) = vsol − 30 SOL`.

## Coverage note

D13 is P1 knowledge. This corpus is the source-verified core; the operational/venue-
incentive and latency aspects (Jito tips, priority fees, bundle inclusion) are covered
in `cost_model.rs` / `latency.rs` and can be appended as the training corpus is assembled
(Stage 5).