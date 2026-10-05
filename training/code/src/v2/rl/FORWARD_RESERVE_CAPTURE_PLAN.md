# FORWARD RESERVE CAPTURE PLAN (replaces historical backfill)

Status: design + config + code. No GPU used. Config: `forward_reserve_capture.json`
(concrete filters/discriminators/schemas). This file says exactly what has to
change in the existing capture setup, and why each change is the minimum needed.

## 1. What we have today

* `capture-rs-patched` subscribes, in `capture.rs` (`--training-capture`):
  * **transactions** `account_include = [pump.fun, pump-amm]`, `vote=false`,
    no `failed` filter, commitment CONFIRMED;
  * **accounts** `owner = [pump.fun, pump-amm]`;
  * slots + blocks_meta.
* Consequence, measured on two real 2026-08-24 parts
  (`/mnt/data/mev_bot-artifacts/raw/...part0000/0001`):
  * **13,721 pump-amm BuyEvents and 14,090 SellEvents are already in the stream**
    (discriminators `67f4521f2cf57777` / `3e2f370aa503dc2a`, derived as
    `sha256('event:BuyEvent')[:8]` — a derivation verified by reproducing
    pump.fun's known `TradeEvent = bddb7fd34ee661ee` and
    `BondingCurve account = 17b7f83760d8ac60`);
  * **14,436 pump-amm `Pool` account records** are already in the stream
    (disc `f19a6d0411b16dbc`).
* So the *data is arriving*. What is missing is **decoding and persistence**:
  `normalizer.rs` declares `virtual_sol`, `virtual_token`, `real_sol`,
  `real_token`, `curve_complete`, `mayhem`, `cashback`,
  `pool_account_b58`, `base_reserve`, `quote_reserve`, `lp_supply`
  and initialises them to `None` — and never assigns them, because it decodes
  instruction data only. No reserves artifact is written.

## 2. The two reserves sources, per regime

| regime | reserves live in | reachable by | authoritative read |
|---|---|---|---|
| BONDING_CURVE | pump.fun `BondingCurve` account (program-owned) | existing `accounts owner=[pump.fun]` filter | account struct: disc `17b7f83760d8ac60`, then `<QQQQ` at offset 8 = `virtual_token, virtual_sol, real_token, real_sol` (verified against event data in `xval_accounts.py`) |
| BONDING_CURVE (cross-check) | anchor `TradeEvent` in tx logs | existing transaction filter | disc `bddb7fd34ee661ee`; `<QQ` @40 = (sol_amt, tok_amt), `<q` @89 = ts, `<QQQQ` @97 = (vsol, vtok, rsol, rtok) — this is what `build_reserves.py` already decodes offline |
| AMM | the *pool's two SPL token vaults* (`pool_base_token_account`, `pool_quote_token_account`) | **needs a NEW address-based account filter** — the vaults are owned by the Token program, so no owner filter can reach them | SPL token account: `mint` @0 (32B), `amount` @64 (u64) |
| AMM (cross-check) | pump-amm `BuyEvent` / `SellEvent` in tx logs | existing transaction filter | disc `67f4521f2cf57777` / `3e2f370aa503dc2a`; the events carry `pool_base_token_reserves` / `pool_quote_token_reserves` alongside the trader legs |

Cross-check that was actually run on a real event (sell of
`4SsV4h435NHJaQY2Km5WSDqUmbHZUTL16d5YwQTVB2rZt5VZ5L5HYBWooPydJhf3VLEoebcBrFP8yrzAz1FKrAyD`):
the event's u64s at offsets 104/112 reproduce the recorded trader token leg
(509,259,586,392) and pool token delta (509,004,318,428) exactly, offset 96 is
the 5 bp coin-creator fee in tokens (255,267,964), and the candidate reserve
pair (offsets 48/56 = 484,680,168,782 / 2,366,885,622,134,135) implies a
marginal price of 2.05e-4 lamports/raw token against the recorded average
2.21e-4 — consistent for a seller. The vault-account path is still the primary
read; the event pair is the independent cross-check, and an ingest rule
(`|event − vault| / vault ≤ 1e-6`) decides which rows are trusted.

## 3. Exact changes to the capture setup

1. `capture.rs` — **add a third account filter**, `pumpswap_pool_vaults`:
   * `account_include` = the vault addresses harvested from the `Pool` account
     stream; refresh the SubscribeRequest every 30 s on the live stream.
   * keep the existing `pump_accounts` owner filter (it is how Pool accounts,
     and therefore the vault addresses, are discovered).
   * `Slots`/`blocks_meta` unchanged.
2. `normalizer.rs` — three decoders, behind the existing event/account paths:
   * `decode_pumpfun_trade_event(raw)` → reserves from `Program data:` payloads
     (`bddb7fd34ee661ee`, offsets as above);
   * `decode_pumpfun_curve_account(data)` → reserves from the account body
     (`17b7f83760d8ac60`, offset 8);
   * `decode_pumpamm_swap_event(raw)` + `decode_pumpamm_pool_account(data)` →
     event reserves + pool→(base_mint, quote_mint, base_vault, quote_vault);
   * `decode_spl_token_amount(data)` → `amount` @64, validated against the
     pool's mint.
   Populate the already-declared `Option<u64>` fields instead of leaving `None`.
3. New writer `reserves_writer.rs` (mirroring `events_writer.rs`):
   * flush **both** parquet artifacts on a 5-second timer (max 60 s staleness
     budget in the engine; the current end-of-session flush is useless for a
     forward trader);
   * append `source = event|vault_account|account` and a `slot`;
   * write a companion `*.manifest.json` with row counts + the disagreement count.
4. `manifest.rs` — record `reserves_curve_rows`, `reserves_pool_rows`,
   `vaults_subscribed`, `reserve_disagreements`.

## 4. Consumer side (already implemented, no GPU needed)

* BONDING_CURVE: `regime_pricing.build_curve_oracle(paths)` /
  `slinky_curve_oracle(dir)` → `MechanicsEngine(regime=BONDING_CURVE)`;
  replay harness `--reconcile-mechanics`.
* AMM: `regime_pricing.pool_reserve_oracle(glob)` → `MechanicsEngine(regime=AMM)`.
  The loader is exercised today by a synthetic fixture in
  `test_regime_pricing.py::test_forward_amm_first_fill_path`, so the first real
  pumpswap fill after capture is priced by code that has already run.
* Both oracle paths refuse loudly (no tape fallback) and expose
  `reserve_joins` + `staleness_ms` per episode.

## 5. What cannot be done without the capture

Historical pumpswap pool *levels* do not exist anywhere in the corpus (only
per-swap deltas in a 50 MB derived subset). No amount of offline work can
recover them: the AMM replay is reported as `not_attempted` for that reason.
The AMM path is nonetheless fully implemented, unit-tested, and wired to the
capture schema above.
