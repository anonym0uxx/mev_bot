// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'attention_decay' component (leaf 'decay_after_peak_bps_is_bounded_fraction').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_narrative::attention_decay::*;

fn base_inputs() -> DecayInputs {
    DecayInputs {
        semantic_duplication_bps: 1_500,
        source_diversity: 12,
        raid_activity: 2,
        narrative_saturation_bps: 3_000,
        conversion_to_new_wallets: 40,
        conversion_to_independent_breadth: 9,
        conversion_to_net_flow: 25_000,
        peak_level: 1_000,
        current_level: 600,
    }
}

#[test]
fn decay_after_peak_bps_is_bounded_fraction() {
    // Concrete: (1000-600)/1000 = 0.4 -> 4_000 bps.
    let mut inp = base_inputs();
    inp.peak_level = 1_000;
    inp.current_level = 600;
    let m = nv_attention_decay(&[], 10_000, 1_000, &inp);
    assert_eq!(m.decay_after_peak_bps, 4_000);

    // At peak -> 0 (rejection of "any drop" claim).
    inp.current_level = 1_000;
    assert_eq!(
        nv_attention_decay(&[], 10_000, 1_000, &inp).decay_after_peak_bps,
        0
    );

    // Above peak (fresh high) -> 0.
    inp.current_level = 1_200;
    assert_eq!(
        nv_attention_decay(&[], 10_000, 1_000, &inp).decay_after_peak_bps,
        0
    );

    // Unknown peak (peak==0) -> 0, no divide-by-zero.
    inp.peak_level = 0;
    inp.current_level = 500;
    assert_eq!(
        nv_attention_decay(&[], 10_000, 1_000, &inp).decay_after_peak_bps,
        0
    );

    // Sweep: for peak>current>0 result equals (peak-current)*10_000/peak and never exceeds 10_000.
    for peak in [1u64, 7, 100, 999, 1_000, 65_536, 1_000_000] {
        for current in 0..peak {
            let mut s = base_inputs();
            s.peak_level = peak;
            s.current_level = current;
            let got = nv_attention_decay(&[], 10_000, 1_000, &s).decay_after_peak_bps;
            let want = ((peak - current) as u128 * 10_000u128 / peak as u128) as u64;
            assert_eq!(got, want, "peak={peak} current={current}");
            assert!(got <= 10_000, "decay bps must stay <= FP_ONE");
        }
    }
    // current==0 with peak>0 -> full decay 10_000 bps.
    let mut full = base_inputs();
    full.peak_level = 1_000;
    full.current_level = 0;
    assert_eq!(
        nv_attention_decay(&[], 10_000, 1_000, &full).decay_after_peak_bps,
        10_000
    );
}
