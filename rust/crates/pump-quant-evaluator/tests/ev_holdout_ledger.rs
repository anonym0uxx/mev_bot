use pump_quant_evaluator::evaluator_pin::fnv1a_64;
use pump_quant_evaluator::holdout_ledger::*;

#[test]
fn hash_is_order_and_duplicate_invariant() {
    // Set identity, not listing: permutations and duplicates collide.
    let a = holdout_hash(&[3, 1, 2]);
    let b = holdout_hash(&[1, 2, 3]);
    let c = holdout_hash(&[1, 2, 3, 3, 1]);
    assert_eq!(a, b);
    assert_eq!(b, c);
    // Different membership -> different key (independently: FNV over sorted LE
    // bytes of {1,2} differs from {1,2,3}).
    assert_ne!(holdout_hash(&[1, 2]), holdout_hash(&[1, 2, 3]));
}

#[test]
fn hash_matches_independent_fnv_over_sorted_le_bytes() {
    // Reconstruct the key from first principles for {5, 7}.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&5u64.to_le_bytes());
    bytes.extend_from_slice(&7u64.to_le_bytes());
    assert_eq!(holdout_hash(&[7, 5, 5]), HoldoutHash(fnv1a_64(&bytes)));
}

#[test]
fn budget_one_grants_then_flags_reuse() {
    let mut led = HoldoutLedger::new();
    let h = holdout_hash(&[42, 43]);
    led.register(h, 1);

    // First access: within budget, not a reuse, zero remaining.
    assert_eq!(
        led.record_access(h),
        AccessOutcome::Granted {
            access_no: 1,
            remaining: 0,
            reused: false,
        }
    );
    // Second access: over budget -> silent re-tuning surfaced.
    assert_eq!(
        led.record_access(h),
        AccessOutcome::BudgetExceeded {
            access_no: 2,
            budget: 1,
        }
    );
    assert_eq!(
        led.record(h),
        Some(AccessRecord {
            count: 2,
            budget: 1
        })
    );
}

#[test]
fn budget_two_allows_one_reuse() {
    let mut led = HoldoutLedger::new();
    let h = holdout_hash(&[1]);
    led.register(h, 2);
    assert_eq!(
        led.record_access(h),
        AccessOutcome::Granted {
            access_no: 1,
            remaining: 1,
            reused: false,
        }
    );
    // Second access still within budget but flagged as a reuse.
    assert_eq!(
        led.record_access(h),
        AccessOutcome::Granted {
            access_no: 2,
            remaining: 0,
            reused: true,
        }
    );
    // Third exceeds.
    assert_eq!(
        led.record_access(h),
        AccessOutcome::BudgetExceeded {
            access_no: 3,
            budget: 2,
        }
    );
}

#[test]
fn unregistered_access_is_flagged_and_not_counted() {
    let mut led = HoldoutLedger::new();
    let h = holdout_hash(&[99]);
    assert_eq!(led.record_access(h), AccessOutcome::Unregistered);
    assert_eq!(led.record(h), None);
}

#[test]
fn distinct_holdouts_have_independent_budgets() {
    let mut led = HoldoutLedger::new();
    let h1 = holdout_hash(&[1, 2]);
    let h2 = holdout_hash(&[3, 4]);
    led.register(h1, 1);
    led.register(h2, 1);
    assert!(matches!(
        led.record_access(h1),
        AccessOutcome::Granted { .. }
    ));
    // h2 untouched -> its first access still granted.
    assert!(matches!(
        led.record_access(h2),
        AccessOutcome::Granted { .. }
    ));
    // h1 second access exceeds independently.
    assert!(matches!(
        led.record_access(h1),
        AccessOutcome::BudgetExceeded { .. }
    ));
}
