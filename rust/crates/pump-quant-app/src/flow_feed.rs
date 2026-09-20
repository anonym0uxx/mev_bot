//! The flow reducer's feed — the app-side adapter from a live engine print to a
//! `pump_quant_market_state::flow_reducer::FlowEvent`.
//!
//! WHY THIS EXISTS. `FlowReducer` is the live derivation of the thirteen `LIVE FLOW STATE`
//! features the entry prompt states, and it was **entirely unwired** — zero users outside its
//! own crate. Every served flow number would have had no producer. This adapter is that
//! producer's edge, and it follows the same discipline as the state ledger's: a print that
//! cannot honestly contribute is refused, and the reducer never sees a substituted clock, a
//! collapsed identity, or a zero that means "unknown".
//!
//! THE SIGN CONVENTION IS NOT OBVIOUS. `FlowEvent::sol_lamports` follows the *tape*: negative
//! for a buy, positive for a sell, because the tape records the trader's balance change (SOL
//! leaves them on a buy). The reducer's net-flow line is `-sum(sol_lamports)`, so a buy has to
//! be stored negative for net flow to come out POSITIVE. Getting this backwards inverts every
//! flow feature while leaving them all plausible, so a test feeds a buy through the real
//! reducer and asserts the served net flow is positive.
//!
//! FEE AND COMPUTE UNITS: the live `AppEvent::MarketTrade` carries neither. They are recorded
//! into the reducer's per-trade row but **no served aggregate reads them** — the only
//! references in `flow_reducer.rs` are the two `sort_unstable` calls that are themselves dead
//! work (worth removing at the source). Feeding `0` / `None` is therefore provably inert for
//! every emitted value; the moment a served feature reads them, this adapter must take them
//! from the wire instead.

use pump_quant_market_state::flow_reducer::{FlowEvent, Side};

/// Build the reducer's view of one live print, or `None` when the print cannot honestly
/// contribute.
///
/// Refusals, each a way to corrupt every flow feature at once:
/// * **no receive time** — the reducer's windows are millisecond-keyed; an absent clock is not
///   a licence to substitute one (the tape's own discipline);
/// * **an all-zero wallet** — the unknown-trader sentinel. The flow features are *identity*
///   features (`entrants_60s`, `entrants_300s`, `fresh_wallet_share`, the smart-wallet and
///   co-entry rules); collapsing every unknown wallet into one identity would read as a single
///   whale. The junction's `(mint, slot)` join is what supplies a real one;
/// * **the reducer wants the WALLET, not the engine's hashed entity id.** `Wallet` is
///   `[u8; 32]` because the identity the corpus reasons about is the address: freshness,
///   smart-wallet membership and co-entry are all address-keyed properties, and the engine's
///   `u64` entity is a lossy convenience for its own bitsets. This adapter therefore takes the
///   pubkey; the plumbing that must carry it (instruction decode → event → join) is noted in
///   `trade_join`'s next step, because today only the hash leaves the decode site.
/// * **zero quote or zero base** — not a swap, so it cannot be volume.
#[must_use]
pub fn flow_event_from_market_trade(
    mint: &[u8; 32],
    slot: u64,
    recv_unix_ms: Option<i64>,
    quote_lamports: u64,
    signed_base: i64,
    trader: [u8; 32],
    fee_lamports: u64,
    cu_consumed: Option<u64>,
) -> Option<FlowEvent> {
    let recv_unix_ms = recv_unix_ms?;
    if trader == [0u8; 32] || quote_lamports == 0 || signed_base == 0 {
        return None;
    }
    let is_buy = signed_base > 0;
    let quote = i64::try_from(quote_lamports).unwrap_or(i64::MAX);
    Some(FlowEvent {
        mint: *mint,
        trader,
        side: if is_buy { Side::Buy } else { Side::Sell },
        slot,
        recv_unix_ms,
        // The tape's convention: a buy is SOL leaving the trader, so it is negative here.
        sol_lamports: if is_buy { -quote } else { quote },
        fee_lamports,
        cu_consumed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_market_state::flow_reducer::{FlowOutcome, FlowReducer};

    const MINT: [u8; 32] = [17u8; 32];

    #[test]
    fn a_buy_is_stored_negative_so_the_served_net_flow_is_positive() {
        let mut reducer = FlowReducer::new();
        reducer.track_mint(MINT);
        let t0 = 1_700_000_000_000i64;
        let ev = flow_event_from_market_trade(
            &MINT,
            100,
            Some(t0),
            400_000_000,
            2_000_000,
            [42u8; 32],
            0,
            None,
        )
        .expect("a complete print");
        assert_eq!(ev.side, Side::Buy);
        assert_eq!(
            ev.sol_lamports, -400_000_000,
            "the tape records buys negative"
        );
        reducer.on_event(&ev);
        match reducer.serve(&MINT, t0 + 1_000) {
            FlowOutcome::Aggregates(a) => {
                assert!(
                    a.net_flow_sol_300s_micro > 0,
                    "a buy must read as INFLOW: {:?}",
                    a.net_flow_sol_300s_micro
                );
                assert_eq!(a.entrants_300s, 1);
            }
            other => panic!("expected aggregates, got {other:?}"),
        }
    }

    #[test]
    fn a_sell_is_stored_positive_and_reads_as_outflow() {
        let mut reducer = FlowReducer::new();
        reducer.track_mint(MINT);
        let t0 = 1_700_000_000_000i64;
        let sell = flow_event_from_market_trade(
            &MINT,
            101,
            Some(t0),
            250_000_000,
            -1_000_000,
            [9u8; 32],
            0,
            None,
        )
        .expect("complete");
        assert_eq!(sell.side, Side::Sell);
        assert_eq!(sell.sol_lamports, 250_000_000);
        reducer.on_event(&sell);
        match reducer.serve(&MINT, t0 + 1_000) {
            FlowOutcome::Aggregates(a) => assert!(a.net_flow_sol_300s_micro < 0),
            other => panic!("expected aggregates, got {other:?}"),
        }
    }

    #[test]
    fn prints_that_cannot_honestly_contribute_are_refused() {
        let t = Some(1_700_000_000_000i64);
        // No receive time: the windows are ms-keyed.
        assert!(flow_event_from_market_trade(&MINT, 1, None, 1, 1, [7u8; 32], 0, None).is_none());
        // Unknown trader: the sentinel would collapse every unknown wallet into one entity.
        assert!(
            flow_event_from_market_trade(&MINT, 1, t, 400_000_000, 1, [0u8; 32], 0, None).is_none()
        );
        // Not a swap.
        assert!(flow_event_from_market_trade(&MINT, 1, t, 0, 1, [7u8; 32], 0, None).is_none());
        assert!(
            flow_event_from_market_trade(&MINT, 1, t, 400_000_000, 0, [7u8; 32], 0, None).is_none()
        );
    }
}
