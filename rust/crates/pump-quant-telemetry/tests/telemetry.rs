//! Integration tests for `pump-quant-telemetry`.
//!
//! These pin the two properties the operator's standing rules make non-negotiable:
//! every number is an exact integer (no approximation anywhere a threshold is compared),
//! and the alert surface stays SILENT unless a real fault exists. A pager that fires on a
//! quiet system is a false alarm and is forbidden.

use pump_quant_telemetry::{
    alerts, AccountState, Alert, BandHealth, TelemetrySnapshot, BPS_DENOMINATOR,
    CRITICAL_DRAWDOWN_BPS, DEGRADED_DRAWDOWN_BPS,
};

/// Build an account state whose equity is `equity` lamports, all of it realized.
fn book(
    equity: i64,
    open_positions: u32,
    peak: u64,
    floor: u64,
) -> AccountState {
    AccountState {
        bankroll_realized_lamports: equity,
        bankroll_committed_lamports: 0,
        open_positions,
        peak_equity_lamports: peak,
        floor_lamports: floor,
    }
}

/// (1) An idle, healthy book pages nobody — including a completely fresh one.
#[test]
fn idle_healthy_book_produces_no_alerts() {
    // Fresh book: nothing realized, nothing committed, no peak known.
    let fresh = book(0, 0, 0, 0);
    let s = TelemetrySnapshot::observe(None, &fresh);
    assert_eq!(s.equity_lamports, 0);
    assert_eq!(s.drawdown_bps, 0);
    assert!(!s.floor_breached);
    assert_eq!(s.band_health, BandHealth::Healthy);
    assert!(s.is_idle());
    assert!(
        alerts(&s).is_empty(),
        "a fresh idle book must page nobody: {:?}",
        alerts(&s)
    );

    // Funded but idle: capital parked, nothing deployed, sitting at its own peak.
    let idle = book(5_000_000_000, 0, 5_000_000_000, 1_000_000_000);
    let s = TelemetrySnapshot::observe(None, &idle);
    assert!(s.is_idle());
    assert_eq!(s.band_health, BandHealth::Healthy);
    assert_eq!(s.pnl_delta_lamports, 0, "no predecessor => no delta claimed");
    assert!(
        alerts(&s).is_empty(),
        "an idle book with no drawdown must be silent: {:?}",
        alerts(&s)
    );
}

/// (2) A floor breach pages exactly once, and leads the vector when other faults exist.
#[test]
fn floor_breach_pages_exactly_once_and_is_ordered_first() {
    // Equity 9_500 under a 9_700 floor: a real breach whose drawdown (500 bps) is
    // still inside the healthy band, so the floor is the ONLY fault present.
    let breach = book(9_500, 2, 10_000, 9_700);
    let s = TelemetrySnapshot::observe(None, &breach);
    assert_eq!(s.drawdown_bps, 500);
    assert_eq!(s.band_health, BandHealth::Healthy);
    assert!(s.floor_breached);
    let only = alerts(&s);
    assert_eq!(only.len(), 1, "exactly one alert: {:?}", only);
    assert_eq!(
        only[0],
        Alert::DrawdownFloorBreached {
            equity_lamports: 9_500,
            floor_lamports: 9_700,
        }
    );

    // When the band ALSO faults, the floor breach is still element 0.
    let deep = book(7_000, 3, 10_000, 7_500);
    let s = TelemetrySnapshot::observe(None, &deep);
    assert_eq!(s.drawdown_bps, 3_000);
    assert_eq!(s.band_health, BandHealth::Critical);
    let both = alerts(&s);
    assert_eq!(both.len(), 2, "two independent faults: {:?}", both);
    assert_eq!(
        both[0],
        Alert::DrawdownFloorBreached {
            equity_lamports: 7_000,
            floor_lamports: 7_500,
        },
        "the floor breach is the operator's first-read item"
    );
    assert_eq!(both[1], Alert::DrawdownCritical { drawdown_bps: 3_000 });

    // Equity exactly AT the floor is not a breach — the rule is strictly below.
    let at_floor = book(9_700, 2, 10_000, 9_700);
    let s = TelemetrySnapshot::observe(None, &at_floor);
    assert_eq!(s.equity_lamports, 9_700);
    assert!(!s.floor_breached);
    assert_eq!(s.drawdown_bps, 300);
    assert!(alerts(&s).is_empty(), "no breach, healthy band, no page");
}

/// (3) Drawdown is exact at both band boundaries produced by the named constants.
#[test]
fn drawdown_is_exact_at_the_named_constant_boundaries() {
    assert!(
        DEGRADED_DRAWDOWN_BPS < CRITICAL_DRAWDOWN_BPS,
        "bands must ascend"
    );
    let peak = 10_000_000u64;

    // Exactly the degraded boundary, inclusive: (peak-equity)*10_000/peak == DEGRADED.
    let equity = peak as i64 - (peak as i64 * DEGRADED_DRAWDOWN_BPS as i64) / BPS_DENOMINATOR as i64;
    let s = TelemetrySnapshot::observe(None, &book(equity, 1, peak, 0));
    assert_eq!(s.equity_lamports, 8_500_000);
    assert_eq!(s.drawdown_bps, DEGRADED_DRAWDOWN_BPS);
    assert_eq!(s.band_health, BandHealth::Degraded);
    assert_eq!(
        alerts(&s),
        vec![Alert::BandDegraded {
            band_health: BandHealth::Degraded
        }]
    );

    // One basis point shallower: integer division truncates back to Healthy.
    let s = TelemetrySnapshot::observe(None, &book(8_500_100, 1, peak, 0));
    assert_eq!(s.drawdown_bps, DEGRADED_DRAWDOWN_BPS - 1);
    assert_eq!(s.band_health, BandHealth::Healthy);
    assert!(alerts(&s).is_empty(), "just inside the band is silent");

    // Exactly the critical boundary, inclusive.
    let s = TelemetrySnapshot::observe(None, &book(7_000_000, 1, peak, 0));
    assert_eq!(s.drawdown_bps, CRITICAL_DRAWDOWN_BPS);
    assert_eq!(s.band_health, BandHealth::Critical);
    assert_eq!(
        alerts(&s),
        vec![Alert::DrawdownCritical {
            drawdown_bps: CRITICAL_DRAWDOWN_BPS
        }]
    );

    // One basis point shallower than critical: Degraded, reported but not survival-paged.
    let s = TelemetrySnapshot::observe(None, &book(7_000_100, 1, peak, 0));
    assert_eq!(s.drawdown_bps, CRITICAL_DRAWDOWN_BPS - 1);
    assert_eq!(s.band_health, BandHealth::Degraded);

    // At the peak, and above it, drawdown is exactly zero.
    let s = TelemetrySnapshot::observe(None, &book(peak as i64, 1, peak, 0));
    assert_eq!(s.drawdown_bps, 0);
    let s = TelemetrySnapshot::observe(None, &book(peak as i64 + 1, 1, peak, 0));
    assert_eq!(s.drawdown_bps, 0, "a new high is not a drawdown");
}

/// (4) An unknown peak (zero) yields drawdown 0 instead of a panic or a bogus page.
#[test]
fn zero_peak_yields_zero_drawdown() {
    let s = TelemetrySnapshot::observe(None, &book(123_456, 0, 0, 0));
    assert_eq!(s.drawdown_bps, 0);
    assert_eq!(s.band_health, BandHealth::Healthy);
    assert_eq!(s.equity_lamports, 123_456);
    assert!(alerts(&s).is_empty());

    // A zero peak with the floor unreachable is still silent, and a zero peak on a
    // breached floor still pages — exactly once, on the floor alone.
    let s = TelemetrySnapshot::observe(None, &book(0, 0, 0, 500));
    assert_eq!(s.drawdown_bps, 0);
    assert!(s.floor_breached);
    assert_eq!(
        alerts(&s),
        vec![Alert::DrawdownFloorBreached {
            equity_lamports: 0,
            floor_lamports: 500,
        }]
    );
}

/// (5) A deeply negative book floors at 0 equity rather than underflowing into a gain.
#[test]
fn large_negative_equity_floors_at_zero() {
    let state = AccountState {
        bankroll_realized_lamports: i64::MIN,
        bankroll_committed_lamports: -1,
        open_positions: 0,
        peak_equity_lamports: u64::MAX,
        floor_lamports: 1,
    };
    let s = TelemetrySnapshot::observe(None, &state);
    assert_eq!(s.equity_lamports, 0, "negative equity is not a positive book");
    assert_eq!(s.realized_lamports, i64::MIN);
    // The whole peak is drawn down, saturating at exactly BPS_DENOMINATOR.
    assert_eq!(s.drawdown_bps, BPS_DENOMINATOR);
    assert_eq!(s.band_health, BandHealth::Critical);
    assert!(s.floor_breached);
    assert_eq!(
        alerts(&s),
        vec![
            Alert::DrawdownFloorBreached {
                equity_lamports: 0,
                floor_lamports: 1,
            },
            Alert::DrawdownCritical {
                drawdown_bps: BPS_DENOMINATOR,
            },
        ]
    );

    // Both buckets at the extreme negative end: the saturating add itself must not wrap.
    let state = AccountState {
        bankroll_realized_lamports: i64::MIN,
        bankroll_committed_lamports: i64::MIN,
        open_positions: 0,
        peak_equity_lamports: 0,
        floor_lamports: 0,
    };
    let s = TelemetrySnapshot::observe(None, &state);
    assert_eq!(s.equity_lamports, 0);
    assert_eq!(s.drawdown_bps, 0);
    assert!(!s.floor_breached);
    assert!(alerts(&s).is_empty());
}

/// PnL delta is the signed equity change between observations, and 0 for the first one.
#[test]
fn pnl_delta_is_the_signed_equity_change() {
    let prev = TelemetrySnapshot::observe(None, &book(1_000, 1, 1_000, 0));
    assert_eq!(prev.pnl_delta_lamports, 0);

    let down = TelemetrySnapshot::observe(Some(&prev), &book(600, 1, 1_000, 0));
    assert_eq!(down.pnl_delta_lamports, -400);
    assert_eq!(down.drawdown_bps, 4_000, "(1_000-600)*10_000/1_000");
    assert_eq!(down.band_health, BandHealth::Critical);

    let up = TelemetrySnapshot::observe(Some(&down), &book(1_600, 1, 1_600, 0));
    assert_eq!(up.pnl_delta_lamports, 1_000);
    assert_eq!(up.drawdown_bps, 0);
}

/// The crate holds ONE integer definition per number: no floating-point types at all.
///
/// This is a tripwire on the source itself. A drawdown reported two ways eventually
/// reports two different values, and the version the pager reads must be the version the
/// engine computed.
#[test]
fn crate_source_is_integer_only() {
    let lib = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
        .expect("src/lib.rs must be readable");
    for banned in ["f64", "f32"] {
        assert!(
            !lib.contains(banned),
            "src/lib.rs mentions `{banned}`: this crate is integer-only"
        );
    }
}
