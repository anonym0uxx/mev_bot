//! Leaf `si_incident_gate`: the incident-branch remediation admission gate.
//!
//! ## Responsibility
//! Enforce that a *model-produced* remediation (an incident-branch proposal to
//! recover a stuck exit) can **never reach chain** unless it independently
//! passes both:
//!   1. a live-state **sell-simulation** proof (the position is actually
//!      sellable against decoded reserves for at least the required amount), and
//!   2. the **signing policy** boundary (approved program, size within cap).
//!
//! The gate is *model-independent* by construction: the model only proposes the
//! remediation *action* (which position, which program, what tx size/digest).
//! The gate never trusts a model-supplied out-amount or signature — it
//! **recomputes** the sell out-amount from decoded reserves and **recomputes**
//! the signature through the signing policy. A model that lies about either is
//! rejected, and a chain-reaching artifact ([`AdmittedRemediation`]) is only
//! ever produced from gate-computed values.
//!
//! This composes the ported `si_sell_simulation` piece ([`simulate_sell`]) and
//! the `si_signing_boundary` piece ([`sign_through_policy`]) into a single
//! deterministic guard ([`si_incident_gate`]).
//!
//! ## Model-independence of the *deterministic* exit path
//! Criterion 79/80 also require that the deterministic exit path (the escalation
//! ladder) never blocks on model availability. That is proved here at
//! compile-time by the [`ModelIndependent`] marker trait and the
//! [`assert_model_independent`] witness: every type the deterministic exit step
//! ([`deterministic_exit_step`]) consumes implements `ModelIndependent`, while
//! the model-produced [`RemediationProposal`] deliberately does **not**. Feeding
//! a model-derived type into the deterministic exit path therefore fails to
//! compile.
//!
//! ## Constitution refs
//! - **Criterion 80:** incident-branch (model-produced) remediations cannot
//!   reach chain without passing live-state simulation and the signing policy.
//! - **Criterion 79:** the deterministic ExitRemediationLadder recovers exits
//!   without model involvement (proved model-independent here).
//! - **§22:** integer-only; the constant-product simulation uses widened `u128`
//!   intermediates and no floats.
//! - **Overflow:** simulation uses `checked_*`; the signing mixer is
//!   `wrapping_*`-by-contract (a boundary demonstration, not a real scheme).

use crate::ex_sell_ladder_state::{sell_ladder_next, LadderCtx, LadderState, SellOutcome};

// ---------------------------------------------------------------------------
// Composed piece A: sell simulation (port of `si_sell_simulation`)
// ---------------------------------------------------------------------------

/// A held position an incident remediation would try to liquidate.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Position {
    /// Token amount held.
    pub token_amount: u64,
    /// Token mint identifier.
    pub mint: u64,
}

/// Decoded market reserves plus whether a sell instruction is constructible
/// against this decoded state.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DecodedMarket {
    /// Base (token) reserve.
    pub base_reserve: u64,
    /// Quote reserve.
    pub quote_reserve: u64,
    /// Whether a sell instruction can be constructed against this decoded state.
    pub constructible: bool,
}

/// Proof that a position is sellable, carrying the *gate-computed* simulated
/// quote out-amount for selling the whole position.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SellProof {
    /// Simulated quote out-amount for selling the whole position.
    pub out_amount: u64,
}

/// Why a sell could not be proven.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SellUnprovable {
    /// The sell instruction could not be constructed (or the position is empty).
    Unconstructible,
    /// Reserves/liquidity are insufficient for the position size.
    InsufficientLiquidity,
}

/// Simulate selling the entire position against decoded reserves using a
/// constant-product (`x * y = k`, no fees) model in integer math.
///
/// `out = quote * amount / (base + amount)`. Never assumes sellability: an
/// unconstructible market, an empty position, empty reserves, or a rounded-to-
/// zero out-amount all yield an error.
///
/// Constitution §22: widened `u128` intermediates, no floats; overflow on the
/// widened multiply/add is handled with `checked_*`.
pub fn simulate_sell(pos: &Position, state: &DecodedMarket) -> Result<SellProof, SellUnprovable> {
    if !state.constructible {
        return Err(SellUnprovable::Unconstructible);
    }
    if pos.token_amount == 0 {
        return Err(SellUnprovable::Unconstructible);
    }
    if state.base_reserve == 0 || state.quote_reserve == 0 {
        return Err(SellUnprovable::InsufficientLiquidity);
    }

    let amount = pos.token_amount as u128;
    let base = state.base_reserve as u128;
    let quote = state.quote_reserve as u128;

    let denom = base
        .checked_add(amount)
        .ok_or(SellUnprovable::Unconstructible)?;
    let out = quote
        .checked_mul(amount)
        .ok_or(SellUnprovable::Unconstructible)?
        / denom;

    if out == 0 {
        return Err(SellUnprovable::InsufficientLiquidity);
    }
    // `out <= quote <= u64::MAX`, so this narrowing cannot truncate.
    Ok(SellProof {
        out_amount: out as u64,
    })
}

// ---------------------------------------------------------------------------
// Composed piece B: signing boundary (port of `si_signing_boundary`)
// ---------------------------------------------------------------------------

/// Deterministic signing mixer used by [`KeyHandle`]. Exposed as a free
/// function so tests can compute the expected signature independently; it is a
/// boundary demonstration, **not** a real signature scheme. Uses
/// `wrapping_*`-by-contract arithmetic (constitution: explicit overflow).
pub fn sign_digest(seed: u64, digest: u64) -> u64 {
    digest
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(seed)
        .rotate_left(17)
}

/// Opaque handle to a trading key. It intentionally does **not** implement
/// `Debug`, `Display`, `Clone`, or any accessor to the raw seed — the key is
/// non-exportable to the model/agent by construction.
pub struct KeyHandle {
    seed: u64,
}

impl KeyHandle {
    /// Create a handle from an opaque key seed. There is no inverse: no method
    /// returns the seed or any key bytes.
    pub fn new(seed: u64) -> Self {
        KeyHandle { seed }
    }

    /// Deterministically sign a digest without ever returning key material.
    fn sign(&self, digest: u64) -> u64 {
        sign_digest(self.seed, digest)
    }
}

/// Signing policy: the set of approved programs, a serialized-size cap, and the
/// opaque (non-exportable) signing key.
pub struct SignPolicy {
    /// Programs permitted to be signed for.
    pub approved_programs: Vec<u64>,
    /// Maximum permitted serialized transaction size, in bytes.
    pub max_tx_size: u32,
    /// The opaque signing key (non-exportable).
    pub key: KeyHandle,
}

/// A signed transaction. Carries only the signature, never key material.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SignedTx {
    /// The produced signature.
    pub signature: u64,
}

/// Why a signing request is refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SignError {
    /// The request violated policy (unapproved program or over-cap size).
    PolicyDenied,
}

/// Sign a request only if it satisfies policy. Policy is checked *before*
/// signing; the key is borrowed and never returned.
pub fn sign_through_policy(
    program_id: u64,
    tx_size: u32,
    digest: u64,
    policy: &SignPolicy,
) -> Result<SignedTx, SignError> {
    if !policy.approved_programs.contains(&program_id) {
        return Err(SignError::PolicyDenied);
    }
    if tx_size > policy.max_tx_size {
        return Err(SignError::PolicyDenied);
    }
    Ok(SignedTx {
        signature: policy.key.sign(digest),
    })
}

// ---------------------------------------------------------------------------
// The gate itself
// ---------------------------------------------------------------------------

/// A model-produced incident remediation *proposal*. This is untrusted input:
/// it describes the recovery action the model wants (which position to sell,
/// against which decoded market, targeting which program, at what serialized
/// size and digest) but carries **no** authority — no out-amount, no signature.
///
/// It deliberately does **not** implement [`ModelIndependent`]: this is the
/// type-level boundary that keeps model output out of the deterministic exit
/// path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RemediationProposal {
    /// The position the model proposes to liquidate.
    pub position: Position,
    /// Program the remediation transaction would target.
    pub program_id: u64,
    /// Serialized size of the proposed transaction, in bytes.
    pub tx_size: u32,
    /// Digest the remediation transaction would sign.
    pub digest: u64,
}

/// Everything the gate needs to independently validate a proposal.
pub struct IncidentGateInput<'a> {
    /// The model-produced proposal (untrusted).
    pub proposal: RemediationProposal,
    /// Decoded live-market state to simulate the sell against.
    pub market: DecodedMarket,
    /// The minimum acceptable simulated out-amount for the remediation to be
    /// worth executing; a proposal that would recover less is rejected.
    pub min_out_amount: u64,
    /// The signing policy boundary.
    pub policy: &'a SignPolicy,
}

/// A remediation that has passed **both** gates and may reach chain. It carries
/// only gate-computed artifacts (the simulated proof and the policy-produced
/// signature) plus the program it is authorized for — never any model-supplied
/// out-amount or signature.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AdmittedRemediation {
    /// The gate-recomputed sell-simulation proof.
    pub proof: SellProof,
    /// The policy-produced signed transaction.
    pub signed: SignedTx,
    /// The program the remediation is authorized for.
    pub program_id: u64,
}

/// Why an incident remediation was refused admission to chain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IncidentReject {
    /// The sell could not be proven against decoded reserves.
    Unsellable(SellUnprovable),
    /// The sell is provable but recovers less than the required minimum.
    BelowMinOut {
        /// The gate-simulated out-amount.
        simulated: u64,
        /// The required minimum out-amount.
        required: u64,
    },
    /// The signing policy denied the request.
    SigningDenied(SignError),
}

/// The incident-branch remediation admission gate (criterion 80).
///
/// Deterministic guard composing [`simulate_sell`] and [`sign_through_policy`].
/// A model-produced remediation reaches chain **only** if, in order:
///   1. the sell simulates successfully against decoded reserves, **and**
///   2. the simulated out-amount meets `min_out_amount`, **and**
///   3. the resulting transaction passes the signing policy.
///
/// The simulation is performed *before* signing (defense-in-depth: never sign
/// something not first proven sellable). All returned values are gate-computed,
/// so the model cannot smuggle a fabricated out-amount or signature onto chain.
pub fn si_incident_gate(input: &IncidentGateInput) -> Result<AdmittedRemediation, IncidentReject> {
    // Gate 1: independently prove the position is sellable.
    let proof = simulate_sell(&input.proposal.position, &input.market)
        .map_err(IncidentReject::Unsellable)?;

    // Economic floor: the remediation must actually recover the required amount.
    if proof.out_amount < input.min_out_amount {
        return Err(IncidentReject::BelowMinOut {
            simulated: proof.out_amount,
            required: input.min_out_amount,
        });
    }

    // Gate 2: independently sign through policy (approved program + size cap).
    let signed = sign_through_policy(
        input.proposal.program_id,
        input.proposal.tx_size,
        input.proposal.digest,
        input.policy,
    )
    .map_err(IncidentReject::SigningDenied)?;

    Ok(AdmittedRemediation {
        proof,
        signed,
        program_id: input.proposal.program_id,
    })
}

// ---------------------------------------------------------------------------
// Static proof: the deterministic exit path has zero model dependency
// ---------------------------------------------------------------------------

/// Compile-time marker for types that carry **no** model-derived data and may
/// therefore appear in the deterministic exit path. Implemented for the
/// on-chain-driven ladder types; **not** implemented for
/// [`RemediationProposal`].
pub trait ModelIndependent {}

impl ModelIndependent for SellOutcome {}
impl ModelIndependent for LadderState {}
impl ModelIndependent for LadderCtx {}

/// Zero-cost witness that a type is model-independent. This function exists so a
/// test (and the compiler) can assert that the exit-path input types satisfy
/// the [`ModelIndependent`] bound; attempting `assert_model_independent::
/// <RemediationProposal>()` would fail to compile, which is the static proof
/// that model output cannot enter the deterministic exit path.
pub fn assert_model_independent<T: ModelIndependent>() {}

/// Advance the deterministic exit ladder by one step. This is the criterion-79
/// exit path: it consumes only [`ModelIndependent`] inputs (a [`LadderState`]
/// and a [`LadderCtx`] built from an on-chain [`SellOutcome`]) and delegates to
/// [`sell_ladder_next`]. It has zero model dependency by construction and can
/// never block on model availability.
pub fn deterministic_exit_step(cur: LadderState, ctx: LadderCtx) -> LadderState {
    // Bound-checked witness: both inputs are model-independent.
    assert_model_independent::<LadderState>();
    assert_model_independent::<LadderCtx>();
    sell_ladder_next(cur, ctx)
}
