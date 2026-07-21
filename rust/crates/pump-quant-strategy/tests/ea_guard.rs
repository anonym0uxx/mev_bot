//! Leaf ea_guard: emergency-fix risk-reduction guard (criterion 58).

use pump_quant_strategy::emergency_action::{
    evaluate_emergency, EmergencyVerdict, RiskIncrease, RiskParams,
};

fn current() -> RiskParams {
    RiskParams {
        max_size_lamports: 1_000,
        exposure_limit_lamports: 5_000,
        slippage_tolerance_bps: 300,
        entries_enabled: true,
        route_authority: 2,
    }
}

#[test]
fn non_increasing_action_accepted_and_quarantined() {
    // Everything tightened or held: smaller size/exposure/slippage, entries off,
    // fewer routes.
    let proposed = RiskParams {
        max_size_lamports: 500,
        exposure_limit_lamports: 5_000, // equal allowed
        slippage_tolerance_bps: 200,
        entries_enabled: false,
        route_authority: 1,
    };
    assert_eq!(
        evaluate_emergency(&current(), &proposed),
        EmergencyVerdict::Accepted { quarantined: true }
    );
}

#[test]
fn identical_params_accepted() {
    assert_eq!(
        evaluate_emergency(&current(), &current()),
        EmergencyVerdict::Accepted { quarantined: true }
    );
}

#[test]
fn size_increase_rejected() {
    let mut p = current();
    p.max_size_lamports = 1_001;
    assert_eq!(
        evaluate_emergency(&current(), &p),
        EmergencyVerdict::Rejected {
            reason: RiskIncrease::SizeIncreased
        }
    );
}

#[test]
fn exposure_increase_rejected() {
    let mut p = current();
    p.exposure_limit_lamports = 6_000;
    assert_eq!(
        evaluate_emergency(&current(), &p),
        EmergencyVerdict::Rejected {
            reason: RiskIncrease::ExposureIncreased
        }
    );
}

#[test]
fn slippage_loosen_rejected() {
    let mut p = current();
    p.slippage_tolerance_bps = 400;
    assert_eq!(
        evaluate_emergency(&current(), &p),
        EmergencyVerdict::Rejected {
            reason: RiskIncrease::SlippageLoosened
        }
    );
}

#[test]
fn enabling_entries_rejected() {
    let mut base = current();
    base.entries_enabled = false;
    let mut p = base;
    p.entries_enabled = true;
    assert_eq!(
        evaluate_emergency(&base, &p),
        EmergencyVerdict::Rejected {
            reason: RiskIncrease::EntriesEnabled
        }
    );
}

#[test]
fn disabling_entries_allowed() {
    let mut p = current();
    p.entries_enabled = false;
    assert_eq!(
        evaluate_emergency(&current(), &p),
        EmergencyVerdict::Accepted { quarantined: true }
    );
}

#[test]
fn route_authority_expansion_rejected() {
    let mut p = current();
    p.route_authority = 3;
    assert_eq!(
        evaluate_emergency(&current(), &p),
        EmergencyVerdict::Rejected {
            reason: RiskIncrease::RouteAuthorityExpanded
        }
    );
}

#[test]
fn size_checked_before_exposure() {
    // Both increase; size is reported first (fixed order).
    let mut p = current();
    p.max_size_lamports = 2_000;
    p.exposure_limit_lamports = 9_000;
    assert_eq!(
        evaluate_emergency(&current(), &p),
        EmergencyVerdict::Rejected {
            reason: RiskIncrease::SizeIncreased
        }
    );
}
