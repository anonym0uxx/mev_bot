//! Leaf ca_reweight: CapitalAllocator reweight within envelope + promoted-policy guard (criterion 85).

use pump_quant_strategy::capital_allocator::{
    allocate_to_category, reweight, AllocError, CategoryReject, LaneGrant, LaneRequest,
};

fn lane(id: u32, req: u32, min: u32, max: u32, promoted: bool) -> LaneRequest {
    LaneRequest {
        lane_id: id,
        requested_bps: req,
        min_bps: min,
        max_bps: max,
        has_promoted_policy: promoted,
    }
}

#[test]
fn category_guard_refuses_unpromoted() {
    assert_eq!(
        allocate_to_category(false, 500, 0, 1_000),
        Err(CategoryReject::NoPromotedPolicy)
    );
    // Promoted: clamped into envelope.
    assert_eq!(allocate_to_category(true, 5_000, 100, 1_000), Ok(1_000));
    assert_eq!(allocate_to_category(true, 50, 100, 1_000), Ok(100));
    assert_eq!(allocate_to_category(true, 400, 100, 1_000), Ok(400));
}

#[test]
fn reweight_clamps_within_envelope() {
    let lanes = [
        lane(1, 5_000, 1_000, 3_000, true), // clamp down to 3_000
        lane(2, 100, 500, 4_000, true),     // clamp up to 500
        lane(3, 2_000, 0, 10_000, true),    // in-range
    ];
    let grants = reweight(&lanes).unwrap();
    assert_eq!(
        grants,
        vec![
            LaneGrant {
                lane_id: 1,
                granted_bps: 3_000
            },
            LaneGrant {
                lane_id: 2,
                granted_bps: 500
            },
            LaneGrant {
                lane_id: 3,
                granted_bps: 2_000
            },
        ]
    );
}

#[test]
fn unpromoted_lane_granted_zero() {
    let lanes = [
        lane(1, 4_000, 0, 10_000, false), // no promoted policy => 0
        lane(2, 4_000, 0, 10_000, true),
    ];
    let grants = reweight(&lanes).unwrap();
    assert_eq!(
        grants[0],
        LaneGrant {
            lane_id: 1,
            granted_bps: 0
        }
    );
    assert_eq!(
        grants[1],
        LaneGrant {
            lane_id: 2,
            granted_bps: 4_000
        }
    );
}

#[test]
fn invalid_envelope_aborts() {
    let lanes = [lane(9, 100, 5_000, 1_000, true)]; // min > max
    assert_eq!(
        reweight(&lanes),
        Err(AllocError::InvalidEnvelope { lane_id: 9 })
    );
}

#[test]
fn empty_input_yields_empty_output() {
    assert_eq!(reweight(&[]), Ok(vec![]));
}

#[test]
fn all_grants_within_bounds() {
    let lanes = [
        lane(1, 0, 200, 900, true),
        lane(2, 10_000, 200, 900, true),
        lane(3, 550, 200, 900, true),
    ];
    for g in reweight(&lanes).unwrap() {
        assert!(
            (200..=900).contains(&g.granted_bps),
            "grant {g:?} out of envelope"
        );
    }
}
