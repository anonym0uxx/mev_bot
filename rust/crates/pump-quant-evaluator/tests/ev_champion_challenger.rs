use pump_quant_evaluator::champion_challenger::*;
use pump_quant_evaluator::evaluator_stats::{Lane, NetSol, ReconTrade};

// Build a NetSol via the real aggregation so the test exercises the wiring.
fn net(lane: Lane, gross: i128) -> NetSol {
    pump_quant_evaluator::evaluator_stats::net_sol(&[ReconTrade::test(lane, gross, 0, 0, 0)], lane)
}

#[test]
fn defeats_when_margin_cleared() {
    let champ = net(Lane::Scalp, 1_000);
    let chall = net(Lane::Scalp, 1_600);
    // margin = 1600 - 1000 = 600, required 500 -> Defeats.
    let v = challenger_defeats_champion(&champ, &chall, 500);
    assert_eq!(v, ChampionVerdict::Defeats);
    assert!(v.defeats());
}

#[test]
fn fails_when_margin_short() {
    let champ = net(Lane::Scalp, 1_000);
    let chall = net(Lane::Scalp, 1_400);
    // margin = 400, required 500 -> Fails carrying the exact shortfall.
    let v = challenger_defeats_champion(&champ, &chall, 500);
    assert_eq!(
        v,
        ChampionVerdict::Fails {
            margin_lamports: 400,
            required_lamports: 500,
        }
    );
    assert!(!v.defeats());
}

#[test]
fn missing_challenger_has_no_evidence() {
    let champ = net(Lane::Scalp, 1_000);
    let chall = NetSol::missing();
    assert_eq!(
        challenger_defeats_champion(&champ, &chall, 0),
        ChampionVerdict::NoEvidence
    );
}

#[test]
fn missing_champion_treated_as_break_even() {
    let champ = NetSol::missing();
    let chall = net(Lane::Scalp, 300);
    // champ counts as net 0, margin = 300, required 300 -> Defeats (>=).
    assert_eq!(
        challenger_defeats_champion(&champ, &chall, 300),
        ChampionVerdict::Defeats
    );
    // required 301 -> Fails margin 300.
    assert_eq!(
        challenger_defeats_champion(&champ, &chall, 301),
        ChampionVerdict::Fails {
            margin_lamports: 300,
            required_lamports: 301,
        }
    );
}

#[test]
fn negative_challenger_fails_against_positive_champion() {
    let champ = net(Lane::Scalp, 500);
    let chall = net(Lane::Scalp, -200);
    // margin = -700, required 0 -> Fails.
    assert_eq!(
        challenger_defeats_champion(&champ, &chall, 0),
        ChampionVerdict::Fails {
            margin_lamports: -700,
            required_lamports: 0,
        }
    );
}
