//! REGRESSION CLASS 1 — determinism / replay.
//!
//! Drives the golden tape N times and asserts a byte-identical decision-journal
//! digest every run, equal to the pinned baseline, plus every pinned outcome
//! count. Also asserts permutation-invariance on a small causally-independent
//! scenario ("shuffled-but-causal ingest ordering, where legal"): reordering
//! whole per-mint blocks that share no window must not change the digest.
//!
//! All integer, no wall-clock, no RNG (§22). The golden replay is the slowest
//! part; N = 3 is enough to catch a non-deterministic drift.

use pq_regression::baselines::*;
use pq_regression::golden_tape::{drive, mint};
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::AppEvent;

/// N replays of the golden tape (kept small — the tape is the slow part).
const N_REPLAYS: usize = 3;

#[test]
fn golden_tape_digest_is_byte_stable_across_replays() {
    let first = drive(Config::dev_portable());
    for i in 1..N_REPLAYS {
        let again = drive(Config::dev_portable());
        assert_eq!(
            first, again,
            "golden replay #{i} diverged from replay #0 — non-determinism (§22)"
        );
    }
    // The whole Report is frozen, field by field, against the manifest.
    assert_eq!(
        first.journal_digest, GOLDEN_DIGEST,
        "decision-journal digest drifted from the pinned baseline"
    );
    assert_eq!(first.net_lamports, GOLDEN_NET_LAMPORTS, "net-SOL drifted");
    assert_eq!(first.promoted, GOLDEN_PROMOTED, "promoted drifted");
    assert_eq!(first.admitted, GOLDEN_ADMITTED, "admitted drifted");
    assert_eq!(first.rejected, GOLDEN_REJECTED, "rejected drifted");
    assert_eq!(
        first.universe_filtered, GOLDEN_UNIVERSE_FILTERED,
        "§21.5 universe-screen activity drifted"
    );
}

// ---------------------------------------------------------------------------
// Shuffled-but-causal ingest ordering (where legal).
//
// The journal digest folds decisions in the ORDER they occur, so an arbitrary
// shuffle is not expected to preserve it — decisions emit during `evaluate`
// (`AppEvent::Tick`), and interleaving evaluates between blocks would journal
// each block's decisions in block order. The genuinely LEGAL reordering is of
// ingest events that all land BEFORE a single shared evaluate burst: a
// MarketTrade / OnchainConfirm only *updates per-mint state*, keyed by mint, so
// for DISTINCT mints their relative arrival order cannot change the per-mint
// state seen at the trailing evaluate — and the decisions then emit in the
// engine's own deterministic ranking order, not ingest order. We assert the full
// Report (digest included) is invariant under such reorderings.
// ---------------------------------------------------------------------------

/// One self-contained per-mint block: a short net-buy pump then an on-chain
/// confirm. Returned so blocks can be reordered wholesale while each block's
/// internal event order is preserved (the causal constraint).
fn causal_block(tag: u64) -> Vec<AppEvent> {
    let mt = mint(tag);
    let mut evs = Vec::new();
    for i in 0..10u64 {
        evs.push(AppEvent::MarketTrade {
            mint: mt,
            price_fp: (100 + i as i128) * 10_000_000,
            quote_lamports: 800_000,
            liquidity_lamports: 400_000_000,
            signed_base: 900_000 - i as i64,
            buyer_entity: 40 + (tag + i) % 7,
            age_slots: 12,
        });
    }
    evs.push(AppEvent::OnchainConfirm {
        mint: mt,
        sellable_depth_lamports: 500_000_000,
    });
    evs
}

/// Feed every block's state-updating events FIRST (no evaluate in between — so
/// ingest order among distinct mints is causally free), then run a single
/// trailing evaluate burst that emits all decisions in the engine's own order.
fn run_blocks(order: &[u64]) -> Report {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    for &tag in order {
        for ev in causal_block(tag) {
            eng.tick(ev);
        }
    }
    // Single shared evaluate burst: decisions emit in ranking order, not ingest.
    for _ in 0..24 {
        eng.tick(AppEvent::Tick);
    }
    eng.report()
}

#[test]
fn causally_independent_ingest_reordering_is_report_invariant() {
    // Distinct, well-separated mint tags — no shared narrative/attention window,
    // and far below every bounded-table cap so no order-dependent eviction fires.
    let base: [u64; 5] = [5_100, 5_200, 5_300, 5_400, 5_500];
    let reference = run_blocks(&base);

    // Deterministic permutations of the ingest order (no RNG). Each keeps every
    // block's internal event order intact — only whole blocks move, and all
    // ingest precedes the single shared evaluate burst.
    let permutations: [[u64; 5]; 3] = [
        [5_500, 5_400, 5_300, 5_200, 5_100], // reversed
        [5_300, 5_100, 5_500, 5_200, 5_400], // interleaved
        [5_200, 5_300, 5_400, 5_500, 5_100], // single rotation
    ];
    for (i, perm) in permutations.iter().enumerate() {
        let permuted = run_blocks(perm);
        assert_eq!(
            reference, permuted,
            "permutation #{i} of causally-independent ingest changed the Report \
             — a legal reordering must be decision-invariant (digest included)"
        );
    }
    // Guard against a vacuous pass: the scenario must actually produce decisions.
    assert!(
        reference.admitted > 0,
        "the reordering scenario must exercise real admissions to be meaningful"
    );
}
