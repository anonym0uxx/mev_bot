//! Cross-channel copy-echo detection (constitution §29.8 D7/D8).
//!
//! # Responsibility
//! Given the calls that landed on one token across many channels, find near-duplicate
//! content posted within a time window and orient each match earlier→later so the
//! originator (earliest poster of the content) is separated from the echoes. Semantic
//! similarity is a deterministic integer Jaccard over content shingles (hashed token
//! n-grams supplied by the caller); no float, no clock (§22). Feeds D7 copy-echo
//! density and the D8 amplification graph.

use crate::fixedpoint::ratio_bps;

/// One call on a token, with its content reduced to sorted-unique shingle hashes.
///
/// `shingles` need not be pre-sorted; [`jaccard_bps`] and [`detect_copy_echo`]
/// normalise defensively. `timestamp_ns` is the capture timestamp (already measured;
/// the module never reads a clock).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelCall {
    /// Source (account) that posted the call.
    pub source_id: u64,
    /// Channel the call was posted in.
    pub channel_id: u64,
    /// Token the call is about.
    pub token_id: u64,
    /// Capture timestamp in ns.
    pub timestamp_ns: u64,
    /// Content shingle hashes.
    pub shingles: Vec<u32>,
}

/// A detected copy-echo relationship: `echo` reposted content semantically matching
/// `originator`'s earlier call, within the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyEchoEdge {
    /// Earlier poster of the matching content.
    pub originator: u64,
    /// Later poster (the echo).
    pub echo: u64,
    /// Token the echo concerns.
    pub token_id: u64,
    /// Semantic similarity in bps (Jaccard × 10_000).
    pub similarity_bps: i64,
    /// Time lag from originator to echo in ns.
    pub lag_ns: u64,
}

/// Sorted-unique copy of a shingle slice. Deterministic (stable sort of integers).
fn norm(shingles: &[u32]) -> Vec<u32> {
    let mut v = shingles.to_vec();
    v.sort_unstable();
    v.dedup();
    v
}

/// Jaccard similarity in bps of two shingle sets: `|A∩B| / |A∪B| × 10_000`.
///
/// Inputs are normalised to sorted-unique internally, then intersected by a linear
/// two-pointer merge. Two empty sets yield `0` (no evidence of similarity).
#[must_use]
pub fn jaccard_bps(a: &[u32], b: &[u32]) -> i64 {
    let na = norm(a);
    let nb = norm(b);
    if na.is_empty() && nb.is_empty() {
        return 0;
    }
    let mut i = 0usize;
    let mut j = 0usize;
    let mut inter: u64 = 0;
    while i < na.len() && j < nb.len() {
        match na[i].cmp(&nb[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                inter += 1;
                i += 1;
                j += 1;
            }
        }
    }
    let union = (na.len() + nb.len()) as u64 - inter;
    ratio_bps(inter, union)
}

/// Detect copy-echo edges among calls on the (implicitly single) token set.
///
/// Calls are ordered deterministically by `(timestamp_ns, source_id, channel_id)`.
/// For every earlier→later pair whose lag is within `window_ns` and whose
/// [`jaccard_bps`] is at least `similarity_threshold_bps`, an edge is emitted from the
/// earlier source (originator) to the later source (echo). A call from the *same*
/// source is not counted as its own echo. Deterministic, allocation-bounded by the
/// number of qualifying pairs.
#[must_use]
pub fn detect_copy_echo(
    calls: &[ChannelCall],
    similarity_threshold_bps: i64,
    window_ns: u64,
) -> Vec<CopyEchoEdge> {
    let mut order: Vec<usize> = (0..calls.len()).collect();
    order.sort_by(|&x, &y| {
        (
            calls[x].timestamp_ns,
            calls[x].source_id,
            calls[x].channel_id,
        )
            .cmp(&(
                calls[y].timestamp_ns,
                calls[y].source_id,
                calls[y].channel_id,
            ))
    });
    let mut edges = Vec::new();
    for a in 0..order.len() {
        let ci = &calls[order[a]];
        for &yb in order.iter().skip(a + 1) {
            let cj = &calls[yb];
            if cj.source_id == ci.source_id {
                continue;
            }
            let lag = cj.timestamp_ns.saturating_sub(ci.timestamp_ns);
            if lag > window_ns {
                continue;
            }
            let sim = jaccard_bps(&ci.shingles, &cj.shingles);
            if sim >= similarity_threshold_bps {
                edges.push(CopyEchoEdge {
                    originator: ci.source_id,
                    echo: cj.source_id,
                    token_id: cj.token_id,
                    similarity_bps: sim,
                    lag_ns: lag,
                });
            }
        }
    }
    edges
}

/// Copy-echo density in bps: the share of calls that are echoes (have at least one
/// earlier, within-window, similar call from a different source).
///
/// This is the D7 "semantic copy-echo density" audience-authenticity input. `0` calls
/// → `0`.
#[must_use]
pub fn copy_echo_density_bps(
    calls: &[ChannelCall],
    similarity_threshold_bps: i64,
    window_ns: u64,
) -> i64 {
    if calls.is_empty() {
        return 0;
    }
    // Count the calls that acted as an echo: a call is an echo if any earlier call
    // from a different source, within the window, is similar enough.
    let mut order: Vec<usize> = (0..calls.len()).collect();
    order.sort_by(|&x, &y| {
        (calls[x].timestamp_ns, calls[x].source_id)
            .cmp(&(calls[y].timestamp_ns, calls[y].source_id))
    });
    let mut echo_count: u64 = 0;
    for a in 0..order.len() {
        let cj = &calls[order[a]];
        let mut is_echo = false;
        for &xb in order.iter().take(a) {
            let ci = &calls[xb];
            if ci.source_id == cj.source_id {
                continue;
            }
            let lag = cj.timestamp_ns.saturating_sub(ci.timestamp_ns);
            if lag <= window_ns
                && jaccard_bps(&ci.shingles, &cj.shingles) >= similarity_threshold_bps
            {
                is_echo = true;
                break;
            }
        }
        if is_echo {
            echo_count += 1;
        }
    }
    ratio_bps(echo_count, calls.len() as u64)
}
