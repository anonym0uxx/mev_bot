# pump-quant

> Solana trading bot — dual-engine architecture  
> **MomentumEngine** (live, post-graduation PumpSwap) + **SniperEngine** (in design, bonding curve)  
> Single Rust binary · ShredStream-first · Jito atomic bundles · Kelly sizing · Paper mode

---

## Engines

### MomentumEngine — Live
Enters tokens **after** graduation from the Pump.fun bonding curve onto PumpSwap. Detects migration via ShredStream, scores graduation quality, fires Kelly-sized probes, manages position with trailing stop + take-profit tiers.

### SniperEngine — In Design (Phase 0 logging active)
Enters tokens **before** graduation, directly on the bonding curve. Uses Jito atomic bundles (buy + sell in same bundle — both land or neither does). 12% scalp target. Max loss per attempt = Jito tip only (~5000 lamports).

See [`docs/SNIPER_SIGNAL_SPEC.md`](docs/SNIPER_SIGNAL_SPEC.md) for full spec.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                     pump-quant  (single Rust binary)                    │
│                                                                         │
│  FEEDS (priority order)         ENGINE REGISTRY       ENGINES          │
│  ─────────────────────          ───────────────       ───────          │
│                                                                         │
│  ShredStream gRPC (:20100) ──►  FeedRouter        ──► MomentumEngine  │
│  (~0ms from block production)   (fan-out to all       (live trading)   │
│                                  registered engines)                    │
│  Helius WS ──────────────────►                    ──► SniperEngine     │
│  (enhanced, decoded)                                  (stub, paper)    │
│                                                                         │
│  PumpPortal WS ──────────────►  ExecutionContext                       │
│  (TokenCreated, social)         • Jito HTTP/2 (NY primary)             │
│                                 • Nozomi (EWR1)                         │
│  [CoreCast — being removed]     • BlockhashCache                       │
│                                 • Wallet + WSOL ATA                    │
│                                                                         │
│  SHARED INFRA                                                           │
│  ────────────                                                           │
│  HealthMonitor (per-feed staleness → auto-pause)                       │
│  ShredStream gRPC proxy (:20100)                                       │
│  REST API (:9421) — status, control, health                            │
│  JSONL trade log · Telegram alerts                                     │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Feed Priority

| Feed | Latency | Role |
|------|---------|------|
| **ShredStream** | ~0ms from shred | First. All execution-critical events. |
| **Helius** | 50–200ms | Second. Account state, enrichment, reserves. |
| **PumpPortal** | 100–500ms | Third. TokenCreated events, social metadata. |
| Public RPC | variable | Last resort only. Never on hot path. |

---

## SniperEngine — Entry Gate System

Entry fires only when **all Tier 0 gates pass** AND **on-chain score ≥ threshold** AND **final score (with social multiplier) ≥ 40**.

### Tier 0: Hard Gates

Binary pass/fail. Any failure = immediate skip. Evaluated within 10ms of `TokenCreated`.

| Gate | Logic | Source | Why |
|------|-------|--------|-----|
| **G0** Not Mayhem Mode | `is_mayhem_mode == true` → FAIL | Helius BC account / PumpPortal | Pump.fun AI agent corrupts all Tier 1 signals (velocity, wallet diversity, sell timing) |
| **G1** No Dev Pre-Buy | Any of first 5 trades: `trader == creator` and `is_buy` → FAIL. Also: same-slot buy from SOL-linked wallet → FAIL | ShredStream + creator_map | Dev front-loading own token |
| **G2** No Coordinated Bundle | `≥2 distinct wallets` in create slot AND `>2 SOL total` in that slot → FAIL | ShredStream slot tracking | Coordinated snipers use exactly 2 wallets to evade ≥3 detection |
| **G3** Dev Not Serial Rugger | `dev_tokens_launched ≥ 10` → FAIL, or `≥5 tokens + >40% rug rate` → FAIL | Helius `getAssetsByCreator` (cached) | ArXiv: prolific creators graduate less. Serial launchers are almost always rugs |
| **G4** Curve Fill < 25 SOL | `vsol ≥ 25.0` → FAIL | Helius BC accountSubscribe | At vSol=25, +12% requires ~4.2 SOL follow-on — unrealistic for most tokens |
| **G6** Creator Not Throwaway | Creator balance `<0.05 SOL` AND zero launch history → FAIL | Helius `getBalance` (cached) | Script-generated rug factory wallets funded with exactly the creation fee |
| **G7** No Supply Concentration | Any single wallet holds `>15%` of tokens bought in first 20 trades → FAIL | ShredStream holdings tracker | Single large holder = coordinated dump setup |

### Tier 1: On-Chain Score (0–100 pts)

Computed in real-time from ShredStream trades + Helius BC account state. Re-evaluated on every trade event.  
**Entry threshold: ≥50 pts** (with velocity data) or **≥40 pts** (trade_count < 10).

| Signal | Max Pts | Metric | Source | Notes |
|--------|---------|--------|--------|-------|
| **S1** Inflow Rate | 30 | `(vsol−30) / seconds_since_create` (SOL/s) | ShredStream + Helius BC | 0.3 SOL/s = sweet spot (30pts). >1.0 SOL/s = spike risk (20pts). ArXiv #1 predictor. |
| **S2** Wallet Diversity | 25 | `unique_wallets / trade_count` | ShredStream | ≥0.80 ratio → 25pts. Replaces fragile bot detection. Bots reuse wallets; organic = diverse. |
| **S3** Curve Fill | 15 | `vsol / 115.0` (U-curve) | Helius BC accountSubscribe | Sweet spot: 2–8% fill (vsol 2.3–9.2 SOL) → 15pts. Too early = no data. Too late = hard math. |
| **S4** Sell Timing | 15 | Index of first sell trade | ShredStream | First sell after trade 15 → 15pts. First sell before trade 5 → 0pts. Replaces buy/sell ratio (noise at mint). |
| **S5** Smart Money | −10 to +15 | Pre-seeded top-500 PnL wallet set | ShredStream + wallet list | Top-100 wallet buys early → +15pts. Known dumper buys early → −10pts. |

**Sub-10-trade velocity substitute (S1 when trade_count < 10):**
- First buy 0.1–0.5 SOL → 8pts (human-sized)
- First buy 0.5–2.0 SOL → 5pts (large, possible whale)
- First buy >2.0 SOL → 3pts (spike risk)
- First buy <0.01 SOL → 0pts (bot dust)

### Tier 2: Social Multiplier (0.5×–2.0×)

Applied async: `final_score = on_chain_score × social_multiplier_bps / 10_000`  
**Signals SS2 and SS5 disabled for tokens age < 120s** — data doesn't exist at mint.

| Signal | Weight | Source | Notes |
|--------|--------|--------|-------|
| **SS1** Dev wallet history | 55% | Helius `getAssetsByCreator` | Only reliable signal at mint. 1-4 launches, low rug rate → +2000bps. Blacklisted → hard veto (multiplier=0). |
| **SS4** Metadata quality | 25% | PumpPortal `TokenCreated` | Instant, no API. Link count + description length + image. Zero effort → −1500bps. Full set → +1200bps. |
| **SS3** Twitter presence | 10% | PumpPortal event | Presence only. Present → +800bps. Absent → −800bps. |
| **SS2** Pump.fun engagement | 5% | Pump.fun API | **Disabled <120s.** Reply count, KOTH, live stream. |
| **SS5** Telegram community | 5% | Telegram Bot API | **Disabled <120s.** Pre-built large channels at mint = negative signal. |

### Decision Flow

```
TokenCreated
    │
    ├─ G0: Mayhem? ──────────────────────────────────────────► DROP
    ├─ G1: Dev prebuy? ──────────────────────────────────────► DROP
    ├─ G2: Coordinated bundle? ──────────────────────────────► DROP
    ├─ G3: Serial rugger? ───────────────────────────────────► DROP
    ├─ G4: vSol ≥ 25? ───────────────────────────────────────► DROP
    ├─ G6: Throwaway wallet? ────────────────────────────────► DROP
    └─ G7: Supply concentration? ────────────────────────────► DROP
                    │ ALL PASS
                    ▼
    [Fire async social enrichment — non-blocking]
    [ShredStream trades update BondingCurveState continuously]
                    │
    S1 + S2 + S3 + S4 + S5 = on_chain_score (0–100)
                    │
    score < threshold (50/40)? ──────────────────────────────► MONITOR
                    │ threshold met
                    ▼
    social_multiplier × on_chain_score = final_score
    final_score < 40? ───────────────────────────────────────► MONITOR
                    │ ≥ 40
                    ▼
    Kelly size → p = score_to_p(final_score), half-Kelly, 0.01–0.10 SOL
                    │
    Jito bundle → TX1: buy at P1 · TX2: sell at P1×1.12
    Both land or neither does.
```

---

## MomentumEngine — How It Works

1. **Graduation detected** — ShredStream `parse_pump_migration()` spots bonding curve → PumpSwap migration
2. **Pool resolved** — Engine fetches on-chain pool accounts, validates liquidity ≥ 30 SOL
3. **Scored** — Graduation quality scored 0–100 (curve dynamics, flow momentum, manipulation detection)
4. **Probe** — Kelly-sized buy (~0.03 SOL) if score ≥ threshold
5. **Position management** — Trailing stop floor, 3 take-profit tiers, hard SL, time SL
6. **Exit** — Sell on Jito + Nozomi dual submit

---

## Bonding Curve Math

```
k = 30 × 1.073e9 = 3.219e10  (constant product invariant)
Price P = vsol² / k
Graduation at vsol = 115 (virtual) = 85 SOL real raised

Buy: tokens_out = vtok - k / (vsol + delta_sol × 0.9875)
Sell: sol_out = (vsol_exit - vsol) × 0.9875
     where vsol_exit = sqrt(P_target × k)

For +12% target: vsol_exit = sqrt(P1 × 1.12 × k)
SOL needed for +12% from entry:
  vSol=5  → ~0.67 SOL follow-on
  vSol=10 → ~1.4 SOL
  vSol=20 → ~3.1 SOL
  vSol=25 → ~4.2 SOL  ← G4 ceiling
```

---

## Infra

| Component | Value |
|-----------|-------|
| VPS | Hostinger, Boston MA (US East) |
| Jito block engine | `ny.mainnet.block-engine.jito.wtf` (primary), Frankfurt (secondary) |
| Nozomi endpoint | `ewr1.nozomi.temporal.xyz` (Newark NJ) |
| ShredStream proxy | `localhost:20100` |
| Rust daemon port | `9421` |
| Paper mode | Controlled via `canary.json` → `paper_mode: true` |

---

## Key Files

| File | Description |
|------|-------------|
| `rust/pump-quant-core/src/main.rs` | Entry point, engine registry, feed wiring |
| `rust/pump-quant-core/src/momentum/` | MomentumEngine implementation |
| `rust/pump-quant-core/src/sniper/` | SniperEngine stub |
| `rust/pump-quant-core/src/engine/` | TradingEngine trait, FeedRouter, EngineRegistry |
| `rust/pump-quant-core/src/feeds/` | ShredStream, Helius, PumpPortal feed handlers |
| `rust/pump-quant-core/src/execution/` | ExecutionContext, Jito, Nozomi |
| `scripts/watchdog.sh` | Service health + auto-restart |
| `scripts/rust-status.js` | Status report for heartbeat |
| `canary.json` | Runtime config (paper mode, thresholds, feature flags) |
| `data/momentum_paper_trades.jsonl` | Trade log |
| `docs/SNIPER_SIGNAL_SPEC.md` | Complete sniper entry signal spec (v1.2) |
| `docs/BONDING_CURVE_SNIPER_IDEATION.md` | Sniper strategy context |
| `docs/social-signal-layer-spec.md` | Social enrichment pipeline spec |

---

## Research

Signal system backed by:
- **ArXiv 2602.14860** — "Predicting the success of new crypto-tokens: the Pump.fun case" (Marino/Tarantelli/Lillo, Feb 2026). Dataset: 655k tokens, Sep 2025. Key findings: liquidity inflow rate is #1 graduation predictor; bot-dominated tokens graduate less; prolific creators graduate less.
- Internal momentum engine trade log (84 trades, live PnL data)

---

## Status

| Engine | Mode | Status |
|--------|------|--------|
| MomentumEngine | Paper | Live, monitoring |
| SniperEngine | Paper | Stub only — Phase 0 logging next |

*Last updated: 2026-04-04*
