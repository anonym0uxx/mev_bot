//! The decision journal: a canonical, hashable record of what the engine did.
//!
//! Determinism is only useful if it is *checkable*. Every material decision — a
//! promotion, a gate verdict, a fill, a reflection weight move — is folded, in a
//! fixed integer encoding, into a rolling FNV-1a hash. Two runs over the same events
//! produce identical `digest()`s; a single divergence (a non-determinism bug, an
//! accidental wall-clock read) flips the hash. That is what makes replay a
//! correctness authority rather than a demo (§54).
//!
//! The engine runs indefinitely, so the journal keeps **bounded** state (§99): the
//! digest is a running 64-bit fold (constant space), and only the most recent
//! `RECENT_CAP` decisions are retained for inspection. The FNV-1a constants and
//! byte encoding match `pump_quant_memory::hashing::fnv1a_64`, so the rolling digest
//! equals a one-shot hash over the full decision stream — folding incrementally just
//! avoids retaining that stream.

use std::collections::VecDeque;

/// FNV-1a/64 offset basis — identical to `pump_quant_memory::hashing`.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a/64 prime — identical to `pump_quant_memory::hashing`.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// How many recent decisions to retain for inspection. Bounds journal memory in the
/// long-running loop; the digest still covers *all* decisions, retained or not.
const RECENT_CAP: usize = 4_096;

/// A single journaled decision, in the order the engine took it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// A candidate was promoted to the gate at a given rank.
    Promoted { mint: [u8; 32], lane: u8, rank: u64 },
    /// The gate admitted a candidate at a chosen size, with the §34.4 provenance
    /// of that decision: the economic size band `[x_min, x_cost, x_max]` the size
    /// was clamped within, the attempt/fail-rate multiplier (bps) that inflated the
    /// per-attempt fixed cost, and the round-trip impact cost (bps) measured at the
    /// admitted size — the exact inputs computed at admit, folded so the journal
    /// record is a complete, replayable account of WHY this size was admitted, not
    /// just THAT it was (§34.4 DecisionRecord completeness).
    Admitted {
        mint: [u8; 32],
        size_lamports: u64,
        x_min: u64,
        x_cost: u64,
        x_max: u64,
        fail_rate_bps: u32,
        rt_cost_bps: u32,
        /// The ONE expected favourable move (signed bps) this size was justified by
        /// — the benefit half of the admission, which the record previously omitted
        /// entirely. Signed, because a lane that has lost money must be able to say
        /// so (`crate::priced_move::PricedMove`).
        move_bps: i128,
        /// Which estimator produced `move_bps`
        /// (`crate::priced_move::MoveSource::code`): 0 cold-start constant, 1 lane
        /// realized expectancy, 2 the calibrated per-candidate model. Recorded so a
        /// replay can attribute a size to the thing that priced it.
        move_source: u8,
        /// Provenance of the depth the capacity cap came from
        /// (`crate::curve_depth::CurveDepth::basis_code`): 0 unknown, 1 derived from
        /// the `real_sol = virtual_sol − 30 SOL` identity, 2 decoded from the curve
        /// account, 3 a migrated pool.
        depth_basis: u8,
    },
    /// The gate rejected a candidate; `reason` is a stable small code.
    Rejected { mint: [u8; 32], reason: u8 },
    /// A paper scalp realized a signed net PnL. `reason` is the stable
    /// [`crate::position::ExitReason::code`] that fired the exit (0 = legacy/unknown),
    /// so exit-policy attribution (§48/§49) survives into the journal.
    Filled {
        mint: [u8; 32],
        net_pnl_lamports: i128,
        reason: u8,
    },
    /// A paper-MODEL routing fill closed. Cash and inventory settled exactly as for any exit, but the
    /// fill is NOT assessable (quote and/or landing unvalidated), so it is journalled as this distinct
    /// record instead of `Filled`: a consumer summing `Filled` PnL can never include it. `status`
    /// bit 0 = quote validated, bit 1 = landing validated (a routing fill has bit 1 clear).
    RoutingExit {
        mint: [u8; 32],
        net_pnl_lamports: i128,
        reason: u8,
        status: u8,
    },
    /// Contradictory execution evidence for one order. `closed` = 1 when the order's position is
    /// already settled (needs a deliberate ledger adjustment). Durable audit of the fault itself.
    ReconFault {
        mint: [u8; 32],
        order_id: u64,
        closed: u8,
    },
    /// A reflection pass moved a lane weight.
    Reweighted {
        lane: u8,
        before_bp: u32,
        after_bp: u32,
    },
    /// A sub-`x_min` calibration probe (§33/§43): NOT a position — a size below
    /// the economic cost floor whose outcome is *paid information*, routed through
    /// the calibration budget and labeled as budgeted research expenditure.
    /// `cost_lamports` is the accounted research spend, `measurement_id` names the
    /// measurement it funds. Recorded INSTEAD of `Admitted` when probe-budget
    /// accounting is on, so the journal never conflates a research probe with a
    /// profit-seeking position.
    Probe {
        mint: [u8; 32],
        cost_lamports: u64,
        measurement_id: u32,
    },
}

impl Decision {
    /// A stable 1-byte tag so the encoding is unambiguous across variants.
    const fn tag(&self) -> u8 {
        match self {
            Decision::Promoted { .. } => 1,
            Decision::Admitted { .. } => 2,
            Decision::Rejected { .. } => 3,
            Decision::Filled { .. } => 4,
            Decision::Reweighted { .. } => 5,
            Decision::Probe { .. } => 6,
            Decision::RoutingExit { .. } => 7,
            Decision::ReconFault { .. } => 8,
        }
    }

    /// Encode into a reusable scratch buffer in the same layout the memory crate's
    /// `push_*` helpers use (length-prefixed byte fields, little-endian integers).
    fn encode(&self, buf: &mut Vec<u8>) {
        buf.clear();
        buf.push(self.tag());
        match *self {
            Decision::Promoted { mint, lane, rank } => {
                push_bytes(buf, &mint);
                buf.push(lane);
                push_u64(buf, rank);
            }
            Decision::Admitted {
                mint,
                size_lamports,
                x_min,
                x_cost,
                x_max,
                fail_rate_bps,
                rt_cost_bps,
                move_bps,
                move_source,
                depth_basis,
            } => {
                push_bytes(buf, &mint);
                push_u64(buf, size_lamports);
                push_u64(buf, x_min);
                push_u64(buf, x_cost);
                push_u64(buf, x_max);
                push_u64(buf, u64::from(fail_rate_bps));
                push_u64(buf, u64::from(rt_cost_bps));
                // Signed 128-bit move as two's-complement bytes: exact, sign-stable,
                // the same encoding `Filled` uses for signed PnL.
                push_bytes(buf, &move_bps.to_le_bytes());
                buf.push(move_source);
                buf.push(depth_basis);
            }
            Decision::Rejected { mint, reason } => {
                push_bytes(buf, &mint);
                buf.push(reason);
            }
            Decision::Filled {
                mint,
                net_pnl_lamports,
                reason,
            } => {
                push_bytes(buf, &mint);
                // Signed 128-bit PnL as two's-complement bytes: exact, sign-stable.
                push_bytes(buf, &net_pnl_lamports.to_le_bytes());
                buf.push(reason);
            }
            Decision::RoutingExit {
                mint,
                net_pnl_lamports,
                reason,
                status,
            } => {
                push_bytes(buf, &mint);
                push_bytes(buf, &net_pnl_lamports.to_le_bytes());
                buf.push(reason);
                buf.push(status);
            }
            Decision::ReconFault {
                mint,
                order_id,
                closed,
            } => {
                push_bytes(buf, &mint);
                push_u64(buf, order_id);
                buf.push(closed);
            }
            Decision::Reweighted {
                lane,
                before_bp,
                after_bp,
            } => {
                buf.push(lane);
                push_u64(buf, before_bp as u64);
                push_u64(buf, after_bp as u64);
            }
            Decision::Probe {
                mint,
                cost_lamports,
                measurement_id,
            } => {
                push_bytes(buf, &mint);
                push_u64(buf, cost_lamports);
                push_u64(buf, u64::from(measurement_id));
            }
        }
    }
}

fn push_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn push_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    push_u64(buf, bytes.len() as u64);
    buf.extend_from_slice(bytes);
}

/// An append-only journal with a rolling canonical hash and bounded retention.
#[derive(Clone, Debug)]
pub struct DecisionJournal {
    /// Rolling FNV-1a state over every decision ever recorded.
    hash: u64,
    /// Total decisions recorded (not just retained).
    count: u64,
    /// The most recent decisions, capped at `RECENT_CAP`.
    recent: VecDeque<Decision>,
    /// Reused encoding scratch so `record` allocates nothing steady-state.
    scratch: Vec<u8>,
}

impl Default for DecisionJournal {
    fn default() -> Self {
        Self {
            hash: FNV_OFFSET_BASIS,
            count: 0,
            recent: VecDeque::new(),
            scratch: Vec::with_capacity(64),
        }
    }
}

impl DecisionJournal {
    /// A fresh, empty journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold an arbitrary seed (e.g. the canonical strategy-config hash, §19/§56.2)
    /// into the rolling digest BEFORE any decision is recorded, so two runs under
    /// different configs can never share a digest. Call once, at construction time.
    pub fn seed(&mut self, seed: u64) {
        for b in seed.to_le_bytes() {
            self.hash ^= u64::from(b);
            self.hash = self.hash.wrapping_mul(FNV_PRIME);
        }
    }

    /// Append a decision: fold it into the rolling digest and retain it (evicting
    /// the oldest once `RECENT_CAP` is reached).
    pub fn record(&mut self, d: Decision) {
        let mut scratch = std::mem::take(&mut self.scratch);
        d.encode(&mut scratch);
        for &b in &scratch {
            self.hash ^= u64::from(b);
            self.hash = self.hash.wrapping_mul(FNV_PRIME);
        }
        self.scratch = scratch;

        if self.recent.len() == RECENT_CAP {
            self.recent.pop_front();
        }
        self.recent.push_back(d);
        self.count = self.count.saturating_add(1);
    }

    /// Total number of decisions recorded over the engine's life.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.count
    }

    /// Whether any decision has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The most recent decisions retained (up to `RECENT_CAP`), oldest first.
    pub fn recent(&self) -> impl Iterator<Item = &Decision> {
        self.recent.iter()
    }

    /// The canonical FNV-1a digest over *all* recorded decisions. Identical across
    /// any two runs that took identical decisions, and equal to a one-shot
    /// `fnv1a_64` over the concatenated encodings.
    #[must_use]
    pub fn digest(&self) -> u64 {
        self.hash
    }
}

#[cfg(test)]
mod serialization_pins {
    //! Journal serialization is a COMPATIBILITY surface: recorded journals and their digests must keep
    //! meaning what they meant. These pins were computed by an INDEPENDENT implementation of the
    //! documented layout (length-prefixed bytes, little-endian integers, two's-complement i128, rolling
    //! FNV-1a) in `consolidation/journal_pin.py`, not by running this code, so an edit to `encode`, a
    //! tag, or the hash fails here. They replace the retired golden-digest test's journal assertion
    //! without pinning any strategy behaviour.
    use super::*;

    const MINT: [u8; 32] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    fn records() -> [(&'static str, Decision, usize, u64); 8] {
        [
            (
                "promoted",
                Decision::Promoted {
                    mint: MINT,
                    lane: 3,
                    rank: 7,
                },
                50,
                0x7db6_4f63_c910_d3fa,
            ),
            (
                "admitted",
                Decision::Admitted {
                    mint: MINT,
                    size_lamports: 300_000_000,
                    x_min: 0,
                    x_cost: 0,
                    x_max: 0,
                    fail_rate_bps: 50,
                    rt_cost_bps: 180,
                    move_bps: -42,
                    move_source: 1,
                    depth_basis: 2,
                },
                115,
                0xc846_3ba6_76a8_2bfa,
            ),
            (
                "rejected",
                Decision::Rejected {
                    mint: MINT,
                    reason: 29,
                },
                42,
                0x04d5_4084_79af_4d5d,
            ),
            (
                "filled",
                Decision::Filled {
                    mint: MINT,
                    net_pnl_lamports: -1_234_567,
                    reason: 10,
                },
                66,
                0x4274_204d_22ba_3259,
            ),
            (
                "reweighted",
                Decision::Reweighted {
                    lane: 2,
                    before_bp: 1000,
                    after_bp: 1100,
                },
                18,
                0x1baa_a20f_66d5_5893,
            ),
            (
                "probe",
                Decision::Probe {
                    mint: MINT,
                    cost_lamports: 5_000_000,
                    measurement_id: 9,
                },
                57,
                0xd354_1323_e644_e9d5,
            ),
            (
                "routing_exit",
                Decision::RoutingExit {
                    mint: MINT,
                    net_pnl_lamports: 777,
                    reason: 8,
                    status: 1,
                },
                67,
                0xf295_531a_6bd1_8f75,
            ),
            (
                "recon_fault",
                Decision::ReconFault {
                    mint: MINT,
                    order_id: 99,
                    closed: 1,
                },
                50,
                0x15a1_8c94_b39f_c22f,
            ),
        ]
    }

    #[test]
    fn every_variant_encodes_to_the_pinned_layout_and_hash() {
        for (name, d, len, digest) in records() {
            let mut buf = Vec::new();
            d.encode(&mut buf);
            assert_eq!(buf.len(), len, "{name}: encoded length changed");
            let mut j = DecisionJournal::new();
            j.record(d);
            assert_eq!(
                j.digest(),
                digest,
                "{name}: digest changed - a journal compatibility break"
            );
        }
    }

    #[test]
    fn the_variant_tags_are_frozen() {
        let tags: Vec<u8> = records().iter().map(|(_, d, _, _)| d.tag()).collect();
        assert_eq!(
            tags,
            vec![1, 2, 3, 4, 5, 6, 7, 8],
            "serialized tags must never be renumbered"
        );
    }

    #[test]
    fn the_whole_sequence_digest_is_pinned_and_order_sensitive() {
        let mut j = DecisionJournal::new();
        for (_, d, _, _) in records() {
            j.record(d);
        }
        assert_eq!(j.digest(), 0xf15b_f9e2_6486_9b97, "sequence digest changed");
        let mut swapped = DecisionJournal::new();
        let recs = records();
        for i in [1usize, 0, 2, 3, 4, 5, 6, 7] {
            swapped.record(recs[i].1);
        }
        assert_ne!(
            swapped.digest(),
            j.digest(),
            "the digest must depend on decision order"
        );
    }

    #[test]
    fn a_signed_pnl_survives_encoding_exactly_for_both_signs() {
        let enc = |v: i128| {
            let mut b = Vec::new();
            Decision::Filled {
                mint: MINT,
                net_pnl_lamports: v,
                reason: 0,
            }
            .encode(&mut b);
            b
        };
        assert_ne!(enc(-1), enc(1), "sign must be preserved");
        assert_eq!(
            enc(i128::MIN).len(),
            enc(0).len(),
            "fixed-width two's complement"
        );
    }

    #[test]
    fn the_seed_separates_configs_before_any_decision() {
        let (mut a, mut b) = (DecisionJournal::new(), DecisionJournal::new());
        a.seed(1);
        b.seed(2);
        assert_ne!(a.digest(), b.digest());
        assert_eq!(DecisionJournal::new().digest(), FNV_OFFSET_BASIS);
    }
}
