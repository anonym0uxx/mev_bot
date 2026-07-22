//! Leaf tests for `calibration`: versioned bounded store + deterministic model
//! application over recorded fills (§38/§39).

use pump_quant_simulator::calibration::{
    CalStoreError, CalibrationKey, CalibrationParams, CalibrationStore, RecordedFill,
};
use pump_quant_simulator::fill::{CostModel, ExitImpairment, FillMode};
use pump_quant_simulator::terminal_loss::TerminalLossPolicy;

fn params(mode: FillMode) -> CalibrationParams {
    CalibrationParams {
        costs: CostModel {
            entry_fee_bps: 100,
            exit_fee_bps: 100,
            entry_tip_lamports: 50_000,
            exit_tip_lamports: 50_000,
        },
        imp: ExitImpairment {
            first_sell_penalty_bps: 200,
            retry_slippage_bps: 300,
            fee_escalation_bps: 50,
            retry_tip_lamports: 20_000,
            unexitable: false,
        },
        impact_k_bps: 10_000,
        terminal: TerminalLossPolicy::WriteToZero,
        mode,
    }
}

const KEY_A: CalibrationKey = CalibrationKey {
    route_id: 1,
    tip_band: 0,
};
const KEY_B: CalibrationKey = CalibrationKey {
    route_id: 2,
    tip_band: 0,
};
const KEY_C: CalibrationKey = CalibrationKey {
    route_id: 3,
    tip_band: 0,
};

#[test]
fn versions_are_bounded_fifo() {
    let mut store = CalibrationStore::new(4, 2);
    store.insert(KEY_A, params(FillMode::SignalReplay)).unwrap();
    store
        .insert(KEY_A, params(FillMode::OptimisticCeiling))
        .unwrap();
    // Third version evicts the oldest; only 2 retained.
    store
        .insert(
            KEY_A,
            params(FillMode::Adversarial(
                pump_quant_simulator::fill::ImpairmentLevel::Realistic,
            )),
        )
        .unwrap();
    assert_eq!(store.version_count(&KEY_A), 2);
    // Latest is the most recently inserted.
    assert_eq!(
        store.latest(&KEY_A).unwrap().mode,
        FillMode::Adversarial(pump_quant_simulator::fill::ImpairmentLevel::Realistic)
    );
}

#[test]
fn key_capacity_is_enforced_explicitly() {
    let mut store = CalibrationStore::new(2, 2);
    store
        .insert(KEY_A, params(FillMode::OptimisticCeiling))
        .unwrap();
    store
        .insert(KEY_B, params(FillMode::OptimisticCeiling))
        .unwrap();
    // A third distinct key exceeds capacity -> explicit error, no silent overwrite.
    assert_eq!(
        store.insert(KEY_C, params(FillMode::OptimisticCeiling)),
        Err(CalStoreError::KeyCapacityExceeded)
    );
    // Existing keys still accept new versions.
    assert!(store.insert(KEY_A, params(FillMode::SignalReplay)).is_ok());
    assert_eq!(store.key_count(), 2);
}

#[test]
fn apply_recorded_matches_direct_fill_math() {
    let mut store = CalibrationStore::new(2, 2);
    store
        .insert(KEY_A, params(FillMode::OptimisticCeiling))
        .unwrap();
    let recorded = RecordedFill {
        key: KEY_A,
        notional_lamports: 100_000_000,
        move_bps: 5_000,
        depth_lamports: 1_000_000_000,
    };
    let out = store
        .apply_recorded(&recorded)
        .expect("calibration present");
    // Same Mode-B math as the fill leaf: net = 14_536_417.
    assert_eq!(out.net_pnl_lamports, 14_536_417);
    assert_eq!(out.exit_proceeds_lamports, 114_586_417);
}

#[test]
fn missing_calibration_returns_none_and_batch_preserves_order() {
    let mut store = CalibrationStore::new(2, 2);
    store
        .insert(KEY_A, params(FillMode::OptimisticCeiling))
        .unwrap();
    let fills = [
        RecordedFill {
            key: KEY_A,
            notional_lamports: 100_000_000,
            move_bps: 5_000,
            depth_lamports: 1_000_000_000,
        },
        RecordedFill {
            key: KEY_B, // no calibration for B
            notional_lamports: 50_000_000,
            move_bps: 1_000,
            depth_lamports: 1_000_000_000,
        },
    ];
    let out = store.apply_all(&fills);
    assert_eq!(out.len(), 2);
    assert!(out[0].is_some());
    assert!(out[1].is_none(), "missing calibration surfaced as None");
}

#[test]
fn application_is_deterministic() {
    let mut store = CalibrationStore::new(2, 2);
    store
        .insert(
            KEY_A,
            params(FillMode::Adversarial(
                pump_quant_simulator::fill::ImpairmentLevel::Realistic,
            )),
        )
        .unwrap();
    let recorded = RecordedFill {
        key: KEY_A,
        notional_lamports: 100_000_000,
        move_bps: 5_000,
        depth_lamports: 1_000_000_000,
    };
    let a = store.apply_recorded(&recorded).unwrap();
    let b = store.apply_recorded(&recorded).unwrap();
    assert_eq!(a, b, "identical inputs must yield identical outputs");
}
