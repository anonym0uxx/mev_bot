//! Leaf tests: amplification-graph edge scoring (§29.8 D8).

use pump_quant_social::amplification::{
    build_amplification_graph, originator_echo_counts, originator_fraction_bps, score_edge,
    AmplificationEdge,
};
use pump_quant_social::copy_echo::CopyEchoEdge;

#[test]
fn score_edge_proximity() {
    assert_eq!(score_edge(10_000, 0, 1_000), 10_000); // instant, identical
    assert_eq!(score_edge(10_000, 1_000, 1_000), 0); // lag == max window
    assert_eq!(score_edge(10_000, 500, 1_000), 5_000); // half window
    assert_eq!(score_edge(6_000, 500, 1_000), 3_000); // 6000 * 5000/10000
    assert_eq!(score_edge(10_000, 10, 0), 0); // no window
}

fn ce(orig: u64, echo: u64, sim: i64, lag: u64) -> CopyEchoEdge {
    CopyEchoEdge {
        originator: orig,
        echo,
        token_id: 1,
        similarity_bps: sim,
        lag_ns: lag,
    }
}

#[test]
fn graph_aggregates_and_caps() {
    // Two identical A->B observations at zero lag → 10_000 each, capped at 10_000.
    let edges = [
        ce(1, 2, 10_000, 0),
        ce(1, 2, 10_000, 0),
        ce(2, 3, 10_000, 500),
    ];
    let g = build_amplification_graph(&edges, 1_000);
    assert_eq!(
        g,
        vec![
            AmplificationEdge {
                from_source: 1,
                to_source: 2,
                weight_bps: 10_000, // 10000 + 10000 capped
            },
            AmplificationEdge {
                from_source: 2,
                to_source: 3,
                weight_bps: 5_000, // half window
            },
        ]
    );
}

#[test]
fn originator_fraction_and_counts() {
    // A originates once (A->B), never echoes → fraction 10_000.
    let g = vec![
        AmplificationEdge {
            from_source: 1,
            to_source: 2,
            weight_bps: 8_000,
        },
        AmplificationEdge {
            from_source: 2,
            to_source: 3,
            weight_bps: 6_000,
        },
    ];
    assert_eq!(originator_echo_counts(1, &g), (1, 0));
    assert_eq!(originator_fraction_bps(1, &g), 10_000);

    // Source 2 originates once and echoes once → 5_000.
    assert_eq!(originator_echo_counts(2, &g), (1, 1));
    assert_eq!(originator_fraction_bps(2, &g), 5_000);

    // Source 3 only echoes → 0 (pure reach, not alpha).
    assert_eq!(originator_echo_counts(3, &g), (0, 1));
    assert_eq!(originator_fraction_bps(3, &g), 0);

    // Unknown source → 0 (no evidence).
    assert_eq!(originator_fraction_bps(99, &g), 0);
}

#[test]
fn graph_is_deterministically_ordered() {
    // Feeding edges out of order yields the same sorted graph.
    let a = [ce(3, 1, 9_000, 0), ce(1, 2, 9_000, 0)];
    let b = [ce(1, 2, 9_000, 0), ce(3, 1, 9_000, 0)];
    assert_eq!(
        build_amplification_graph(&a, 1_000),
        build_amplification_graph(&b, 1_000)
    );
}
