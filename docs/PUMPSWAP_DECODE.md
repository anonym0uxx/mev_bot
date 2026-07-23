# PUMPSWAP (Pump AMM) DECODE — raw-bytes reference (built + tested)

Program `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` (upgraded in place — there is no
"v2 program"; accounts/args/events only ever get fields APPENDED). Decode is therefore
**length-tolerant everywhere**: known prefix decoded, optional tails as `Option`, unknown
trailing bytes ignored, wrong discriminator/truncation → `None`, never a panic.
Source of truth: the official IDL at pump-fun/pump-public-docs (`idl/pump_amm.json`) —
re-pin on any pump_tech_updates announcement; the Apr/May-2026 changes were breaking at
the transaction-build level (reserved fee recipients; USDC quote mints).

Where it lives (all dep-free, in-workspace, 42 new tests):
- `pump-quant-protocol::pumpswap` — Pool account (disc `f19a6d0411b16dbc`; coin_creator/
  mayhem/cashback tail), GlobalConfig (disc `95089ccaa0fcb0d9`, pinned mainnet PDA
  `ADyA8hde…`, seeds `["global_config"]`), SPL vault amount (u64@64 — **reserves are the
  two pool token-account balances, NOT Pool fields**), bonding-curve creator tail.
- `pump-quant-protocol::pumpswap_ix` — buy `66063d1201daebea`, sell `33e685a4017f83ad`,
  create_pool `e992d18ecf6840bc`, deposit/withdraw, prefix-tolerant `SwapAccounts` map
  (0 pool, 1 user, …, 18 coin_creator_vault_ata, 19 vault authority; NEVER assume a fixed
  account count — cashback/reserved-fee accounts ride `remaining_accounts`), and pump-program
  `migrate` detection (`9beae792ec9ea21e`, mint@2, pool@9).
- `pump-quant-protocol::pumpswap_event` — Anchor self-CPI events (inner ix data =
  tag `e445a52e51cb9a1d` ‖ event disc ‖ borsh): BuyEvent `67f4521f2cf57777`, SellEvent
  `3e2f370aa503dc2a`, CreatePoolEvent `b1310cd2a076a774`. **Per-trade events are the
  canonical fee/reserve truth**: `*_fee_basis_points` fields beat any hardcoded schedule
  (dynamic market-cap-tiered fees via `pfeeUxB6…` since 2025-09-01). Reserve fields are
  PRE-trade snapshots. Normalized `PumpSwapTrade` extractor + fixed-point price helpers +
  `verify_buy_event` CP cross-check (uses `curve::pumpswap_amount_out`).

Operational laws: key everything off `pool.quote_mint` (WSOL canonical, USDC pools exist
since 2026-05; the AMM has no native-SOL path — callers wrap/unwrap); canonical pool for a
graduated mint arrives via the pump `migrate` → `create_pool` CPI (index 0, creator =
pump `["pool-authority", mint]` PDA, coin_creator inherited from the bonding curve, LP
burned); the legacy `decode::PumpSwapPool` reduced view is dossier-pinned and unchanged —
`pumpswap::PoolAccount` is the full correct decoder.

Wiring status: decode plane complete and gate-verified; stream feed (LaserStream
accounts-by-owner=pAMMBay… + transaction subscriptions) connects at Phase-B per
docs/HELIUS_INTEGRATION.md — a `PumpSwapTrade` → engine trade-event join is the
remaining one-liner, deliberately left for live-tape verification (§38 honesty).
