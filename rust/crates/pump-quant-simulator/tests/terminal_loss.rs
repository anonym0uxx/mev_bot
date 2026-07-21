//! Leaf tests for `terminal_loss`: predeclared terminal-loss accounting (§38).

use pump_quant_simulator::terminal_loss::TerminalLossPolicy;

const BASIS: u64 = 89_100_000;

#[test]
fn write_to_zero_is_zero() {
    assert_eq!(TerminalLossPolicy::WriteToZero.terminal_value(BASIS), 0);
    assert_eq!(TerminalLossPolicy::WriteToZero.terminal_value(0), 0);
}

#[test]
fn residual_bps_is_fraction_of_basis() {
    // 20% residual: 89_100_000 * 2000/10000 = 17_820_000.
    assert_eq!(
        TerminalLossPolicy::ResidualBps(2_000).terminal_value(BASIS),
        17_820_000
    );
    // 100% residual returns the whole basis.
    assert_eq!(
        TerminalLossPolicy::ResidualBps(10_000).terminal_value(BASIS),
        BASIS
    );
    // Over-100% residual is clamped to the basis (never above).
    assert_eq!(
        TerminalLossPolicy::ResidualBps(20_000).terminal_value(BASIS),
        BASIS
    );
}

#[test]
fn fixed_residual_is_capped_at_basis() {
    // Fixed below basis returns the fixed amount.
    assert_eq!(
        TerminalLossPolicy::FixedResidualLamports(10_000_000).terminal_value(BASIS),
        10_000_000
    );
    // Fixed above basis is capped at basis (never valued above committed capital).
    assert_eq!(
        TerminalLossPolicy::FixedResidualLamports(200_000_000).terminal_value(BASIS),
        BASIS
    );
}

#[test]
fn invariant_value_never_exceeds_basis() {
    // Multiple bases and policies: recovered value is always in [0, basis].
    for basis in [0u64, 1, 1_000, 89_100_000, u64::MAX / 2] {
        for policy in [
            TerminalLossPolicy::WriteToZero,
            TerminalLossPolicy::ResidualBps(0),
            TerminalLossPolicy::ResidualBps(5_000),
            TerminalLossPolicy::ResidualBps(9_999),
            TerminalLossPolicy::FixedResidualLamports(1),
            TerminalLossPolicy::FixedResidualLamports(u64::MAX),
        ] {
            let v = policy.terminal_value(basis);
            assert!(v <= basis, "policy {policy:?} basis {basis} -> {v}");
        }
    }
}
