// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_no_copy_trade').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_strategy::safety_integrity::*;

fn full_pipe() -> DeterministicPipeline {
    DeterministicPipeline {
        feature_ok: true,
        liquidity_ok: true,
        risk_ok: true,
        economic_ok: true,
        sellability_ok: true,
        signing_ok: true,
    }
}
fn sig() -> ExternalSignal {
    ExternalSignal {
        kind: SignalKind::Wallet,
        token_mint: 77,
    }
}

#[test]
fn every_skipped_stage_blocks() {
    let cases = [
        (
            Stage::Feature,
            DeterministicPipeline {
                feature_ok: false,
                ..full_pipe()
            },
        ),
        (
            Stage::Liquidity,
            DeterministicPipeline {
                liquidity_ok: false,
                ..full_pipe()
            },
        ),
        (
            Stage::Risk,
            DeterministicPipeline {
                risk_ok: false,
                ..full_pipe()
            },
        ),
        (
            Stage::Economic,
            DeterministicPipeline {
                economic_ok: false,
                ..full_pipe()
            },
        ),
        (
            Stage::Sellability,
            DeterministicPipeline {
                sellability_ok: false,
                ..full_pipe()
            },
        ),
        (
            Stage::Signing,
            DeterministicPipeline {
                signing_ok: false,
                ..full_pipe()
            },
        ),
    ];
    for (stage, p) in cases {
        assert_eq!(to_order(sig(), &p), Err(Blocked { stage }));
    }
}
#[test]
fn full_pipeline_yields_order_no_mirror_source() {
    let o = to_order(sig(), &full_pipe()).expect("should produce order");
    assert_eq!(o.token_mint(), 77);
    // Order exposes only its token — no source-wallet accessor exists to mirror.
}
