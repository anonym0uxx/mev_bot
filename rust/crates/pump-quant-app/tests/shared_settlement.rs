//! Shared BUY/ADD/SELL settlement laws: one path, `cash + committed == seed + realized` after every step.
use pump_quant_app::settlement::{
    FillReport, SettleError, Settled, SettlementLedger, NETWORK_FEE_LABEL,
    NETWORK_FEE_PER_LANDED_LEG_ESTIMATE,
};
use pump_quant_app::shadow_pool::LegKind;

const M: [u8; 32] = [1u8; 32];
const N: u64 = NETWORK_FEE_PER_LANDED_LEG_ESTIMATE;

fn r(leg: LegKind, id: u64, t: u64, v: u64, n: u64) -> FillReport {
    FillReport {
        mint: M,
        leg,
        order_id: id,
        cum_tokens: t,
        cum_venue_lamports: v,
        cum_network_lamports: n,
    }
}

fn step(l: &mut SettlementLedger, f: FillReport) -> Result<Settled, SettleError> {
    let out = l.settle(f);
    assert!(
        l.invariant_holds(),
        "cash+committed==seed+realized after {f:?}"
    );
    out
}

#[test]
fn network_fee_is_a_labelled_estimate_constant() {
    assert_eq!(N, 10_000);
    assert!(NETWORK_FEE_LABEL.contains("estimate"));
}

#[test]
fn buy_add_partial_sells_close_with_invariant_after_every_step() {
    let mut l = SettlementLedger::new(1_000_000_000);
    // entry BUY: partial then complete (cumulative reports; one landed leg -> one network fee)
    step(&mut l, r(LegKind::Entry, 1, 400, 40_000_000, N)).unwrap();
    step(&mut l, r(LegKind::Entry, 1, 1_000, 100_000_000, N)).unwrap();
    // ADD (management namespace)
    step(&mut l, r(LegKind::Add, 1, 500, 60_000_000, N)).unwrap();
    assert_eq!(l.holdings[&M].tokens, 1_500);
    assert_eq!(l.committed, 160_000_000 + 2 * i128::from(N));
    assert_eq!(l.cash, 1_000_000_000 - 160_000_000 - 2 * i128::from(N));
    // REDUCE 1/3
    let s = step(&mut l, r(LegKind::Sell, 7, 500, 70_000_000, N)).unwrap();
    let released = (160_000_000 + 2 * i128::from(N)) / 3;
    assert_eq!(
        s,
        Settled::Applied {
            tokens: 500,
            realized_delta: 70_000_000 - i128::from(N) - released
        }
    );
    // EXIT rest in two partial increments of one order (one landed leg fee)
    step(&mut l, r(LegKind::Sell, 8, 600, 50_000_000, N)).unwrap();
    step(&mut l, r(LegKind::Sell, 8, 1_000, 90_000_000, N)).unwrap();
    assert!(
        l.holdings.get(&M).is_none(),
        "fully closed, no stranded basis"
    );
    assert_eq!(l.committed, 0);
    // Closed: realized == total proceeds - total outlay.
    let outlay = 160_000_000 + 2 * i128::from(N);
    let proceeds = 70_000_000 + 90_000_000 - 2 * i128::from(N);
    assert_eq!(l.realized, proceeds - outlay);
    assert_eq!(l.cash, 1_000_000_000 + l.realized);
    assert_eq!(l.network_estimate, 4 * i128::from(N));
}

#[test]
fn duplicates_are_idempotent_backwards_and_overdraw_are_refused_unchanged() {
    let mut l = SettlementLedger::new(100_000_000);
    step(&mut l, r(LegKind::Entry, 1, 1_000, 50_000_000, N)).unwrap();
    let before = l.clone();
    assert_eq!(
        step(&mut l, r(LegKind::Entry, 1, 1_000, 50_000_000, N)),
        Ok(Settled::Duplicate)
    );
    assert_eq!(
        step(&mut l, r(LegKind::Entry, 1, 900, 50_000_000, N)),
        Err(SettleError::Backwards)
    );
    assert_eq!(
        step(&mut l, r(LegKind::Add, 2, 10, 60_000_000, N)),
        Err(SettleError::InsufficientCash)
    );
    assert_eq!(
        step(&mut l, r(LegKind::Sell, 3, 1_001, 1, N)),
        Err(SettleError::InsufficientInventory)
    );
    assert_eq!(l, before);
}

#[test]
fn losing_and_dust_sells_book_negative_realized_correctly() {
    let mut l = SettlementLedger::new(10_000_000);
    step(&mut l, r(LegKind::Entry, 1, 100, 1_000_000, N)).unwrap();
    // dust: venue-net below the landed-leg fee
    let s = step(&mut l, r(LegKind::Sell, 2, 100, 5_000, N)).unwrap();
    assert_eq!(
        s,
        Settled::Applied {
            tokens: 100,
            realized_delta: 5_000 - i128::from(N) - 1_000_000 - i128::from(N)
        }
    );
    assert_eq!(l.cash, 10_000_000 + l.realized);
}
