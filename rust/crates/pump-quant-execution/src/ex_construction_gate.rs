//! Leaf `ex_construction_gate`: the Construction Validation Gate
//! (constitution **criteria 77 / 113**).
//!
//! ## Why this exists
//! Criterion 113 requires that a *construction validation gate* exist **from
//! authoring time** — before any live submission is wired — so that a built
//! transaction instruction is proven to match its intended on-chain effect
//! before it can ever reach the chain. Criterion 77 (the "laptop parts" of
//! validation) enumerates three rungs; two are fully deterministic and are
//! implemented here, the third is live-state simulation and is deferred to
//! Phase-B behind a trait:
//!
//! - **(a) fixture-parity** — a byte-level differential of the built
//!   instruction's canonical serialization (instruction data **and**
//!   account-meta ordering) against a golden fixture: `built_bytes == golden`.
//! - **(b) live-state simulation** — replay the instruction against a simulated
//!   chain state. *Deferred to Phase-B* ([`LiveStateSimulator`]); the gate is
//!   structured so it slots in without changing the deterministic rungs.
//! - **(c) micro-verification** — decode-round-trip: decode the built
//!   instruction back to its logical operation and assert it re-decodes to the
//!   same logical op that was requested.
//!
//! Because no Phase-A instruction *serializer with account metas* existed in
//! this decision crate, a minimal deterministic one is built here
//! ([`build_ix`]) — sufficient to exercise the gate against fixtures. It is
//! integer-only and credential-free; it is NOT a signing / submission path.
//!
//! ## Constitution refs
//! - criterion 77 — construction validation (laptop-side rungs).
//! - criterion 113 — the gate must exist from authoring time.
//! - §22 — integer-only, deterministic, no clock / RNG / float / I/O.

/// Which venue an instruction targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVenue {
    /// pump.fun bonding-curve program.
    PumpFun,
    /// PumpSwap (Pump AMM) program.
    PumpSwap,
}

/// Which side of the trade the instruction performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateSide {
    /// Buy (spend SOL, receive tokens).
    Buy,
    /// Sell (spend tokens, receive SOL).
    Sell,
}

/// An account reference with its signer / writable flags, in the exact order the
/// instruction requires. Account-meta **ordering** is part of what the gate
/// differentials against the golden fixture (a re-ordering is a construction
/// defect that must be caught).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountMeta {
    /// 32-byte account public key.
    pub pubkey: [u8; 32],
    /// Whether this account must sign.
    pub is_signer: bool,
    /// Whether this account is written.
    pub is_writable: bool,
}

/// The logical operation an instruction is intended to perform — the invariant
/// the round-trip rung checks. Two instructions with the same [`LogicalOp`] are
/// the same operation regardless of encoding incidentals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalOp {
    /// Target venue.
    pub venue: GateVenue,
    /// Trade side.
    pub side: GateSide,
    /// First u64 argument (buy: `min_tokens_out`; sell: `token_amount`).
    pub arg0: u64,
    /// Second u64 argument (buy: `max_sol_cost`; sell: `min_sol_out`).
    pub arg1: u64,
    /// The primary account (mint / pool) this op acts on.
    pub primary: [u8; 32],
}

/// A built instruction: program id, ordered account metas, and data bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltIx {
    /// Target program id.
    pub program_id: [u8; 32],
    /// Account metas in required order.
    pub accounts: Vec<AccountMeta>,
    /// Instruction data (discriminator ++ args).
    pub data: Vec<u8>,
}

/// pump.fun `global:buy` discriminator (matches `pump_quant_protocol::ix`).
pub const PUMPFUN_BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
/// pump.fun `global:sell` discriminator.
pub const PUMPFUN_SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];
/// PumpSwap `buy` discriminator (distinct namespace from pump.fun).
pub const PUMPSWAP_BUY_DISCRIMINATOR: [u8; 8] = [10, 20, 30, 40, 50, 60, 70, 80];
/// PumpSwap `sell` discriminator.
pub const PUMPSWAP_SELL_DISCRIMINATOR: [u8; 8] = [11, 21, 31, 41, 51, 61, 71, 81];

/// Program id bytes for pump.fun (deterministic placeholder — layout, not creds).
pub const PUMPFUN_PROGRAM_ID: [u8; 32] = [0xF0; 32];
/// Program id bytes for PumpSwap.
pub const PUMPSWAP_PROGRAM_ID: [u8; 32] = [0x5A; 32];

/// Total instruction-data length (8 discriminator + 2×8 args).
pub const IX_DATA_LEN: usize = 8 + 8 + 8;

/// The discriminator for a `(venue, side)` pair.
#[must_use]
fn discriminator(venue: GateVenue, side: GateSide) -> [u8; 8] {
    match (venue, side) {
        (GateVenue::PumpFun, GateSide::Buy) => PUMPFUN_BUY_DISCRIMINATOR,
        (GateVenue::PumpFun, GateSide::Sell) => PUMPFUN_SELL_DISCRIMINATOR,
        (GateVenue::PumpSwap, GateSide::Buy) => PUMPSWAP_BUY_DISCRIMINATOR,
        (GateVenue::PumpSwap, GateSide::Sell) => PUMPSWAP_SELL_DISCRIMINATOR,
    }
}

/// Program id for a venue.
#[must_use]
fn program_id(venue: GateVenue) -> [u8; 32] {
    match venue {
        GateVenue::PumpFun => PUMPFUN_PROGRAM_ID,
        GateVenue::PumpSwap => PUMPSWAP_PROGRAM_ID,
    }
}

/// Derive a deterministic 32-byte "account" from a tag and the primary key, so
/// the minimal builder produces a stable, fixture-able account set without any
/// real key derivation. Pure integer mixing — not cryptographic, not creds.
#[must_use]
fn derive_account(primary: &[u8; 32], tag: u8) -> [u8; 32] {
    let mut out = *primary;
    let mut i = 0;
    while i < 32 {
        out[i] = out[i].wrapping_add(tag).wrapping_add(i as u8);
        i += 1;
    }
    out[0] = tag;
    out
}

/// Build the minimal deterministic instruction for a logical op.
///
/// Account-meta order is fixed per `(venue, side)`:
/// index 0 = signer/payer, 1 = primary (mint/pool, writable), 2 = venue-derived
/// authority (writable). Data = discriminator ++ arg0 LE ++ arg1 LE.
#[must_use]
pub fn build_ix(op: LogicalOp) -> BuiltIx {
    let disc = discriminator(op.venue, op.side);
    let mut data = Vec::with_capacity(IX_DATA_LEN);
    data.extend_from_slice(&disc);
    data.extend_from_slice(&op.arg0.to_le_bytes());
    data.extend_from_slice(&op.arg1.to_le_bytes());

    let signer = derive_account(&op.primary, 0x01);
    let authority = derive_account(&op.primary, 0x02);

    let accounts = vec![
        AccountMeta {
            pubkey: signer,
            is_signer: true,
            is_writable: true,
        },
        AccountMeta {
            pubkey: op.primary,
            is_signer: false,
            is_writable: true,
        },
        AccountMeta {
            pubkey: authority,
            is_signer: false,
            is_writable: true,
        },
    ];

    BuiltIx {
        program_id: program_id(op.venue),
        accounts,
        data,
    }
}

/// Canonically serialize a [`BuiltIx`] to bytes for the byte-level parity
/// differential. The layout captures **both** the instruction data and the
/// account-meta ordering + flags, so a re-ordering or a flag flip changes the
/// bytes and is caught by the parity rung.
///
/// Layout:
/// ```text
/// program_id (32)
/// account_count (u32 LE)
/// per account: pubkey(32) ++ signer(1) ++ writable(1)
/// data_len (u32 LE)
/// data (data_len)
/// ```
#[must_use]
pub fn serialize(ix: &BuiltIx) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + 4 + ix.accounts.len() * 34 + 4 + ix.data.len());
    out.extend_from_slice(&ix.program_id);
    out.extend_from_slice(&(ix.accounts.len() as u32).to_le_bytes());
    for a in &ix.accounts {
        out.extend_from_slice(&a.pubkey);
        out.push(u8::from(a.is_signer));
        out.push(u8::from(a.is_writable));
    }
    out.extend_from_slice(&(ix.data.len() as u32).to_le_bytes());
    out.extend_from_slice(&ix.data);
    out
}

/// The golden fixture for a logical op: the canonical serialization of the
/// correctly-built instruction. In production the golden bytes are curated /
/// pinned; here they are derived from [`build_ix`] so tests can construct both
/// a matching and a deliberately-mutated fixture.
#[must_use]
pub fn golden_fixture(op: LogicalOp) -> Vec<u8> {
    serialize(&build_ix(op))
}

/// Decode a built instruction back to its [`LogicalOp`] (the micro-verification
/// rung). Fails closed (`None`) on any structural mismatch: wrong program id,
/// unknown discriminator, short data, or a missing primary account.
#[must_use]
pub fn decode_ix(ix: &BuiltIx) -> Option<LogicalOp> {
    if ix.data.len() != IX_DATA_LEN {
        return None;
    }
    let venue = if ix.program_id == PUMPFUN_PROGRAM_ID {
        GateVenue::PumpFun
    } else if ix.program_id == PUMPSWAP_PROGRAM_ID {
        GateVenue::PumpSwap
    } else {
        return None;
    };

    let mut disc = [0u8; 8];
    disc.copy_from_slice(&ix.data[0..8]);
    let side = if disc == discriminator(venue, GateSide::Buy) {
        GateSide::Buy
    } else if disc == discriminator(venue, GateSide::Sell) {
        GateSide::Sell
    } else {
        return None;
    };

    let mut a0 = [0u8; 8];
    let mut a1 = [0u8; 8];
    a0.copy_from_slice(&ix.data[8..16]);
    a1.copy_from_slice(&ix.data[16..24]);

    // The primary account is the writable non-signer at index 1 (fixed order).
    let primary = ix.accounts.get(1).map(|m| m.pubkey)?;

    Some(LogicalOp {
        venue,
        side,
        arg0: u64::from_le_bytes(a0),
        arg1: u64::from_le_bytes(a1),
        primary,
    })
}

/// Reason a construction validation rung rejected an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateRejection {
    /// Rung (a): built bytes differ from the golden fixture (data or account
    /// ordering / flags mismatch).
    FixtureParityMismatch,
    /// Rung (c): the built instruction did not decode back to the intended
    /// logical op (or failed to decode at all).
    RoundTripMismatch,
    /// Rung (b): live-state simulation rejected the instruction.
    LiveStateRejected,
}

/// Outcome of running the construction validation gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveValidatedStatus {
    /// Both deterministic rungs (parity + round-trip) passed; the live-state
    /// simulation rung is deferred to Phase-B and was not run.
    ValidatedDeterministic,
    /// All rungs incl. live-state simulation passed (Phase-B path).
    ValidatedLive,
    /// A rung rejected the instruction.
    Rejected(GateRejection),
}

impl LiveValidatedStatus {
    /// Whether the instruction is cleared for use through the rungs that ran.
    #[must_use]
    #[inline]
    pub fn is_validated(self) -> bool {
        matches!(
            self,
            LiveValidatedStatus::ValidatedDeterministic | LiveValidatedStatus::ValidatedLive
        )
    }
}

/// Phase-B live-state simulation rung, behind a trait so it can be wired later
/// without touching the deterministic rungs. Implementations replay the built
/// instruction against a simulated chain state and report whether it would
/// succeed. **No implementation is provided in Phase-A** beyond the explicit
/// deferral stub [`PhaseBDeferredSim`].
pub trait LiveStateSimulator {
    /// Simulate `ix` (its intended `op` supplied for context). Return `true` iff
    /// the instruction is accepted by the simulated state.
    fn simulate(&self, ix: &BuiltIx, op: LogicalOp) -> bool;
}

/// The Phase-A stand-in for the Phase-B live-state simulator. It performs **no**
/// simulation (that rung is deferred); callers use the deterministic-only gate
/// entry point [`ConstructionValidationGate::validate`] until a real
/// [`LiveStateSimulator`] is wired in Phase-B.
#[derive(Debug, Clone, Copy, Default)]
pub struct PhaseBDeferredSim;

impl LiveStateSimulator for PhaseBDeferredSim {
    /// PHASE-B: not implemented. Returns `false` so it can never be mistaken for
    /// a passing live check — the deterministic-only entry point must be used
    /// until real simulation exists.
    fn simulate(&self, _ix: &BuiltIx, _op: LogicalOp) -> bool {
        false
    }
}

/// The construction validation gate (criteria 77 / 113).
#[derive(Debug, Clone, Copy, Default)]
pub struct ConstructionValidationGate;

impl ConstructionValidationGate {
    /// Run the two deterministic rungs against `built`:
    /// 1. **fixture-parity** — `serialize(built) == golden`.
    /// 2. **micro-verification** — `decode_ix(built) == Some(intended)`.
    ///
    /// Returns [`LiveValidatedStatus::ValidatedDeterministic`] on success (the
    /// Phase-B live-state rung is deferred), else a [`GateRejection`].
    #[must_use]
    pub fn validate(built: &BuiltIx, intended: LogicalOp, golden: &[u8]) -> LiveValidatedStatus {
        // Rung (a): byte-level differential of data + account-meta ordering.
        if serialize(built) != golden {
            return LiveValidatedStatus::Rejected(GateRejection::FixtureParityMismatch);
        }
        // Rung (c): decode round-trip to the same logical op.
        match decode_ix(built) {
            Some(op) if op == intended => LiveValidatedStatus::ValidatedDeterministic,
            _ => LiveValidatedStatus::Rejected(GateRejection::RoundTripMismatch),
        }
    }

    /// Full gate: the two deterministic rungs plus the Phase-B live-state
    /// simulation rung supplied by `sim`. On deterministic success it runs the
    /// simulator and returns [`LiveValidatedStatus::ValidatedLive`] iff the
    /// simulation also accepts.
    #[must_use]
    pub fn validate_with_sim<S: LiveStateSimulator>(
        built: &BuiltIx,
        intended: LogicalOp,
        golden: &[u8],
        sim: &S,
    ) -> LiveValidatedStatus {
        match Self::validate(built, intended, golden) {
            LiveValidatedStatus::ValidatedDeterministic => {
                if sim.simulate(built, intended) {
                    LiveValidatedStatus::ValidatedLive
                } else {
                    LiveValidatedStatus::Rejected(GateRejection::LiveStateRejected)
                }
            }
            other => other,
        }
    }
}
