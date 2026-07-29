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
