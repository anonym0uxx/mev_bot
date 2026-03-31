# ShredStream Primary Trigger — Master Architecture Design

**Author:** Apollo (Master Kelly/Solana MEV Quant Architect)
**Date:** 2026-03-30
**Status:** APPROVED FOR BUILD

---

## The Paradigm Shift

We now have Jito ShredStream WL. This gives us access to raw shreds ~80-200ms before ANY websocket feed (PumpPortal, Helius, etc.). The proxy decodes shreds into Solana `Entry` objects containing full `VersionedTransaction` data via gRPC on port 10002.

**Current state:** ShredStream parses raw UDP packets for Pump.fun discriminators → emits `PreWarm` (no mint, no vSOL, no trader, no sig)

**Target state:** ShredStream subscribes to gRPC decoded entries → parses full Pump.fun transactions → emits `FeedEvent::Trade` with complete `TradeEvent` fields → becomes the PRIMARY entry trigger

**PumpPortal demotes to:** confirming/enrichment feed (validates our ShredStream trades, fills gaps)

---

## Data Flow: What We Get From gRPC

The proxy's `SubscribeEntries` gRPC stream provides:

```
Entry {
  slot: u64,
  entries: bytes  // bincode-serialized Vec<solana_entry::entry::Entry>
}
```

Each `solana_entry::entry::Entry` contains `Vec<VersionedTransaction>`. Each `VersionedTransaction` has:
- `signatures: Vec<Signature>` — full 64-byte transaction signatures
- `message.account_keys: Vec<Pubkey>` — ALL account keys (mint, trader, bonding curve, etc.)
- `message.instructions: Vec<CompiledInstruction>` — program calls with data + account indices

For a Pump.fun buy/sell instruction, the account layout is:
```
accounts[0] = global config (fixed)
accounts[1] = fee recipient (fixed)
accounts[2] = mint
accounts[3] = bonding_curve
accounts[4] = associated_bonding_curve  
accounts[5] = trader (buyer/seller)
accounts[6] = trader ATA
accounts[7] = system_program
accounts[8] = token_program
accounts[9] = rent (optional)
accounts[10] = event_authority (optional)
accounts[11] = pump_program
```

Instruction data layout (after 8-byte discriminator):
```
[0..8]   discriminator (buy/sell)
[8..16]  token_amount: u64 (LE)
[16..24] max_sol_cost/min_sol_out: u64 (LE) — slippage parameter
```

**We do NOT get vSOL/vToken reserves directly from the instruction.** But we CAN compute them from the bonding curve account data if we cache the last known state + apply the trade delta. OR we can use a single RPC call to `getAccountInfo` on the bonding curve after seeing the shred — still faster than waiting for PumpPortal.

**Strategy:** For the first version, emit Trade events WITHOUT vSOL reserves (set to 0). The entry engine and watchlist can be modified to handle missing vSOL data by using cached values from PumpPortal when available. For the SECOND version (if needed), add bonding curve state caching.

---

## Architecture: 5 Components

### Component 1: gRPC Client (NEW)

**File:** `feeds/shredstream.rs` — new `run_grpc_loop()` method

- Connects to `http://127.0.0.1:10002` (local proxy gRPC)
- Subscribes via `ShredstreamProxyClient::subscribe_entries()`
- Receives `Entry { slot, entries }` stream
- Deserializes `Vec<solana_entry::entry::Entry>` via bincode
- For each transaction: calls `parse_pump_transaction()`
- Emits `FeedEvent::Trade(TradeEvent)` (NOT PreWarm)

**Dependencies:** `jito-protos` crate (already built in shredstream-proxy), `solana-entry`, `solana-transaction`, `bincode`

### Component 2: Transaction Parser (NEW)

**File:** `feeds/shredstream.rs` — new `parse_pump_transaction()` function

**Input:** `&VersionedTransaction`, `slot: u64`
**Output:** `Option<TradeEvent>`

Logic:
1. Find Pump.fun program invocation in `instructions[]`
2. Check if instruction data starts with BUY_DISCRIMINATOR or SELL_DISCRIMINATOR  
3. Extract account keys using instruction's `account_indices`:
   - `mint = message.account_keys[instr.accounts[2]]`
   - `bonding_curve = message.account_keys[instr.accounts[3]]`
   - `assoc_bonding_curve = message.account_keys[instr.accounts[4]]`
   - `trader = message.account_keys[instr.accounts[5]]`
4. Extract `token_amount` and `max_sol_cost` from instruction data
5. Extract `signature = tx.signatures[0]`
6. Construct `TradeEvent` with all fields populated

**vSOL handling:** Set `vsol_reserves = 0` and `vtoken_reserves = 0`. The hot path and watchlist will need to handle 0 as "unknown" — see Component 4.

### Component 3: Event Priority (MODIFY)

**File:** `feeds/event_joiner.rs`

ShredStream now emits `Trade` events. The EventJoiner must prioritize ShredStream over PumpPortal when both are available. Since `crossbeam_channel::select!` doesn't guarantee priority, we use `try_recv()` with priority:

```rust
// Priority: ShredStream > PumpPortal > Helius > Tick
if let Ok(ev) = s_rx.try_recv() { forward(ev, &e_tx); continue; }
if let Ok(ev) = pp_rx.try_recv() { forward(ev, &e_tx); continue; }
if let Ok(ev) = h_rx.try_recv() { forward(ev, &e_tx); continue; }
// Block on all with select! when nothing ready
select! { ... }
```

### Component 4: Hot Path — vSOL-Absent Entry Support (MODIFY)

**File:** `engine/hot_path.rs`, `engine/watchlist.rs`, `engine/entry_engine.rs`

When ShredStream provides a Trade with `vsol_reserves = 0`:

1. **Watchlist watch:** Skip the vSOL-based slippage check. Store 0 as entry_vsol_reserves.
2. **Watchlist promote:** Skip vSOL velocity check and slippage check when entry_vsol == 0.
3. **Entry scoring:** Features that depend on vSOL (curve position, fill rate) use cached values from MintMap if available, or default to neutral (0.5 score) if no cached data.
4. **When PumpPortal confirms** the same trade (sig-prefix match within 200ms), UPDATE the position's vSOL data with PumpPortal's authoritative values. This is a "lazy enrichment" pattern.

### Component 5: Dedup — ShredStream↔PumpPortal (MODIFY)

**File:** `engine/hot_path.rs`

When a PumpPortal Trade arrives for a mint that ShredStream already triggered:

1. Check sig_prefix match in a 200ms dedup window
2. If match: **enrich** the existing position/watchlist entry with PumpPortal's vSOL data, DO NOT re-trigger entry
3. If no match (different tx): process normally as a new trade

Use a small ring buffer of (sig_prefix_u64, timestamp_ms) for recent ShredStream-triggered trades.

---

## Kelly/Bayesian Model Implications

### Entry Model Changes
- **Speed premium:** ShredStream entries arrive ~80-200ms earlier. The Bayesian prior should be STRONGER for ShredStream-sourced entries because we're seeing genuine market activity before the herd.
- **EVIDENCE_WEIGHTS update:** ShredStream buy evidence should be weighted 15 (up from 12) for buys, and ShredStream sell evidence 20 (up from 15) for sells — reflecting higher signal quality from pre-confirmation data.
- **Score adjustment for missing vSOL:** When vSOL is absent, the magnitude score's curve_position and fill_rate features should use neutral values (50th percentile) rather than zeroing out. This prevents the score from being artificially depressed on ShredStream entries.

### Exit Model Changes  
- **Faster confirming evidence:** With ShredStream, buys_after_entry will increment FASTER (before PumpPortal sees them). This means the Bayesian model gets positive evidence earlier → less premature decay → fewer momentum_decay_flat exits.
- **The 500ms minimum hold we added is now even MORE important:** ShredStream events arrive so fast that without the hold floor, decay could push to exit before the first tick.

### Fee Model
- **No change to fee gate logic** — the fee-aware entry gate operates on Kelly conviction, which is independent of feed source.
- **However:** With earlier entries, we get BETTER positioning on the bonding curve → lower effective slippage → higher realized R. The fee gate's `DEFAULT_ROUND_TRIP_FEE_BP = 210` may be conservative once ShredStream positioning is live — monitor and adjust.

### Jito Bundle Advantage
- **Bundle timing:** See a buy on ShredStream → immediately construct our buy tx → submit as Jito bundle with tip → lands in SAME slot or next slot
- **Latency budget:** ShredStream → parse (1µs) → Kelly eval (5µs) → TX build (10µs) → bundle submit (50ms) = ~50ms total
- **vs PumpPortal:** WebSocket → parse → eval → submit = 80-200ms + 50ms = 130-250ms
- **Net advantage:** 80-200ms earlier positioning on every trade

---

## Build Order

1. **Component 2** — Transaction parser (pure function, fully testable)
2. **Component 1** — gRPC client (wire parser into feed loop)
3. **Component 4** — Hot path vSOL-absent support
4. **Component 5** — Dedup ring buffer
5. **Component 3** — EventJoiner priority
6. **Bayesian weight update** (EVIDENCE_WEIGHTS ShredStream boost)
7. **Integration test** — full pipeline verification

---

## Crate Dependencies

Need to add to `pump-quant-core/Cargo.toml`:
- `tonic` (gRPC client)
- `prost` (protobuf deserialization)
- `solana-sdk` (VersionedTransaction, Pubkey, Signature types)
- `solana-entry` (Entry struct for bincode deserialization)
- `bincode` (Entry deserialization)

The jito-protos crate from the shredstream-proxy can be used as a path dependency.

---

## Zero-Regression Guarantees

1. All existing 405 tests pass
2. PumpPortal/Helius/CoreCast feeds unaffected
3. Kelly LUT values, scoring weights (except EVIDENCE_WEIGHTS[ShredStream]), and Bayesian constants unchanged
4. Engine operates correctly with ShredStream disconnected (graceful fallback to PumpPortal-primary)
5. Paper trade logging format unchanged — ShredStream trades logged with `source: "shredstream"` field
