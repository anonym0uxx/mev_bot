# RIDE API CORRECTIONS — Read BEFORE RIDE_SPEC_PART_A.md

The architect spec (RIDE_SPEC_PART_A.md) has incorrect API signatures for ride_state.rs.
Use THESE signatures — they match the ACTUAL compiled code:

## Correct ride_state.rs API

```rust
// The enum is RideDecision, NOT RideAction
pub enum RideDecision {
    Hold,
    Exit(RideExitReason),
}

pub enum RideExitReason {
    TrailingStop, HardFloor, WhaleExit,
    BuyGapTimeout, SellCascade, CreatorSell, MaxHold,
}

impl RideState {
    // Constructor
    pub fn new(
        entry_mvsol: u32,
        current_mvsol: u32,
        now_ms: u64,
        buy_rate_5s: u16,
        config: &RideConfig,
    ) -> Self;

    // Price tick — returns Hold or Exit
    pub fn on_tick(
        &mut self,
        current_mvsol: u32,
        now_ms: u64,
        config: &RideConfig,
    ) -> RideDecision;

    // Buy event — updates internal state, returns NOTHING (void)
    pub fn on_buy_event(&mut self, sol_amount_mvsol: u32, now_ms: u64);
    // NOTE: No config param, no return value!

    // Sell event — may trigger emergency exit
    pub fn on_sell_event(
        &mut self,
        sol_amount_mvsol: u32,
        now_ms: u64,
        config: &RideConfig,
    ) -> Option<RideExitReason>;
    // NOTE: Returns Option<RideExitReason>, NOT RideDecision!

    // Accessors
    pub fn phase(&self) -> u8;        // field: self.phase
    pub fn peak_mvsol(&self) -> u32;  // field: self.peak_mvsol
    pub fn ride_start_ms(&self) -> u64; // field: self.ride_start_ms
    pub fn unique_wallets(&self) -> u8; // field: self.unique_wallets
}
```

## Key differences from spec:
1. `RideAction` → `RideDecision` everywhere
2. `on_buy_event` takes only `(sol_amount_mvsol: u32, now_ms: u64)` — no config, no buyer_id, no return value
3. `on_sell_event` returns `Option<RideExitReason>`, not `RideDecision`
4. `new()` takes `(entry_mvsol, current_mvsol, now_ms, buy_rate_5s, &config)` — 5 params, not 3

## Conversion helper (already in ride_state.rs but not pub):
```rust
fn lamports_to_mvsol(lamports: u64) -> u32 {
    ((lamports + 500_000) / 1_000_000) as u32
}
```
You may need to add this as a local helper in positions.rs.

## Handling on_sell_event return in positions.rs:
```rust
// on_sell_event returns Option<RideExitReason>, wrap it:
if let Some(reason) = rs.on_sell_event(sell_mvsol, now_ms, &ride_config) {
    let exit_reason = map_ride_exit_reason(reason);
    // close position
}
// Then ALSO call on_tick() to check trail stop:
match rs.on_tick(current_mvsol, now_ms, &ride_config) {
    RideDecision::Exit(reason) => { /* close */ }
    RideDecision::Hold => {}
}
```

## Handling on_buy_event in positions.rs:
```rust
// on_buy_event returns nothing — just updates state
rs.on_buy_event(buy_mvsol, now_ms);
// Then call on_tick() to check trail/phase transitions:
match rs.on_tick(current_mvsol, now_ms, &ride_config) {
    RideDecision::Exit(reason) => { /* close */ }
    RideDecision::Hold => {}
}
```
