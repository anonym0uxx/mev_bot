# Live Transaction Submission Spec v1

**Date:** 2026-03-31
**Author:** Principal Quant (Apollo)
**Target:** Rust engineer implementing live tx submission for pump-quant momentum engine
**Status:** Ready for implementation

---

## 1. Executive Summary

- **Dual-submit architecture:** Every sell transaction fires to Jito AND Nozomi in parallel via `tokio::join!()`. First confirmation wins. This roughly doubles landing rate for critical exits without doubling cost (only the landed tx pays a tip).
- **Tip escalation for sells:** Time-gated escalation ladder with 3 levels. Sell tips are always higher than buy tips — profit capture is worth more than entry.
- **Bundle confirmation tracking:** Poll transaction signature on-chain with 500ms budget. Unconfirmed after 500ms triggers re-sign + resubmit with escalated tip.
- **sendTransaction fallback:** If both Jito and Nozomi are unreachable (catastrophic), fall back to Helius RPC `sendTransaction` with `skipPreflight: true`.
- **Staked Helius deferred:** Not needed pre-launch. Nozomi parallel submit provides the landing rate improvement that staked connections would deliver, without requiring a SOL stake.

---

## 2. Tip Escalation

### 2.1 Core Insight

Tips must be proportional to what's at stake. A trailing stop sell on a 0.50 SOL position with +200bps (0.01 SOL gross) has a very different tip budget than one at +5000bps (0.25 SOL gross). The existing `TipEngine` already computes conviction-aware base tips. This spec adds **time-gated escalation on retry**.

### 2.2 Escalation Ladder (Sell Only)

Sells are time-critical (trailing stop fired, price is falling). Buys are not escalated — if a buy doesn't land, we just skip the entry.

| Level | Trigger | Tip Multiplier | Max Absolute Tip | Notes |
|-------|---------|---------------|-----------------|-------|
| L0 (base) | Initial submit | 1.0x | Per TipEngine | Normal conviction-based tip |
| L1 | 500ms after L0, no confirmation | 2.0x | 5,000,000 lamports (0.005 SOL) | Re-sign with fresh blockhash + escalated tip |
| L2 | 500ms after L1, no confirmation | 4.0x | 10,000,000 lamports (0.010 SOL) | Last escalation. Dual-submit to both networks again |
| ABANDON | 500ms after L2, no confirmation | -- | -- | Accept the loss. Log WARN. Position marked `exit_failed` |

**Total worst-case latency budget:** 1,500ms from stop trigger to ABANDON. Given our price feed polls at 500ms, this means ~3 price ticks of additional exposure.

### 2.3 Tip Cap as Percentage of Expected PnL

**Hard rule:** Tip at any level MUST NOT exceed 25% of gross expected PnL.

```
max_tip_for_trade = max(min_tip, min(escalated_tip, gross_pnl_lamports / 4))
```

For a 0.50 SOL position at +200bps:
- Gross PnL: ~10,000,000 lamports (0.01 SOL)
- 25% cap: 2,500,000 lamports
- L0 tip: ~1,000,000 (RideEarly context) -- OK
- L1 tip: 2,000,000 -- OK
- L2 tip: 4,000,000 -- capped to 2,500,000

For a 0.50 SOL position at +5000bps:
- Gross PnL: ~250,000,000 lamports (0.25 SOL)
- 25% cap: 62,500,000 lamports
- All escalation levels fit comfortably

For stop-loss exits (negative PnL):
- Use emergency base tip (5,000,000 lamports) at L0
- Escalate as normal -- getting out fast limits further losses
- No PnL cap applied on SL exits (gross_pnl < 0 => skip cap)

### 2.4 Buy vs Sell Escalation

| | Buy | Sell |
|---|---|---|
| Escalation | **None** | 3-level ladder |
| Rationale | Missed entry = no loss. Next opportunity comes in seconds. | Missed exit = money left on table or stop-loss deepens |
| Base tip | TipEngine.compute_tip() with TipContext::Scalp | TipEngine.compute_tip() with appropriate context |
| On failure | Log and skip entry | Escalate through L0 -> L1 -> L2 -> ABANDON |

### 2.5 Nozomi Tip Floor

Nozomi has a **minimum tip of 1,000,000 lamports (0.001 SOL)**. All Nozomi submissions must enforce this floor. Jito's floor is lower (~200,000 lamports from our TipConfig.min_tip). When dual-submitting:

- Jito tip: from TipEngine (may be as low as 200,000)
- Nozomi tip: `max(nozomi_min_tip, tip_engine_tip)`

This means at L0, the Nozomi tx may have a higher tip than the Jito tx. That's fine -- Nozomi's tip pays for their staked connections and Jito forwarding internally.

---

## 3. Nozomi Parallel Submit

### 3.1 Architecture

```
         +------------------+
         |  TxExecutor       |
         |  execute_sell()   |
         +---------+---------+
                   | build tx (TxSkeleton::patch)
                   v
         +------------------+
         |  DualSubmitter    |
         |  submit_sell()    |
         +----+--------+----+
              |        |
     tokio::join!()  tokio::join!()
              |        |
              v        v
       +----------+ +-------------+
       | Jito     | | Nozomi      |
       | (bundle) | | (sendTx)    |
       +----------+ +-------------+
              |        |
              v        v
       first Ok() wins --> return sig
       both Err() ------> sendTransaction fallback
```

**Critical design point:** Jito and Nozomi require **different transactions**. The tip instruction transfers to different accounts. So we must build TWO signed transactions per submit -- one with a Jito tip account, one with a Nozomi tip account. The `TxSkeleton` already supports patching the tip amount; we need to also patch the tip recipient (or build two skeletons at position open).

### 3.2 Two-Skeleton Approach

At position open, build TWO `TxSkeleton` instances:
1. `jito_skeleton` -- tip instruction targets a Jito tip account
2. `nozomi_skeleton` -- tip instruction targets a Nozomi tip account

Both are built on the cold path (position open, ~10us each). At exit time, patch both in parallel (<1us each), sign both, submit both.

**Tip account selection:**
- Jito: round-robin through 8 accounts (existing)
- Nozomi: random selection from 17 accounts per tx (new)

### 3.3 Submission Protocol

Nozomi uses standard Solana `sendTransaction` RPC -- NOT a bundle API. This is simpler than Jito:

```
POST https://{region}.nozomi.temporal.xyz/?c={API_KEY}
Content-Type: application/json

{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "sendTransaction",
  "params": [
    "<base64_tx>",
    { "encoding": "base64", "skipPreflight": true, "maxRetries": 0 }
  ]
}
```

**Key Nozomi behavior:** Nozomi automatically retries your tx until confirmed or blockhash expires. We set `maxRetries: 0` because WE manage retries (tip escalation). Nozomi's internal retry is still active regardless of this param -- it retries at the network level.

### 3.4 Endpoint Selection

Nozomi has 8 regional endpoints. Our VPS is in Frankfurt. Closest likely endpoints:

| Endpoint | Region | Expected Latency from Frankfurt |
|----------|--------|---------------------------------|
| `ams1.nozomi.temporal.xyz` | Amsterdam | ~5-10ms |
| `fra1.nozomi.temporal.xyz` | Frankfurt | ~1-3ms (if exists) |
| `nozomi.temporal.xyz` | Auto-routed | ~10-20ms |

**Action item for engineer:** Run the Nozomi SDK's `findFastestEndpoints()` equivalent from our VPS to determine the actual fastest endpoint. Hardcode the top 2 as primary/secondary (same pattern as Jito). The SDK code at `@temporalxyz/nozomi-sdk` shows endpoints are fetched from a GitHub JSON file -- inspect that file to get the full endpoint list, then benchmark from Frankfurt.

### 3.5 Tip Splitting -- Cost Analysis

**Question:** Do we pay two tips?

**Answer:** Only the LANDED transaction costs a tip. If Jito lands first, the Nozomi tx either fails (duplicate nonce/blockhash behavior) or never lands. If Nozomi lands first, the Jito bundle is ignored. The tip is an instruction INSIDE the transaction -- it only executes if the tx lands.

**BUT:** The two transactions have different signatures (different tip recipients -> different message -> different signature). Both COULD theoretically land in the same slot. In practice:
- Pump.fun sell drains your token balance entirely. The second tx would fail with insufficient token balance when the program tries to transfer tokens.
- Even in the rare case where both somehow execute atomically (different validator slots), you'd pay two tips but only sell once (second tx fails the program instruction).

**Worst case cost:** 2x the tip (~2,000,000 lamports extra at L0). Probability: low (~5-10% of trades where both networks deliver to the same slot). This is acceptable -- it's the cost of reliability.

### 3.6 Cost-Benefit at Our Position Sizes

| Scenario | Position | Expected PnL | Tip Cost (dual) | Net Impact |
|----------|----------|-------------|-----------------|------------|
| Probe exit +200bps | 0.10 SOL | 0.002 SOL | 0.002 SOL (2x 0.001) | Marginal -- tip ~= profit |
| Probe exit +500bps | 0.10 SOL | 0.005 SOL | 0.002 SOL | Acceptable -- 40% of profit |
| Scaled exit +200bps | 0.50 SOL | 0.010 SOL | 0.002 SOL | Good -- 20% of profit |
| Scaled exit +2000bps | 0.50 SOL | 0.100 SOL | 0.002 SOL | Excellent -- 2% of profit |
| Stop loss -800bps | 0.50 SOL | -0.040 SOL | 0.002 SOL | Worth it -- limits loss |

**Decision:** For **probe-size positions** (0.10 SOL), Nozomi's 0.001 SOL minimum tip is expensive relative to expected PnL. **Only dual-submit sells where `position_size_lamports >= 200_000_000` (0.20 SOL)**. Below that threshold, Jito-only.

Config field: `nozomi_dual_submit_min_size_lamports: u64` (default: 200,000,000)

### 3.7 API Key Management

Nozomi requires an API key. Store in environment variable `NOZOMI_API_KEY`. The key is appended as a query parameter: `?c={API_KEY}`.

**Signup:** Go to https://www.temporal.xyz/nozomi and join via Discord invite. API key is provisioned from the Nozomi dashboard.

---

## 4. Bundle Confirmation Tracking

### 4.1 The Problem

Currently fire-and-forget: `submit_bundle()` returns after Jito HTTP 200, but HTTP 200 means "bundle received", not "bundle landed". We don't know if our sell actually executed.

### 4.2 Confirmation Strategy

**Primary method:** Check the transaction signature on-chain via `getSignatureStatuses`.

After submitting (Jito + Nozomi in parallel), immediately start polling:

```
POST {SOLANA_RPC_URL}
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "getSignatureStatuses",
  "params": [
    ["<jito_sig>", "<nozomi_sig>"],
    { "searchTransactionHistory": false }
  ]
}
```

**Polling schedule:**
- T+200ms: first check (both sigs in one RPC call)
- T+500ms: second check -- if neither confirmed, trigger L1 escalation
- T+1000ms: third check -- if neither confirmed, trigger L2 escalation
- T+1500ms: final check -- if neither confirmed, ABANDON

This uses `getSignatureStatuses` which is a read RPC call to our Helius endpoint. At 4 polls per exit, that's ~20 polls/min at peak (5 concurrent exits). Well within Helius rate limits.

### 4.3 Double-Submission Safety

**Q: If we resubmit while the first tx is in-flight, can both land?**

**A: No, for full sells.** The sell instruction transfers ALL tokens from our ATA. If tx1 lands first and sells all tokens, tx2 will fail with `InsufficientFunds` on the token transfer. This is safe by construction.

For partial sells (future): both COULD land. But we don't do partial sells via Pump.fun -- it's always full position exit. This is safe today.

**Re-sign requirement:** Each escalation level requires a NEW transaction (different tip amount -> different message -> different signature -> different blockhash). Get fresh blockhash from cache, patch skeleton, re-sign. The `TxSkeleton::patch()` already handles this in <1us.

### 4.4 Feeding Results Back to TipEngine

The confirmation check tells us whether a bundle landed. Feed this into `TipEngine::record_result()`:

```rust
// After confirmation loop completes
if confirmed_at_level.is_some() {
    tip_engine.record_result(true);
} else {
    tip_engine.record_result(false);
}
```

This keeps the landing rate circular buffer accurate, which feeds into the congestion multiplier.

---

## 5. sendTransaction Fallback

### 5.1 When It Fires

Only when BOTH Jito AND Nozomi are unreachable (network-level failure, not just bundle rejection). This is the absolute last resort.

```rust
match dual_submit(&jito_tx, &nozomi_tx).await {
    Ok(sigs) => { /* normal path — proceed to confirmation polling */ },
    Err(DualSubmitError::BothNetworksFailed { jito_err, nozomi_err }) => {
        // Fall back to sendTransaction
        warn!(
            "both MEV networks failed: jito={}, nozomi={}. Falling back to RPC sendTransaction",
            jito_err, nozomi_err
        );
        self.rpc_send_transaction(&fallback_tx).await?;
    },
    Err(DualSubmitError::BundleRejected { .. }) => {
        // Bundle was received but rejected (bad blockhash, etc)
        // Escalate tip, don't fall back
    }
}
```

### 5.2 RPC Endpoint

Use `SOLANA_RPC_URL` (Helius standard endpoint: `marielle-qe2lvr-fast-mainnet.helius-rpc.com`).

**Parameters:**

```json
{
  "encoding": "base64",
  "skipPreflight": true,
  "preflightCommitment": "processed",
  "maxRetries": 3
}
```

- `skipPreflight: true` -- we already validated the tx locally. Preflight adds ~100ms.
- `maxRetries: 3` -- let the RPC node retry a few times for us.

### 5.3 Which Transaction to Send

For sendTransaction fallback, build a **no-tip variant** that omits the MEV tip transfer instruction entirely. This avoids wasting tip on a non-MEV path where it provides zero priority benefit. Priority fee alone determines scheduling in standard RPC.

This requires a third skeleton variant: `fallback_skeleton` with only 3 instructions (CU limit, CU price, sell) -- no tip transfer. Build it at position open alongside the Jito and Nozomi skeletons.

### 5.4 Expected Landing Rate

- Jito bundle alone: ~85-95% landing rate
- Nozomi alone: ~85-95% landing rate
- Dual submit (Jito + Nozomi): ~95-99% combined (1 - (1-0.9)^2 = 99%)
- sendTransaction via standard RPC: ~60-70% (no MEV priority)
- sendTransaction with 10x priority fee: ~75-80%

The fallback exists to handle the ~1% case where both MEV networks are down simultaneously. Worth implementing but not worth optimizing.

### 5.5 Priority Fee for Fallback

When falling back to sendTransaction, the priority fee becomes the ONLY mechanism for landing priority. Multiply the existing `priority_fee_lamports` by 10x for fallback:

```rust
let fallback_priority_fee = config.priority_fee_lamports.saturating_mul(10);
```

This is still cheap: 50 microlamports x 10 = 500 microlamports per CU x 200,000 CU = 100,000 lamports (0.0001 SOL).

---

## 6. Staked Connection Path (Future -- DEFERRED)

### 6.1 What Staking with Helius Means

Helius offers a staked connection endpoint (`staked.helius-rpc.com`) that provides:
- Transaction forwarding via Helius's staked validator connections
- Higher landing rate than standard `sendTransaction` (~80-90% vs ~60-70%)
- No rate limiting on sends (vs standard endpoint rate limits)
- Requires staking SOL to a Helius-affiliated validator

### 6.2 Why It's Deprioritized

Nozomi parallel submit provides equivalent landing rate improvement:
- Staked Helius: ~80-90% single-path landing
- Nozomi: ~85-95% landing rate
- Our dual Jito+Nozomi: ~95-99% combined

Adding staked Helius as a THIRD path would push us to ~99.5%+, but the marginal gain is tiny and requires locking capital.

### 6.3 Minimum Viable Stake

Helius typically requires staking to validators in their network. Current minimum varies but ecosystem standard is ~100-500 SOL minimum for meaningful priority routing benefits. This is disproportionate to our 0.71 SOL bankroll.

### 6.4 When to Revisit

Revisit when:
1. Bankroll exceeds 10 SOL (stake doesn't represent >5% of capital)
2. We observe dual-submit landing rate consistently below 95%
3. Nozomi introduces pricing changes that make it uneconomical

### 6.5 Integration Path (When Ready)

When the time comes, add a third parallel path:

```rust
let (jito_res, nozomi_res, staked_res) = tokio::join!(
    self.jito.submit_bundle(&jito_tx),
    self.nozomi.send_transaction(&nozomi_tx),
    self.staked.send_transaction(&staked_tx),  // Future
);
```

Config field (pre-plumb now, leave disabled): `staked_helius_enabled: bool` (default: false)

---

## 7. Implementation Priority

### P0: Before Go-Live (MUST HAVE)

**1. Nozomi parallel submit** -- `NozomiClient` struct + `DualSubmitter` wrapper
- Doubles landing rate for sells
- Only fires for positions >= 0.20 SOL
- Requires: Nozomi API key, two skeletons per position
- Effort: ~2 days

**2. Bundle confirmation tracking** -- poll `getSignatureStatuses` after submit
- Required for tip escalation to work
- Feed results into `TipEngine::record_result()`
- Effort: ~1 day

**3. Tip escalation on sell retry** -- L0 -> L1 -> L2 -> ABANDON ladder
- Depends on confirmation tracking
- 25% PnL cap on tip
- Effort: ~1 day

### P1: First Week Live

**4. sendTransaction fallback** -- fire-and-forget to Helius RPC when both MEV networks fail
- `fallback_skeleton` variant (no tip instruction, priority fee only)
- 10x priority fee multiplier for fallback
- Effort: ~0.5 days

**5. Nozomi endpoint latency benchmark** -- run `findFastestEndpoints()` equivalent from our VPS
- Determine optimal primary/secondary Nozomi endpoints
- May require switching from auto-routed to specific regional endpoint
- Effort: ~0.5 days

### P2: Deferred (Nice-to-Have)

**6. Staked Helius connection** -- requires SOL stake, revisit when bankroll > 10 SOL

**7. Triple parallel submit** (Jito + Nozomi + Staked) -- when staked is available

**8. Dynamic endpoint selection** -- runtime latency probing to pick fastest Nozomi endpoint per-submit

---

## 8. Config Fields

All new fields go in `canary.json`. Group them under the existing `execution` section or a new top-level `tx_submission` section -- engineer's call on where these fit best alongside existing `execution.private_route` fields.

### 8.1 Tip Escalation Config

| Field | Type | Default | Description | Valid Range |
|-------|------|---------|-------------|-------------|
| `tip_escalation_enabled` | `bool` | `true` | Enable tip escalation on sell retry | -- |
| `tip_escalation_l1_delay_ms` | `u64` | `500` | ms after L0 to trigger L1 escalation | 200-2000 |
| `tip_escalation_l2_delay_ms` | `u64` | `500` | ms after L1 to trigger L2 escalation | 200-2000 |
| `tip_escalation_l1_multiplier_bp` | `u16` | `200` | L1 tip multiplier (percent of L0: 200 = 2.0x) | 100-1000 |
| `tip_escalation_l2_multiplier_bp` | `u16` | `400` | L2 tip multiplier (percent of L0: 400 = 4.0x) | 100-2000 |
| `tip_escalation_max_lamports` | `u64` | `10_000_000` | Absolute max tip at any level (lamports) | 1_000_000-100_000_000 |
| `tip_escalation_pnl_cap_bp` | `u16` | `2500` | Max tip as bps of gross PnL (2500 = 25%) | 500-5000 |
| `tip_escalation_abandon_ms` | `u64` | `1500` | Total ms from first submit to ABANDON | 500-5000 |

### 8.2 Nozomi Config

| Field | Type | Default | Description | Valid Range |
|-------|------|---------|-------------|-------------|
| `nozomi_enabled` | `bool` | `true` | Enable Nozomi parallel submit | -- |
| `nozomi_primary_url` | `String` | `"https://nozomi.temporal.xyz"` | Primary Nozomi endpoint (auto-routed) | Valid HTTPS URL |
| `nozomi_secondary_url` | `String` | `"https://ams1.nozomi.temporal.xyz"` | Secondary Nozomi endpoint | Valid HTTPS URL |
| `nozomi_api_key_env` | `String` | `"NOZOMI_API_KEY"` | Env var name holding the API key | Non-empty string |
| `nozomi_timeout_ms` | `u64` | `3000` | Per-request timeout | 1000-10000 |
| `nozomi_min_tip_lamports` | `u64` | `1_000_000` | Nozomi minimum tip floor | 1_000_000-10_000_000 |
| `nozomi_dual_submit_min_size_lamports` | `u64` | `200_000_000` | Min position size for dual submit (0.20 SOL) | 0-1_000_000_000 |
| `nozomi_cu_price_microlamports` | `u64` | `1_000_000` | Compute unit price for Nozomi txs | 100_000-10_000_000 |

### 8.3 Confirmation Tracking Config

| Field | Type | Default | Description | Valid Range |
|-------|------|---------|-------------|-------------|
| `confirm_poll_enabled` | `bool` | `true` | Enable post-submit confirmation polling | -- |
| `confirm_poll_interval_ms` | `u64` | `200` | Interval between confirmation polls | 100-1000 |
| `confirm_poll_max_attempts` | `u8` | `8` | Max polls before treating as unconfirmed | 1-20 |

### 8.4 Fallback Config

| Field | Type | Default | Description | Valid Range |
|-------|------|---------|-------------|-------------|
| `rpc_fallback_enabled` | `bool` | `true` | Enable sendTransaction fallback | -- |
| `rpc_fallback_priority_fee_multiplier` | `u16` | `10` | Multiply base priority fee for fallback | 1-100 |
| `rpc_fallback_max_retries` | `u8` | `3` | maxRetries param for sendTransaction | 0-10 |

### 8.5 Future (Pre-Plumb, Disabled)

| Field | Type | Default | Description | Valid Range |
|-------|------|---------|-------------|-------------|
| `staked_helius_enabled` | `bool` | `false` | Enable staked Helius tx submission | -- |
| `staked_helius_url` | `String` | `"https://staked.helius-rpc.com"` | Staked Helius RPC URL | Valid HTTPS URL |

---

## 9. Code Changes

### 9.1 New File: `tx/nozomi.rs`

**Purpose:** Persistent HTTP/2 client for Nozomi `sendTransaction` submission. Mirrors `jito_grpc.rs` architecture but uses standard `sendTransaction` RPC instead of Jito's `sendBundle`.

**Structs needed:**

```rust
/// 17 Nozomi tip accounts. Select randomly per transaction to avoid
/// write lock contention (per Nozomi docs).
pub const NOZOMI_TIP_ACCOUNTS: [&str; 17] = [
    "TEMPaMeCRFAS9EKF53Jd6KpHxgL47uWLcpFArU1Fanq",
    "noz3jAjPiHuBPqiSPkkugaJDkJscPuRhYnSpbi8UvC4",
    "noz3str9KXfpKknefHji8L1mPgimezaiUyCHYMDv1GE",
    "noz6uoYCDijhu1V7cutCpwxNiSovEwLdRHPwmgCGDNo",
    "noz9EPNcT7WH6Sou3sr3GGjHQYVkN3DNirpbvDkv9YJ",
    "nozc5yT15LazbLTFVZzoNZCwjh3yUtW86LoUyqsBu4L",
    "nozFrhfnNGoyqwVuwPAW4aaGqempx4PU6g6D9CJMv7Z",
    "nozievPk7HyK1Rqy1MPJwVQ7qQg2QoJGyP71oeDwbsu",
    "noznbgwYnBLDHu8wcQVCEw6kDrXkPdKkydGJGNXGvL7",
    "nozNVWs5N8mgzuD3qigrCG2UoKxZttxzZ85pvAQVrbP",
    "nozpEGbwx4BcGp6pvEdAh1JoC2CQGZdU6HbNP1v2p6P",
    "nozrhjhkCr3zXT3BiT4WCodYCUFeQvcdUkM7MqhKqge",
    "nozrwQtWhEdrA6W8dkbt9gnUaMs52PdAv5byipnadq3",
    "nozUacTVWub3cL4mJmGCYjKZTnE9RbdY5AP46iQgbPJ",
    "nozWCyTPppJjRuw2fpzDhhWbW355fzosWSzrrMYB1Qk",
    "nozWNju6dY353eMkMqURqwQEoM3SFgEKC6psLCSfUne",
    "nozxNBgWohjR75vdspfxR5H9ceC7XXH99xpxhVGt3Bb",
];

#[derive(Debug, Clone)]
pub struct NozomiConfig {
    pub primary_url: String,          // e.g. "https://nozomi.temporal.xyz"
    pub secondary_url: String,        // e.g. "https://ams1.nozomi.temporal.xyz"
    pub api_key: String,              // From env NOZOMI_API_KEY
    pub timeout_ms: u64,              // 3000
    pub min_tip_lamports: u64,        // 1_000_000
    pub cu_price_microlamports: u64,  // 1_000_000
}

pub struct NozomiClient {
    primary: HttpClient,     // Persistent HTTP/2 to primary endpoint
    secondary: HttpClient,   // Persistent HTTP/2 to secondary endpoint
    config: NozomiConfig,
    primary_url: String,     // Pre-built: "{url}/?c={api_key}"
    secondary_url: String,

    // Atomic stats
    pub txs_sent: AtomicU64,
    pub txs_failed: AtomicU64,
    pub primary_failures: AtomicU64,
    pub secondary_failures: AtomicU64,
    pub failovers: AtomicU64,
}
```

**Key methods:**

```rust
impl NozomiClient {
    /// Create client with dual persistent HTTP/2 connections.
    /// Same builder pattern as JitoGrpcClient::build_endpoint().
    pub async fn new(config: NozomiConfig) -> Result<Self>;

    /// Warmup both connections (send getTipAccounts or /ping).
    pub async fn warmup(&self) -> Result<()>;

    /// Submit a base64-encoded signed transaction via sendTransaction.
    /// Primary-first with secondary failover (same pattern as Jito).
    pub async fn send_transaction(&self, tx_base64: &str) -> Result<String>;

    /// Select a random Nozomi tip account pubkey.
    pub fn random_tip_account() -> Pubkey;

    /// Get stats snapshot.
    pub fn stats(&self) -> NozomiStats;
}
```

**HTTP/2 client configuration** — identical to `JitoGrpcClient::build_endpoint()`:
- `http2_prior_knowledge()` -- Nozomi endpoints support h2
- `tcp_nodelay(true)`
- `pool_max_idle_per_host(4)`
- `pool_idle_timeout(90s)`
- `http2_keep_alive_interval(10s)`
- `connect_timeout(3000ms)`

**Logging:**
- INFO: "Nozomi client initialized", "Nozomi tx submitted" (with sig, endpoint, elapsed_ms)
- WARN: "Nozomi primary failed, falling back to secondary"
- DEBUG: "Nozomi response" (status, elapsed_ms)

**Register in `tx/mod.rs`:**
```rust
pub mod nozomi;
pub use nozomi::{NozomiClient, NozomiConfig, NozomiStats, NOZOMI_TIP_ACCOUNTS};
```

---

### 9.2 New File: `tx/dual_submitter.rs`

**Purpose:** Orchestrates parallel submission to Jito + Nozomi with tip escalation and confirmation tracking.

```rust
/// Result of a dual submission attempt.
pub enum DualSubmitResult {
    /// At least one path confirmed within budget.
    Confirmed {
        signature: String,
        network: SubmitNetwork,   // Jito or Nozomi
        escalation_level: u8,     // 0, 1, or 2
        elapsed_ms: u64,
    },
    /// Neither path confirmed after full escalation ladder.
    Abandoned {
        last_jito_err: Option<String>,
        last_nozomi_err: Option<String>,
        escalation_level: u8,
        elapsed_ms: u64,
    },
    /// Both networks unreachable; fell back to RPC sendTransaction.
    FellBackToRpc {
        signature: String,
        elapsed_ms: u64,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum SubmitNetwork {
    Jito,
    Nozomi,
    RpcFallback,
}

pub struct DualSubmitter {
    jito: Arc<JitoGrpcClient>,
    nozomi: Option<Arc<NozomiClient>>,  // None if nozomi_enabled=false
    rpc_client: Arc<RpcClient>,          // For confirmation polling + fallback
    tip_engine: Arc<Mutex<TipEngine>>,
    config: DualSubmitConfig,
    blockhash_cache: Arc<BlockhashCache>,
}

#[derive(Debug, Clone)]
pub struct DualSubmitConfig {
    // Escalation
    pub escalation_enabled: bool,
    pub l1_delay_ms: u64,
    pub l2_delay_ms: u64,
    pub l1_multiplier_bp: u16,    // 200 = 2.0x
    pub l2_multiplier_bp: u16,    // 400 = 4.0x
    pub max_tip_lamports: u64,
    pub pnl_cap_bp: u16,
    pub abandon_ms: u64,
    // Nozomi
    pub nozomi_enabled: bool,
    pub nozomi_min_size_lamports: u64,
    // Confirmation
    pub confirm_poll_interval_ms: u64,
    // Fallback
    pub rpc_fallback_enabled: bool,
    pub rpc_fallback_priority_multiplier: u16,
}
```

**Core method: `submit_sell()`**

```rust
impl DualSubmitter {
    /// Submit a sell transaction with escalation and dual-submit.
    ///
    /// Flow:
    /// 1. Build Jito tx + Nozomi tx from skeletons with L0 tip
    /// 2. tokio::join!(jito.submit_bundle, nozomi.send_transaction)
    /// 3. Poll getSignatureStatuses every 200ms
    /// 4. If not confirmed by l1_delay_ms: escalate tip to L1, rebuild + resubmit
    /// 5. If not confirmed by l1+l2_delay_ms: escalate to L2, rebuild + resubmit
    /// 6. If not confirmed by abandon_ms: return Abandoned
    /// 7. On both-networks-unreachable: sendTransaction fallback
    pub async fn submit_sell(
        &self,
        jito_skeleton: &TxSkeleton,
        nozomi_skeleton: &TxSkeleton,
        fallback_skeleton: &TxSkeleton,  // No-tip variant for RPC fallback
        wallet: &Keypair,
        tokens_to_sell: u64,
        min_sol_out: u64,
        gross_pnl_lamports: i64,
        tip_context: TipContext,
    ) -> DualSubmitResult {
        let start = Instant::now();

        for level in 0..=2u8 {
            // 1. Compute tip for this level
            let base_tip = self.tip_engine.lock().compute_tip(
                gross_pnl_lamports, tip_context
            );
            let multiplier = match level {
                0 => 100,  // 1.0x
                1 => self.config.l1_multiplier_bp,
                2 => self.config.l2_multiplier_bp,
                _ => unreachable!(),
            };
            let escalated_tip = (base_tip as u128 * multiplier as u128 / 100) as u64;

            // Apply PnL cap (skip if SL exit / negative PnL)
            let tip = if gross_pnl_lamports > 0 {
                let pnl_cap = (gross_pnl_lamports as u64)
                    .saturating_mul(self.config.pnl_cap_bp as u64) / 10_000;
                escalated_tip
                    .min(self.config.max_tip_lamports)
                    .min(pnl_cap)
                    .max(base_tip)  // Never go below base
            } else {
                escalated_tip.min(self.config.max_tip_lamports)
            };

            // Nozomi tip: enforce minimum
            let nozomi_tip = tip.max(self.config.nozomi_min_tip);

            // 2. Patch skeletons + sign
            let blockhash = self.blockhash_cache.get();
            let jito_tx = patch_sign_serialize(
                jito_skeleton, wallet, tokens_to_sell,
                min_sol_out, &blockhash, tip
            );
            let nozomi_tx = patch_sign_serialize(
                nozomi_skeleton, wallet, tokens_to_sell,
                min_sol_out, &blockhash, nozomi_tip
            );

            // 3. Dual submit
            let use_nozomi = self.config.nozomi_enabled
                && position_size >= self.config.nozomi_min_size_lamports;
            let (jito_res, nozomi_res) = if use_nozomi {
                tokio::join!(
                    self.jito.submit_bundle(&jito_tx),
                    self.nozomi.as_ref().unwrap().send_transaction(&nozomi_tx),
                )
            } else {
                let jito_res = self.jito.submit_bundle(&jito_tx).await;
                (jito_res, Err(anyhow!("nozomi disabled")))
            };

            // Collect signatures to poll
            let mut sigs_to_poll = Vec::with_capacity(2);
            if let Ok(ref sig) = jito_res { sigs_to_poll.push(sig.clone()); }
            if let Ok(ref sig) = nozomi_res { sigs_to_poll.push(sig.clone()); }

            if sigs_to_poll.is_empty() && level == 2 {
                // Both networks failed on final level — RPC fallback
                if self.config.rpc_fallback_enabled {
                    return self.rpc_fallback(
                        fallback_skeleton, wallet, tokens_to_sell,
                        min_sol_out, &blockhash, start
                    ).await;
                }
                return DualSubmitResult::Abandoned { /* ... */ };
            }

            // 4. Poll for confirmation
            let delay_ms = match level {
                0 => self.config.l1_delay_ms,
                1 => self.config.l2_delay_ms,
                2 => self.config.abandon_ms - self.config.l1_delay_ms
                     - self.config.l2_delay_ms,
                _ => unreachable!(),
            };
            if let Some(confirmed_sig) = self.poll_confirmation(
                &sigs_to_poll, delay_ms
            ).await {
                let network = if jito_res.as_ref().ok() == Some(&confirmed_sig) {
                    SubmitNetwork::Jito
                } else {
                    SubmitNetwork::Nozomi
                };
                self.tip_engine.lock().record_result(true);
                return DualSubmitResult::Confirmed {
                    signature: confirmed_sig,
                    network,
                    escalation_level: level,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                };
            }

            // Not confirmed — escalate (loop continues)
            info!(
                level,
                elapsed_ms = start.elapsed().as_millis(),
                "sell not confirmed, escalating tip"
            );
        }

        self.tip_engine.lock().record_result(false);
        DualSubmitResult::Abandoned {
            last_jito_err: None,
            last_nozomi_err: None,
            escalation_level: 2,
            elapsed_ms: start.elapsed().as_millis() as u64,
        }
    }

    /// Poll getSignatureStatuses for up to `budget_ms` milliseconds.
    async fn poll_confirmation(
        &self,
        signatures: &[String],
        budget_ms: u64,
    ) -> Option<String> {
        let deadline = Instant::now() + Duration::from_millis(budget_ms);
        loop {
            tokio::time::sleep(Duration::from_millis(
                self.config.confirm_poll_interval_ms
            )).await;

            if Instant::now() >= deadline {
                return None;
            }

            // Single RPC call for all signatures
            match self.rpc_client.get_signature_statuses(signatures).await {
                Ok(statuses) => {
                    for (i, status) in statuses.iter().enumerate() {
                        if let Some(s) = status {
                            if s.err.is_none() {
                                return Some(signatures[i].clone());
                            }
                        }
                    }
                },
                Err(e) => {
                    debug!("confirmation poll failed: {}", e);
                    // Continue polling — transient RPC error
                }
            }
        }
    }
}
```

**Logging requirements:**

| Event | Level | Fields |
|-------|-------|--------|
| Sell submitted (L0) | INFO | mint, position_size, tip_jito, tip_nozomi, dual_submit |
| Confirmation received | INFO | sig, network, level, elapsed_ms |
| Escalation triggered | INFO | level, new_tip, elapsed_ms |
| Sell abandoned | WARN | mint, position_size, elapsed_ms, last_errors |
| RPC fallback triggered | WARN | mint, jito_err, nozomi_err |
| RPC fallback success | INFO | sig, elapsed_ms |
| Both networks unreachable | ERROR | jito_err, nozomi_err |
| PnL cap applied | DEBUG | raw_tip, capped_tip, gross_pnl |
| Confirmation poll failed | DEBUG | error |

**Register in `tx/mod.rs`:**
```rust
pub mod dual_submitter;
pub use dual_submitter::{DualSubmitter, DualSubmitConfig, DualSubmitResult, SubmitNetwork};
```

---

### 9.3 Modify: `tx/skeleton.rs`

**Change:** Add ability to build skeletons with different tip account recipients.

Current: `build_sell_skeleton()` hardcodes `JITO_TIP_ACCOUNTS[0]` as the tip recipient.

New: Add a `tip_recipient: &Pubkey` parameter.

```rust
impl TxSkeleton {
    /// Build a sell-transaction skeleton for a specific tip recipient.
    /// Called once per position per MEV network at position open time.
    pub fn build_sell_skeleton(
        mint: &[u8; 32],
        bonding_curve: &[u8; 32],
        assoc_bonding_curve: &[u8; 32],
        wallet_pubkey: &[u8; 32],
        tokens_held: u64,
        tip_recipient: &Pubkey,  // NEW: Jito or Nozomi tip account
    ) -> Result<Self, SkeletonError> {
        // ... same as today but use tip_recipient instead of JITO_TIP_ACCOUNTS[0]
    }

    /// Build a sell-transaction skeleton with NO tip instruction.
    /// Used for RPC sendTransaction fallback.
    /// Instructions: [CU limit, CU price, sell] — no tip transfer.
    pub fn build_sell_skeleton_no_tip(
        mint: &[u8; 32],
        bonding_curve: &[u8; 32],
        assoc_bonding_curve: &[u8; 32],
        wallet_pubkey: &[u8; 32],
        tokens_held: u64,
    ) -> Result<Self, SkeletonError> {
        // Same as build_sell_skeleton but omit the system_transfer tip ix.
        // PatchOffsets.tip_amount will be unused (set to 0).
    }
}
```

**Note:** The no-tip skeleton will have different offsets for vsol/vtoken/blockhash because the message is shorter (3 instructions instead of 4, fewer account keys). The PatchOffsets struct handles this naturally -- offsets are computed at build time by scanning for sentinels.

---

### 9.4 Modify: `tx/executor.rs`

**Change:** Wire `DualSubmitter` into `execute_sell()`.

Current flow:
```
execute_sell() -> build tx via TxBuilder -> jito.submit_bundle()
```

New flow:
```
execute_sell() -> dual_submitter.submit_sell() -> Jito+Nozomi parallel -> confirm -> escalate
```

The executor should store the three pre-built skeletons per position:

```rust
pub struct PositionSkeletons {
    pub jito: TxSkeleton,
    pub nozomi: TxSkeleton,
    pub fallback: TxSkeleton,  // No-tip variant
}
```

Build these at position open time (`execute_buy()` success callback). The skeletons are ~768 bytes each, so 3 skeletons = ~2.3 KB per position. With max 5 concurrent positions = ~12 KB total. Negligible.

`execute_buy()` remains unchanged -- single Jito submit, no escalation, no Nozomi.

---

### 9.5 Modify: `tx/tip_engine.rs`

**Changes:**

1. **Add Nozomi tip floor enforcement:**

```rust
impl TipEngine {
    /// Compute tip for Nozomi submission.
    /// Same as compute_tip() but enforces Nozomi minimum floor.
    #[inline(always)]
    pub fn compute_nozomi_tip(
        &self,
        gross_profit_lamports: i64,
        context: TipContext,
        nozomi_min_tip: u64,
    ) -> u64 {
        self.compute_tip(gross_profit_lamports, context)
            .max(nozomi_min_tip)
    }
}
```

2. **Increase `max_tip` default** from 5,000,000 to 10,000,000 to accommodate L2 escalation:

```rust
impl Default for TipConfig {
    fn default() -> Self {
        Self {
            // ... existing fields ...
            max_tip: 10_000_000,  // Was 5,000,000. Increased for L2 escalation.
            // ...
        }
    }
}
```

---

### 9.6 Modify: `tx/builder.rs`

**Change:** Add Nozomi tip accounts constant and random selection.

```rust
/// Nozomi tip accounts — select randomly per transaction to avoid
/// write lock contention (per Nozomi documentation).
pub const NOZOMI_TIP_ACCOUNTS: [&str; 17] = [
    "TEMPaMeCRFAS9EKF53Jd6KpHxgL47uWLcpFArU1Fanq",
    // ... all 17 accounts as listed in section 9.1 ...
];

impl TxBuilder {
    /// Select a random Nozomi tip account.
    pub fn random_nozomi_tip_account() -> Pubkey {
        use rand::Rng;
        let idx = rand::thread_rng().gen_range(0..NOZOMI_TIP_ACCOUNTS.len());
        Pubkey::from_str(NOZOMI_TIP_ACCOUNTS[idx]).unwrap()
    }
}
```

---

### 9.7 New Enum: `tx/dual_submitter.rs` — Error Types

```rust
#[derive(Debug)]
pub enum DualSubmitError {
    /// Both MEV networks failed at the network level.
    BothNetworksFailed {
        jito_err: String,
        nozomi_err: String,
    },
    /// Transaction was rejected (bad blockhash, etc) — escalate, don't fallback.
    TxRejected {
        reason: String,
    },
    /// Skeleton patching or signing failed — fatal, don't retry.
    BuildFailed {
        reason: String,
    },
}
```

---

### 9.8 Config Loading Changes: `engine/config.rs`

Add deserialization for new config sections. These can be a new `TxSubmissionJsonConfig` struct under the `execution` section:

```rust
#[derive(Deserialize, Debug, Default)]
pub struct TxSubmissionJsonConfig {
    // Tip escalation
    pub tip_escalation_enabled: Option<bool>,
    pub tip_escalation_l1_delay_ms: Option<u64>,
    pub tip_escalation_l2_delay_ms: Option<u64>,
    pub tip_escalation_l1_multiplier_bp: Option<u16>,
    pub tip_escalation_l2_multiplier_bp: Option<u16>,
    pub tip_escalation_max_lamports: Option<u64>,
    pub tip_escalation_pnl_cap_bp: Option<u16>,
    pub tip_escalation_abandon_ms: Option<u64>,

    // Nozomi
    pub nozomi_enabled: Option<bool>,
    pub nozomi_primary_url: Option<String>,
    pub nozomi_secondary_url: Option<String>,
    pub nozomi_api_key_env: Option<String>,
    pub nozomi_timeout_ms: Option<u64>,
    pub nozomi_min_tip_lamports: Option<u64>,
    pub nozomi_dual_submit_min_size_lamports: Option<u64>,
    pub nozomi_cu_price_microlamports: Option<u64>,

    // Confirmation
    pub confirm_poll_enabled: Option<bool>,
    pub confirm_poll_interval_ms: Option<u64>,
    pub confirm_poll_max_attempts: Option<u8>,

    // Fallback
    pub rpc_fallback_enabled: Option<bool>,
    pub rpc_fallback_priority_fee_multiplier: Option<u16>,
    pub rpc_fallback_max_retries: Option<u8>,

    // Future
    pub staked_helius_enabled: Option<bool>,
    pub staked_helius_url: Option<String>,
}
```

Add to `canary.json` root or under `execution.tx_submission`:

```json
{
  "execution": {
    "tx_submission": {
      "nozomi_enabled": true,
      "nozomi_primary_url": "https://nozomi.temporal.xyz",
      "nozomi_secondary_url": "https://ams1.nozomi.temporal.xyz",
      "nozomi_api_key_env": "NOZOMI_API_KEY",
      "nozomi_min_tip_lamports": 1000000,
      "nozomi_dual_submit_min_size_lamports": 200000000,
      "nozomi_cu_price_microlamports": 1000000,
      "tip_escalation_enabled": true,
      "tip_escalation_l1_delay_ms": 500,
      "tip_escalation_l2_delay_ms": 500,
      "tip_escalation_l1_multiplier_bp": 200,
      "tip_escalation_l2_multiplier_bp": 400,
      "tip_escalation_max_lamports": 10000000,
      "tip_escalation_pnl_cap_bp": 2500,
      "tip_escalation_abandon_ms": 1500,
      "confirm_poll_enabled": true,
      "confirm_poll_interval_ms": 200,
      "confirm_poll_max_attempts": 8,
      "rpc_fallback_enabled": true,
      "rpc_fallback_priority_fee_multiplier": 10,
      "rpc_fallback_max_retries": 3,
      "staked_helius_enabled": false
    }
  }
}
```

---

## 10. Open Questions for Engineer

1. **Nozomi API key acquisition:** Sign up at temporal.xyz, get key from dashboard. Do we have an account? If not, create one before starting implementation.

2. **Nozomi endpoint benchmarking:** Before hardcoding endpoints, run latency tests from the Frankfurt VPS. The auto-routed endpoint may already be optimal, or a specific regional endpoint (ams1, fra1 if it exists) may be faster.

3. **Skeleton tip recipient patching:** The current `TxSkeleton` locates fields by sentinel values. The tip recipient is an account key in the compiled message, not instruction data — it's in the accounts array. Two options:
   - **Option A (recommended):** Build separate skeletons per network at position open (cold path, ~10us each). This is simpler and avoids patching account keys.
   - **Option B:** Add a `tip_recipient` patch offset to `PatchOffsets` and scan for a sentinel pubkey. More complex, fragile if message layout changes.

4. **Nozomi `sendTransaction` vs `/api/sendTransaction2`:** The Nozomi SDK mentions a v2 endpoint. Research whether `/api/sendTransaction2` exists on Nozomi's HTTP API and if it offers advantages over standard `sendTransaction`.

---

## Appendix A: Nozomi Tip Accounts (Full List)

```
TEMPaMeCRFAS9EKF53Jd6KpHxgL47uWLcpFArU1Fanq
noz3jAjPiHuBPqiSPkkugaJDkJscPuRhYnSpbi8UvC4
noz3str9KXfpKknefHji8L1mPgimezaiUyCHYMDv1GE
noz6uoYCDijhu1V7cutCpwxNiSovEwLdRHPwmgCGDNo
noz9EPNcT7WH6Sou3sr3GGjHQYVkN3DNirpbvDkv9YJ
nozc5yT15LazbLTFVZzoNZCwjh3yUtW86LoUyqsBu4L
nozFrhfnNGoyqwVuwPAW4aaGqempx4PU6g6D9CJMv7Z
nozievPk7HyK1Rqy1MPJwVQ7qQg2QoJGyP71oeDwbsu
noznbgwYnBLDHu8wcQVCEw6kDrXkPdKkydGJGNXGvL7
nozNVWs5N8mgzuD3qigrCG2UoKxZttxzZ85pvAQVrbP
nozpEGbwx4BcGp6pvEdAh1JoC2CQGZdU6HbNP1v2p6P
nozrhjhkCr3zXT3BiT4WCodYCUFeQvcdUkM7MqhKqge
nozrwQtWhEdrA6W8dkbt9gnUaMs52PdAv5byipnadq3
nozUacTVWub3cL4mJmGCYjKZTnE9RbdY5AP46iQgbPJ
nozWCyTPppJjRuw2fpzDhhWbW355fzosWSzrrMYB1Qk
nozWNju6dY353eMkMqURqwQEoM3SFgEKC6psLCSfUne
nozxNBgWohjR75vdspfxR5H9ceC7XXH99xpxhVGt3Bb
```

Source: https://use.temporal.xyz/nozomi/tipping-and-faq (verified 2026-03-31)

## Appendix B: Nozomi Key Facts

- **Submission method:** Standard Solana `sendTransaction` RPC (not bundles)
- **Minimum tip:** 0.001 SOL (1,000,000 lamports)
- **Recommended CU price:** >= 1,000,000 microlamports
- **Automatic retries:** Yes, Nozomi retries until blockhash expires
- **Front-running protection:** Available (routes through trusted validator whitelist)
- **Requires API key:** Yes, query param `?c={API_KEY}`
- **Regional endpoints:** `{region}.nozomi.temporal.xyz` (pit1, ewr1, ams1, etc.)
- **Auto-routed endpoint:** `nozomi.temporal.xyz` (geo-DNS)
- **Connection warming:** Recommended. Ping endpoints every 30-60s.
- **Delivery paths:** Jito validators, Harmonic bundles, staked connections (Nozomi manages internally)

## Appendix C: Latency Budget Breakdown

```
Trailing stop fires (T=0)
  |
  +-- [0-1ms] TxSkeleton::patch() — two skeletons patched
  +-- [1-2ms] Ed25519 sign — two transactions signed
  +-- [2-5ms] tokio::join!(jito_submit, nozomi_submit) — wire time
  |
  +-- [200ms] First confirmation poll
  +-- [400ms] Second confirmation poll
  +-- [500ms] === L1 ESCALATION if no confirmation ===
  |     +-- [500-502ms] Patch + sign + submit with 2x tip
  |     +-- [700ms] Poll
  |     +-- [900ms] Poll
  +-- [1000ms] === L2 ESCALATION if no confirmation ===
  |     +-- [1000-1002ms] Patch + sign + submit with 4x tip
  |     +-- [1200ms] Poll
  |     +-- [1400ms] Poll
  +-- [1500ms] === ABANDON ===
```

Typical case (L0 confirms): **200-500ms** from stop trigger to confirmed exit.
Worst case (ABANDON): **1,500ms** from stop trigger. Position marked `exit_failed`.

---

*End of spec.*