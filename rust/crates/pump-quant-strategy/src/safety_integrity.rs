//! safety_integrity — the safety-boundary and data-integrity core.
//!
//! This module contains the pure, laptop-buildable functions that enforce the
//! trading constitution's hardest guarantees *by construction*: bad data, model
//! output, un-simulated orders and copy-trading are kept OUT of the live path,
//! and cost math stays honest across quote mints.
//!
//! Design rules obeyed throughout:
//!  * No `f32`/`f64` in any outcome-controlling path. Lamports are `u64`/`u128`
//!    /`i128`; ratios are expressed in basis points (bps).
//!  * Overflow is always explicit (checked / saturating), never silent.
//!  * Every function is total and deterministic: identical inputs always produce
//!    identical outputs, and bad input returns a typed refusal instead of a panic.

use std::collections::VecDeque;

// ---------------------------------------------------------------------------
// leaf: si_missingness
// ---------------------------------------------------------------------------

/// Source-evidence identifier plus the resolved integer value it carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Evidence {
    /// Identifier of the on-chain / decoded evidence this value came from.
    pub evidence_id: u64,
    /// The resolved integer value (never a fabricated default).
    pub value: i128,
}

/// Raw evidence for a single field before resolution. `parsed` is `None` when
/// the field was present on the wire but could not be parsed into a number.
#[derive(Clone, Debug)]
pub struct FieldEvidence {
    /// Identifier of the evidence source.
    pub evidence_id: u64,
    /// Cleanly parsed integer value, or `None` if present-but-unparseable.
    pub parsed: Option<i128>,
}

/// Explicit missingness state. Missing data never silently becomes `0`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FieldState {
    /// Fully resolved value carrying its source evidence.
    Known(Evidence),
    /// Optional field that was absent.
    Unknown,
    /// Field present but unparseable — deliberately not defaulted to a number.
    Incomplete,
    /// Required field that was absent — hard refusal.
    Reject,
}

/// Map an optional/partial field to an explicit missingness state.
///
/// * `None` + `required` → [`FieldState::Reject`]
/// * `None` + optional   → [`FieldState::Unknown`]
/// * present-but-unparseable → [`FieldState::Incomplete`]
/// * fully resolved      → [`FieldState::Known`] carrying its evidence id.
pub fn resolve_field(raw: Option<FieldEvidence>, required: bool) -> FieldState {
    match raw {
        None => {
            if required {
                FieldState::Reject
            } else {
                FieldState::Unknown
            }
        }
        Some(fe) => match fe.parsed {
            Some(v) => FieldState::Known(Evidence {
                evidence_id: fe.evidence_id,
                value: v,
            }),
            None => FieldState::Incomplete,
        },
    }
}

// ---------------------------------------------------------------------------
// leaf: si_no_llm_fact  &  si_narrative_boundary (shared provenance types)
// ---------------------------------------------------------------------------

/// A value decoded from chain/simulation evidence — the only kind admissible
/// as fact.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChainEvidence {
    /// Slot the evidence was decoded at.
    pub slot: u64,
    /// The decoded integer value.
    pub value: i128,
}

/// Text produced by a model. Deliberately opaque; can never become fact.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ModelOutput {
    /// Model-produced text.
    pub text: String,
}

/// Where a narrative/research observation was captured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaptureProvenance {
    Social,
    Chart,
    Wallet,
    Label,
}

/// Time-horizon class of a narrative observation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HorizonClass {
    Immediate,
    Short,
    Long,
}

/// A research artifact — narrative/social/model-adjacent material. It carries
/// capture provenance and a horizon class, and there is intentionally **no**
/// function converting it into a [`FactValue`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResearchArtifact {
    /// Free-form captured note.
    pub note: String,
    /// Where it was captured.
    pub provenance: CaptureProvenance,
    /// Its horizon class.
    pub horizon: HorizonClass,
}

/// A provenance-tagged value at the admission boundary. The provenance lives in
/// the *type*, so a model/research value cannot be laundered into fact.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TaggedValue {
    /// Chain/decoded evidence — admissible.
    ChainEvidence(ChainEvidence),
    /// Model output — never admissible.
    Model(ModelOutput),
    /// Research artifact — never admissible.
    ResearchArtifact(ResearchArtifact),
}

/// A value that has passed the fact-admission boundary.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FactValue {
    /// Slot of origin.
    pub slot: u64,
    /// Admitted value.
    pub value: i128,
}

/// Reasons a tagged value was refused admission as fact.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FactError {
    /// The value was model-produced.
    ModelOutputRejected,
    /// The value was a research artifact.
    ResearchRejected,
}

/// Type-enforced admission boundary: only [`TaggedValue::ChainEvidence`] can
/// become a [`FactValue`]. Model and research values are always rejected.
pub fn admit_fact(v: TaggedValue) -> Result<FactValue, FactError> {
    match v {
        TaggedValue::ChainEvidence(c) => Ok(FactValue {
            slot: c.slot,
            value: c.value,
        }),
        TaggedValue::Model(_) => Err(FactError::ModelOutputRejected),
        TaggedValue::ResearchArtifact(_) => Err(FactError::ResearchRejected),
    }
}

// ---------------------------------------------------------------------------
// leaf: si_narrative_boundary
// ---------------------------------------------------------------------------

/// A captured narrative/social record. It carries its provenance and horizon
/// and can only ever land in a [`ResearchArtifact`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NarrativeRecord {
    /// Captured narrative text.
    pub text: String,
    /// Where it was captured.
    pub provenance: CaptureProvenance,
    /// Its horizon class.
    pub horizon: HorizonClass,
}

/// A narrative record always lands in a [`ResearchArtifact`]. Admission into
/// fact is a *separate*, gated step ([`admit_fact`]) which rejects research —
/// so there is no direct narrative→fact path.
pub fn narrative_to_research(n: NarrativeRecord) -> ResearchArtifact {
    ResearchArtifact {
        note: n.text,
        provenance: n.provenance,
        horizon: n.horizon,
    }
}

// ---------------------------------------------------------------------------
// leaf: si_failed_sell_state
// ---------------------------------------------------------------------------

/// Raw material for terminal classification of a position.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RawOutcome {
    /// Lamports spent acquiring the position (principal at risk).
    pub entry_lamports: u64,
    /// Lamports received from the sell, or `None` if no sell ever landed.
    pub exit_lamports: Option<u64>,
    /// Fixed cost (fees, priority) paid regardless of outcome.
    pub fixed_cost_lamports: u64,
    /// Whether a sell transaction actually landed.
    pub sell_landed: bool,
    /// Whether the position hit an inactivity timeout with no exit.
    pub inactivity_timeout: bool,
}

/// First-class terminal outcome of a position. Losses are representable and are
/// distinct from a zero-PnL close.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TerminalState {
    /// A completed round trip with signed net lamports.
    Closed { net_lamports: i128 },
    /// A sell that never landed.
    FailedSell,
    /// A position abandoned with no exit (inactivity timeout).
    TerminalLoss,
}

impl TerminalState {
    /// Net lamports for the outcome. For `FailedSell`/`TerminalLoss` this is the
    /// *full* loss — principal plus the fixed cost that was still paid.
    pub fn net_lamports(&self, o: &RawOutcome) -> i128 {
        match self {
            TerminalState::Closed { net_lamports } => *net_lamports,
            TerminalState::FailedSell | TerminalState::TerminalLoss => {
                -(o.entry_lamports as i128 + o.fixed_cost_lamports as i128)
            }
        }
    }
}

/// Classify a raw outcome into a terminal state. A failed sell is never a
/// zero-PnL close; an abandoned position is a terminal loss.
pub fn classify_terminal(outcome: &RawOutcome) -> TerminalState {
    if outcome.sell_landed {
        if let Some(exit) = outcome.exit_lamports {
            let net = exit as i128
                - outcome.entry_lamports as i128
                - outcome.fixed_cost_lamports as i128;
            return TerminalState::Closed { net_lamports: net };
        }
        // "landed" but no proceeds recorded is contradictory → treat as failed.
        return TerminalState::FailedSell;
    }
    if outcome.inactivity_timeout {
        return TerminalState::TerminalLoss;
    }
    TerminalState::FailedSell
}

// ---------------------------------------------------------------------------
// leaf: si_config_boot_guard
// ---------------------------------------------------------------------------

/// A committed boot configuration prior to validation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BootConfig {
    /// Whether the config was armed for live trading.
    pub live_armed: bool,
    /// Whether it was committed to source control.
    pub committed_to_source: bool,
    /// Shadow-mode flag.
    pub shadow: bool,
    /// Live-mode flag.
    pub live: bool,
}

/// A validated boot configuration. It has **no public constructor** other than
/// [`validate_boot_config`], so an unvalidated config can never masquerade as
/// validated.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ValidatedConfig {
    shadow: bool,
    live: bool,
}

impl ValidatedConfig {
    /// Validated shadow flag.
    pub fn shadow(&self) -> bool {
        self.shadow
    }
    /// Validated live flag.
    pub fn live(&self) -> bool {
        self.live
    }
}

/// Reasons a boot config is refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BootError {
    /// `live_armed=true` was committed to source.
    LiveArmedCommitted,
    /// Mutually contradictory flags (e.g. shadow && live).
    Contradictory,
}

/// Validate-then-construct: only a consistent, non-live-armed committed config
/// yields a [`ValidatedConfig`].
pub fn validate_boot_config(cfg: &BootConfig) -> Result<ValidatedConfig, BootError> {
    if cfg.live_armed && cfg.committed_to_source {
        return Err(BootError::LiveArmedCommitted);
    }
    if cfg.shadow && cfg.live {
        return Err(BootError::Contradictory);
    }
    Ok(ValidatedConfig {
        shadow: cfg.shadow,
        live: cfg.live,
    })
}

// ---------------------------------------------------------------------------
// leaf: si_signing_boundary
// ---------------------------------------------------------------------------

/// Opaque handle to a trading key. It intentionally does **not** implement
/// `Debug`, `Display`, `Serialize`, or `Clone`, and exposes no accessor to raw
/// key bytes — the key is non-exportable to the agent by construction.
pub struct KeyHandle {
    seed: u64,
}

impl KeyHandle {
    /// Create a handle from an opaque key seed. There is no inverse: no method
    /// returns the seed or any key bytes.
    pub fn new(seed: u64) -> Self {
        KeyHandle { seed }
    }

    /// Deterministically sign a digest. This is a boundary demonstration, not a
    /// real signature scheme — but crucially it never returns key material.
    fn sign_digest(&self, digest: u64) -> u64 {
        digest
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(self.seed)
            .rotate_left(17)
    }
}

/// A request to sign a transaction, carrying the target program and size.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SignRequest {
    /// Program the transaction targets.
    pub program_id: u64,
    /// Serialized transaction size in bytes.
    pub tx_size: u32,
    /// Digest to sign.
    pub digest: u64,
}

/// Signing policy: approved programs, a size cap, and the (opaque) key.
pub struct SignPolicy {
    /// Programs permitted to be signed for.
    pub approved_programs: Vec<u64>,
    /// Maximum permitted transaction size.
    pub max_tx_size: u32,
    /// The opaque signing key (non-exportable).
    pub key: KeyHandle,
}

/// A signed transaction. Carries only the signature, never key material.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SignedTx {
    /// The produced signature.
    pub signature: u64,
}

/// Reasons a signing request is refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SignError {
    /// The request violated policy (unapproved program or over-cap size).
    PolicyDenied,
}

/// Sign a request only if it satisfies policy. Policy is checked *before*
/// signing; the key is borrowed, never returned.
pub fn sign_through_policy(req: &SignRequest, policy: &SignPolicy) -> Result<SignedTx, SignError> {
    if !policy.approved_programs.contains(&req.program_id) {
        return Err(SignError::PolicyDenied);
    }
    if req.tx_size > policy.max_tx_size {
        return Err(SignError::PolicyDenied);
    }
    Ok(SignedTx {
        signature: policy.key.sign_digest(req.digest),
    })
}

// ---------------------------------------------------------------------------
// leaf: si_disconnect_safe
// ---------------------------------------------------------------------------

/// An explicit gap in a stream. An open gap has `to_seq == None`; the reducer
/// treats spans inside an open gap as Unknown, never zero.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GapMarker {
    /// Sequence number at which the gap opened.
    pub from_seq: u64,
    /// Sequence number at which the gap closed, or `None` while still open.
    pub to_seq: Option<u64>,
    /// Epoch during which the gap opened.
    pub epoch: u64,
}

/// Tracked state of a single observation stream.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StreamState {
    /// Current epoch. Reconnection increments this.
    pub epoch: u64,
    /// Last sequence number observed.
    pub last_seq: u64,
    /// Whether the stream is currently connected.
    pub connected: bool,
    /// All recorded gaps (open and closed).
    pub gaps: Vec<GapMarker>,
}

impl StreamState {
    /// A fresh, connected stream at epoch 0.
    pub fn new() -> Self {
        StreamState {
            epoch: 0,
            last_seq: 0,
            connected: true,
            gaps: Vec::new(),
        }
    }

    /// Record a reconnection at `at_seq`: this closes the currently-open gap and
    /// begins a **distinct epoch** — reconnection is never a silent continuation.
    pub fn reconnect(&mut self, at_seq: u64) {
        self.epoch = self.epoch.saturating_add(1);
        if let Some(g) = self.gaps.last_mut() {
            if g.to_seq.is_none() {
                g.to_seq = Some(at_seq);
            }
        }
        self.connected = true;
        self.last_seq = at_seq;
    }

    /// Whether `seq` falls inside an open gap (and is therefore Unknown, not
    /// interpolated).
    pub fn is_gapped(&self, seq: u64) -> bool {
        self.gaps
            .iter()
            .any(|g| g.to_seq.is_none() && seq >= g.from_seq)
    }
}

impl Default for StreamState {
    fn default() -> Self {
        Self::new()
    }
}

/// On disconnect, open an explicit [`GapMarker`] (`to_seq: None`). No state is
/// synthesized to fill the gap.
pub fn on_disconnect(stream: &mut StreamState, at_seq: u64) -> GapMarker {
    stream.connected = false;
    let marker = GapMarker {
        from_seq: at_seq,
        to_seq: None,
        epoch: stream.epoch,
    };
    stream.gaps.push(marker.clone());
    marker
}

// ---------------------------------------------------------------------------
// leaf: si_replay_provenance  &  si_source_contract (shared Observation type)
// ---------------------------------------------------------------------------

/// An ingested observation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Observation {
    /// Sequence number.
    pub seq: u64,
    /// Opaque decoded payload.
    pub payload: i128,
}

/// Whether observations are being sourced live or from a provider replay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceMode {
    Live,
    Replay,
}

/// Provenance of an observation. Set once at ingestion, never defaulted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provenance {
    /// Genuine original live discovery.
    OriginalLive,
    /// Provider replay — must never count as original live discovery.
    ProviderReplay,
}

/// An observation tagged with immutable provenance. The provenance field is
/// private and exposed only through [`ProvenancedObs::provenance`], so it cannot
/// be mutated after ingestion.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProvenancedObs {
    /// The underlying observation.
    pub obs: Observation,
    provenance: Provenance,
}

impl ProvenancedObs {
    /// The immutable provenance of this observation.
    pub fn provenance(&self) -> Provenance {
        self.provenance
    }
}

/// Tag an observation with provenance derived from the source mode.
pub fn tag_provenance(obs: &Observation, source_mode: SourceMode) -> ProvenancedObs {
    let provenance = match source_mode {
        SourceMode::Live => Provenance::OriginalLive,
        SourceMode::Replay => Provenance::ProviderReplay,
    };
    ProvenancedObs {
        obs: obs.clone(),
        provenance,
    }
}

// ---------------------------------------------------------------------------
// leaf: si_source_contract
// ---------------------------------------------------------------------------

/// Stable identifier for an observation source.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SourceId(pub u64);

/// The neutral observation-source contract. `StrategyRuntime` depends only on
/// this trait — never on a concrete adapter — so a successor source can be added
/// and the Jito adapter removed without touching the runtime.
pub trait ObservationSource {
    /// Poll for the next observation, or `None` if none is currently available.
    fn poll(&mut self) -> Option<Observation>;
    /// The source's stable identity.
    fn source_id(&self) -> SourceId;
}

/// A source that never produces observations.
pub struct NullSource {
    /// Its identity.
    pub id: u64,
}

impl ObservationSource for NullSource {
    fn poll(&mut self) -> Option<Observation> {
        None
    }
    fn source_id(&self) -> SourceId {
        SourceId(self.id)
    }
}

/// A deterministic fake source that drains a preloaded queue in order.
pub struct FakeSource {
    /// Its identity.
    pub id: u64,
    /// Observations to emit, front-first.
    pub queue: VecDeque<Observation>,
}

impl FakeSource {
    /// Build a fake source from an ordered list of observations.
    pub fn new(id: u64, items: Vec<Observation>) -> Self {
        FakeSource {
            id,
            queue: items.into_iter().collect(),
        }
    }
}

impl ObservationSource for FakeSource {
    fn poll(&mut self) -> Option<Observation> {
        self.queue.pop_front()
    }
    fn source_id(&self) -> SourceId {
        SourceId(self.id)
    }
}

/// The strategy runtime, depending solely on `Box<dyn ObservationSource>`. It
/// never names a concrete adapter type, so adapters are freely swappable.
pub struct StrategyRuntime {
    source: Box<dyn ObservationSource>,
    /// Count of observations processed so far.
    pub processed: u64,
}

impl StrategyRuntime {
    /// Construct a runtime around any observation source.
    pub fn new(source: Box<dyn ObservationSource>) -> Self {
        StrategyRuntime {
            source,
            processed: 0,
        }
    }

    /// Drive one poll; returns the polled observation (and counts it) or `None`.
    pub fn step(&mut self) -> Option<Observation> {
        match self.source.poll() {
            Some(obs) => {
                self.processed = self.processed.saturating_add(1);
                Some(obs)
            }
            None => None,
        }
    }

    /// Drain the source until it is exhausted, returning the count processed.
    pub fn drive_to_empty(&mut self) -> u64 {
        while self.step().is_some() {}
        self.processed
    }

    /// The current source's identity.
    pub fn source_id(&self) -> SourceId {
        self.source.source_id()
    }
}

// ---------------------------------------------------------------------------
// leaf: si_sell_simulation
// ---------------------------------------------------------------------------

/// A held position to be proven sellable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Position {
    /// Token amount held.
    pub token_amount: u64,
    /// Token mint.
    pub mint: u64,
}

/// Decoded market reserves and constructibility of a sell against them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DecodedMarket {
    /// Base (token) reserve.
    pub base_reserve: u64,
    /// Quote reserve.
    pub quote_reserve: u64,
    /// Whether a sell instruction can be constructed against this decoded state.
    pub constructible: bool,
}

/// Proof that a position is sellable, carrying the simulated out-amount.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SellProof {
    /// Simulated quote out-amount for selling the whole position.
    pub out_amount: u64,
}

/// Why a sell could not be proven.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SellUnprovable {
    /// The sell instruction could not be constructed.
    Unconstructible,
    /// Reserves/liquidity are insufficient for the position size.
    InsufficientLiquidity,
}

/// Prove a position is sellable by constructing and simulating a sell against
/// decoded reserves. Uses a constant-product (`x*y=k`) simulation in integer
/// math. Never assumes sellability.
pub fn prove_sellable(pos: &Position, state: &DecodedMarket) -> Result<SellProof, SellUnprovable> {
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

    // out = quote * amount / (base + amount)  (constant product, no fees)
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
    // out <= quote <= u64::MAX, so this conversion cannot overflow.
    Ok(SellProof {
        out_amount: out as u64,
    })
}

// ---------------------------------------------------------------------------
// leaf: si_failure_taxonomy
// ---------------------------------------------------------------------------

/// A decoded on-chain program error.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DecodedProgramError {
    /// Slippage tolerance exceeded.
    SlippageExceeded,
    /// Price moved between build and land.
    PriceMoved,
    /// Instruction was malformed.
    MalformedInstruction,
    /// An account did not match what the instruction expected.
    AccountMismatch,
    /// Anything not recognised, carrying its raw code.
    Unknown(u32),
}

/// The failure taxonomy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FailureClass {
    /// Retryable within policy (market moved under us).
    Transient,
    /// A construction defect — triggers builder quarantine, never a silent
    /// capital retry.
    Construction,
    /// Unrecognised — treated conservatively (not auto-retried).
    Unknown,
}

impl FailureClass {
    /// Whether this class may be retried *with capital*. Only `Transient` may.
    pub fn retryable_with_capital(&self) -> bool {
        matches!(self, FailureClass::Transient)
    }
    /// Whether this class triggers builder quarantine.
    pub fn triggers_quarantine(&self) -> bool {
        matches!(self, FailureClass::Construction)
    }
}

/// Classify a decoded program error into the failure taxonomy.
pub fn classify_failure(err: &DecodedProgramError) -> FailureClass {
    match err {
        DecodedProgramError::SlippageExceeded | DecodedProgramError::PriceMoved => {
            FailureClass::Transient
        }
        DecodedProgramError::MalformedInstruction | DecodedProgramError::AccountMismatch => {
            FailureClass::Construction
        }
        DecodedProgramError::Unknown(_) => FailureClass::Unknown,
    }
}

// ---------------------------------------------------------------------------
// leaf: si_no_copy_trade
// ---------------------------------------------------------------------------

/// The kind of an external observation that might tempt a copy trade.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SignalKind {
    Wallet,
    Social,
    Chart,
    Label,
    Ranking,
}

/// An external signal. It carries only the candidate token — deliberately **no
/// source-wallet field**, so mirroring a specific wallet's trade is
/// unrepresentable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExternalSignal {
    /// What kind of signal this is.
    pub kind: SignalKind,
    /// The candidate token mint.
    pub token_mint: u64,
}

/// The ordered stages of the deterministic pipeline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Feature,
    Liquidity,
    Risk,
    Economic,
    Sellability,
    Signing,
}

/// The deterministic pipeline's per-stage outcome flags.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeterministicPipeline {
    pub feature_ok: bool,
    pub liquidity_ok: bool,
    pub risk_ok: bool,
    pub economic_ok: bool,
    pub sellability_ok: bool,
    pub signing_ok: bool,
}

/// An order. It has **no public constructor** other than [`to_order`], and
/// carries no source-wallet field to mirror.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Order {
    token_mint: u64,
}

impl Order {
    /// The token this order targets.
    pub fn token_mint(&self) -> u64 {
        self.token_mint
    }
}

/// The pipeline blocked at a specific stage.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Blocked {
    /// The stage that blocked the order.
    pub stage: Stage,
}

/// The only path from an external signal to an order: pass every deterministic
/// stage, in order. Skipping any stage yields [`Blocked`]; there is no bypass
/// constructor for [`Order`].
pub fn to_order(signal: ExternalSignal, pipeline: &DeterministicPipeline) -> Result<Order, Blocked> {
    let stages = [
        (Stage::Feature, pipeline.feature_ok),
        (Stage::Liquidity, pipeline.liquidity_ok),
        (Stage::Risk, pipeline.risk_ok),
        (Stage::Economic, pipeline.economic_ok),
        (Stage::Sellability, pipeline.sellability_ok),
        (Stage::Signing, pipeline.signing_ok),
    ];
    for (stage, ok) in stages {
        if !ok {
            return Err(Blocked { stage });
        }
    }
    Ok(Order {
        token_mint: signal.token_mint,
    })
}

// ---------------------------------------------------------------------------
// leaf: si_smart_money_gate
// ---------------------------------------------------------------------------

/// Evidence about a wallet under smart-money consideration.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WalletEvidence {
    /// Whether the PnL is realized (not merely unrealized/mark-to-market).
    pub realized: bool,
    /// Whether the PnL came from self-dealing (wash between own wallets).
    pub self_dealing: bool,
    /// Whether the PnL is against external counterparties.
    pub external_counterparty: bool,
    /// Whether the wallet passed family (sybil/relatedness) screening.
    pub family_screened: bool,
    /// Whether the record passed the luck filter.
    pub luck_filtered: bool,
    /// Whether the lagged shadow-follow PnL is positive.
    pub lagged_shadow_positive: bool,
    /// Whether the wallet is publicly legible (already front-run by the crowd).
    pub publicly_legible: bool,
    /// Whether this cohort is explicitly inverting (fade) rather than follow.
    pub inverting_cohort: bool,
}

/// Smart-money classification.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SmartMoneyClass {
    /// Passed the full gate chain.
    Qualified,
    /// Failed some gate (default-safe).
    Unqualified,
    /// A demoted/fade cohort whose signal should *lower* score.
    Inverting,
}

/// Classify a wallet. The gate chain is: realized → not self-dealing → external
/// counterparty → family-screened → luck-filtered → lagged-shadow-positive.
/// Publicly-legible wallets carry PUBLIC_BURNED and are Unqualified by default.
pub fn classify_wallet(w: &WalletEvidence) -> SmartMoneyClass {
    // Publicly legible ⇒ burned until re-proven.
    if w.publicly_legible {
        return SmartMoneyClass::Unqualified;
    }
    // Raw unrealized or self-dealing PnL can never qualify.
    if !w.realized || w.self_dealing {
        return SmartMoneyClass::Unqualified;
    }
    // Full gate chain.
    let passes_gate = w.external_counterparty
        && w.family_screened
        && w.luck_filtered
        && w.lagged_shadow_positive;
    if !passes_gate {
        return SmartMoneyClass::Unqualified;
    }
    // A gate-passing but explicitly-inverting cohort fades rather than follows.
    if w.inverting_cohort {
        SmartMoneyClass::Inverting
    } else {
        SmartMoneyClass::Qualified
    }
}

// ---------------------------------------------------------------------------
// leaf: si_signal_scoring_only
// ---------------------------------------------------------------------------

/// Scoring inputs into the downstream decision. Smart-money can only reweight
/// these — it can never construct an [`Order`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScoringInputs {
    /// The running score in signed integer units.
    pub base_score: i64,
    /// Whether smart-money has already been applied (enforces single application).
    pub smart_money_applied: bool,
    /// The delta contributed by smart-money (0 until applied).
    pub smart_money_delta: i64,
}

/// Magnitude of the smart-money score adjustment, in score units.
pub const SMART_MONEY_WEIGHT: i64 = 100;

/// Apply a smart-money class to scoring inputs. Returns modified
/// [`ScoringInputs`] — never an order. The contribution enters scoring exactly
/// once: a second call is a no-op. `Qualified` raises, `Unqualified` is neutral,
/// `Inverting` lowers (fast-kill), and none produces a trade.
pub fn apply_smart_money(base: ScoringInputs, sm: SmartMoneyClass) -> ScoringInputs {
    if base.smart_money_applied {
        // Already contributed once — never apply twice.
        return base;
    }
    let delta = match sm {
        SmartMoneyClass::Qualified => SMART_MONEY_WEIGHT,
        SmartMoneyClass::Unqualified => 0,
        SmartMoneyClass::Inverting => -SMART_MONEY_WEIGHT,
    };
    ScoringInputs {
        base_score: base.base_score.saturating_add(delta),
        smart_money_applied: true,
        smart_money_delta: delta,
    }
}

// ---------------------------------------------------------------------------
// leaf: si_quote_mint_param
// ---------------------------------------------------------------------------

/// The market's quote mint, carrying its decoded decimals. There is no
/// hardcoded "9 decimals / SOL" assumption anywhere: decimals always come from
/// here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuoteMint {
    /// SOL quote with decoded decimals (canonically 9).
    Sol { decimals: u32 },
    /// USDC quote with decoded decimals (canonically 6).
    Usdc { decimals: u32 },
    /// The quote mint could not be decoded — refuse rather than assume.
    Undecoded,
}

impl QuoteMint {
    /// Decoded decimals, or `None` if undecoded.
    pub fn decimals(&self) -> Option<u32> {
        match self {
            QuoteMint::Sol { decimals } | QuoteMint::Usdc { decimals } => Some(*decimals),
            QuoteMint::Undecoded => None,
        }
    }
}

/// A market's fee and fixed-cost parameters.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Market {
    /// Trading fee in basis points, charged on each leg.
    pub fee_bps: u32,
    /// Fixed cost per round trip expressed in *whole* quote tokens.
    pub fixed_cost_whole: u64,
}

/// Round-trip cost for `size` (quote base units traded per leg), expressed in
/// the market's quote base units. Quote decimals come entirely from `quote`; an
/// undecoded quote yields `None` (refuse, never assume SOL). All arithmetic is
/// checked.
pub fn round_trip_cost_quote(size: u64, mkt: &Market, quote: QuoteMint) -> Option<u64> {
    let decimals = quote.decimals()?; // Undecoded ⇒ None
    let scale = 10u128.checked_pow(decimals)?;

    let size = size as u128;
    // Fee on each of the two legs (buy + sell).
    let fee_one_leg = size.checked_mul(mkt.fee_bps as u128)? / 10_000u128;
    let fee_round_trip = fee_one_leg.checked_mul(2)?;

    // Fixed cost in whole tokens → base units via decoded decimals.
    let fixed = (mkt.fixed_cost_whole as u128).checked_mul(scale)?;

    let total = fee_round_trip.checked_add(fixed)?;
    u64::try_from(total).ok()
}

// ---------------------------------------------------------------------------
// leaf: si_authenticity_once
// ---------------------------------------------------------------------------

/// Flow-authenticity confidence, in basis points (10000 = fully authentic).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AuthConfidence {
    /// Confidence in bps (0..=10000).
    pub bps: u32,
}

/// The single channel through which authenticity may enter the sizing chain.
/// The two variants are mutually exclusive, so applying authenticity twice in
/// one call is unrepresentable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthMode {
    /// Fold authenticity into the edge estimate; leave the haircut at 1.0.
    ThroughEdge,
    /// Apply an explicit size haircut; leave the edge unmodified.
    Haircut,
}

/// Sizing inputs. The two flags record which channel (if any) has already taken
/// authenticity, so a double-application is detectable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SizeInputs {
    /// Edge estimate in bps.
    pub edge_bps: i64,
    /// Size haircut factor in bps (10000 = 1.0, no haircut).
    pub haircut_bps: u32,
    /// Whether authenticity was already folded into the edge.
    pub edge_auth_adjusted: bool,
    /// Whether an authenticity haircut was already applied.
    pub haircut_applied: bool,
}

impl SizeInputs {
    /// A fresh set of sizing inputs with no authenticity applied and no haircut.
    pub fn fresh(edge_bps: i64) -> Self {
        SizeInputs {
            edge_bps,
            haircut_bps: 10_000,
            edge_auth_adjusted: false,
            haircut_applied: false,
        }
    }
}

/// Apply flow-authenticity confidence exactly once, through the chosen channel.
///
/// If authenticity has *already* entered the chain through either channel, the
/// second application is detected and **rejected** (the inputs are returned
/// unchanged) — so confidence can never be counted twice.
pub fn apply_authenticity(size_in: SizeInputs, auth: AuthConfidence, mode: AuthMode) -> SizeInputs {
    // Detect and reject a double-application.
    if size_in.edge_auth_adjusted || size_in.haircut_applied {
        return size_in;
    }
    match mode {
        AuthMode::ThroughEdge => {
            // Fold authenticity into the edge; haircut stays neutral (1.0).
            let adj = (size_in.edge_bps as i128 * auth.bps as i128) / 10_000i128;
            SizeInputs {
                edge_bps: adj as i64,
                haircut_bps: 10_000,
                edge_auth_adjusted: true,
                haircut_applied: false,
            }
        }
        AuthMode::Haircut => {
            // Apply an explicit haircut; edge is left untouched.
            SizeInputs {
                edge_bps: size_in.edge_bps,
                haircut_bps: auth.bps,
                edge_auth_adjusted: false,
                haircut_applied: true,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// leaf: si_lpi_screen
// ---------------------------------------------------------------------------

/// Market phase, used to normalize the LPI expectation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MarketPhase {
    Early,
    Mid,
    Late,
}

/// A flow observation window.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FlowWindow {
    /// Net quote inflow over the window, in quote base units.
    pub net_inflow: u64,
    /// Observed price appreciation over the window, in bps.
    pub appreciation_bps: u64,
    /// Age of the detection in seconds (drives covariate decay).
    pub age_secs: u64,
}

/// The LPI verdict. Both variants expose the threshold margin so over-rejection
/// is measurable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LpiVerdict {
    /// Appreciation exceeds what inflow at this depth/phase supports.
    Fabricated {
        /// Decaying extraction-risk covariate contributed by this detection.
        covariate_bps: u64,
        /// `observed - supported` (positive ⇒ beyond support).
        threshold_margin: i128,
    },
    /// Appreciation is within supportable bounds.
    Clean {
        /// `observed - supported` (non-positive when clean).
        threshold_margin: i128,
    },
}

/// Half-life of the LPI extraction-risk covariate, in seconds.
pub const LPI_COVARIATE_HALF_LIFE_SECS: u64 = 3_600;

/// Decay `base` by halving once per elapsed half-life. Deterministic integer
/// decay: non-zero at detection time, decaying over time, not permanent.
pub fn lpi_decayed_covariate(base: u128, age_secs: u64) -> u64 {
    let halvings = (age_secs / LPI_COVARIATE_HALF_LIFE_SECS).min(127) as u32;
    let v = base >> halvings; // halve per elapsed half-life
    u64::try_from(v.min(u64::MAX as u128)).unwrap_or(u64::MAX)
}

/// Depth- and phase-normalized appreciation-per-net-inflow anomaly screen.
///
/// The appreciation that a given net inflow can *legitimately* cause is inversely
/// proportional to market depth (deeper markets move less per unit inflow) and
/// scaled by phase (early markets move more per unit inflow). Appreciation
/// exceeding that support is flagged [`LpiVerdict::Fabricated`] and contributes a
/// decaying extraction-risk covariate.
pub fn lpi_score(flow: &FlowWindow, depth: u64, phase: MarketPhase) -> LpiVerdict {
    // Phase multiplier (numerator/denominator): earlier phases tolerate more
    // move per unit inflow before being anomalous.
    let (pn, pd): (u128, u128) = match phase {
        MarketPhase::Early => (3, 1),
        MarketPhase::Mid => (2, 1),
        MarketPhase::Late => (1, 1),
    };

    let depth = (depth.max(1)) as u128; // avoid div-by-zero deterministically
    let net = flow.net_inflow as u128;

    // supported appreciation (bps) ≈ net_inflow / depth, scaled by phase.
    // (10_000 keeps the ratio in bps units.)
    let supported_bps = net
        .saturating_mul(10_000)
        .saturating_mul(pn)
        / depth.saturating_mul(pd);

    let observed = flow.appreciation_bps as u128;
    let margin: i128 = observed as i128 - supported_bps as i128;

    if observed > supported_bps {
        let excess = observed - supported_bps;
        let covariate_bps = lpi_decayed_covariate(excess, flow.age_secs);
        LpiVerdict::Fabricated {
            covariate_bps,
            threshold_margin: margin,
        }
    } else {
        LpiVerdict::Clean {
            threshold_margin: margin,
        }
    }
}

// ---------------------------------------------------------------------------
// leaf: si_bounded_evict
// ---------------------------------------------------------------------------

/// A capacity-bounded FIFO ring buffer. It never grows beyond its capacity;
/// pushing at capacity evicts the oldest element deterministically.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BoundedRing<T> {
    cap: usize,
    buf: VecDeque<T>,
}

impl<T> BoundedRing<T> {
    /// A ring with capacity `cap` (clamped to at least 1).
    pub fn new(cap: usize) -> Self {
        BoundedRing {
            cap: cap.max(1),
            buf: VecDeque::new(),
        }
    }
    /// Current number of elements held.
    pub fn len(&self) -> usize {
        self.buf.len()
    }
    /// Whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
    /// The ring's fixed capacity.
    pub fn capacity(&self) -> usize {
        self.cap
    }
    /// Immutable view of the oldest element.
    pub fn front(&self) -> Option<&T> {
        self.buf.front()
    }
    /// Immutable view of the newest element.
    pub fn back(&self) -> Option<&T> {
        self.buf.back()
    }
}

/// Admit `item`, evicting the oldest element (FIFO) and returning it as `Some`
/// if the ring was at capacity; otherwise grow by one and return `None`. The
/// length never exceeds the capacity and eviction order is deterministic.
pub fn admit_with_eviction<T>(buf: &mut BoundedRing<T>, item: T) -> Option<T> {
    let evicted = if buf.buf.len() >= buf.cap {
        buf.buf.pop_front()
    } else {
        None
    };
    buf.buf.push_back(item);
    evicted
}

// ---------------------------------------------------------------------------
// leaf: si_backpressure_stale
// ---------------------------------------------------------------------------

/// The backpressure admission decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admission {
    /// The observation is fresh enough to process.
    Accept,
    /// The observation is too old — shed it rather than fall behind.
    RejectStale,
}

/// Admit or shed an observation based purely on its age. An observation strictly
/// older than `max_age_ns` is rejected; the boundary (age exactly `max_age_ns`)
/// is Accept. No blocking, no queue — a pure, total comparison. A timestamp in
/// the future (`obs_ts_ns > now_ns`) saturates to age 0 and is accepted.
pub fn admit_or_stale(obs_ts_ns: u64, now_ns: u64, max_age_ns: u64) -> Admission {
    let age = now_ns.saturating_sub(obs_ts_ns);
    if age > max_age_ns {
        Admission::RejectStale
    } else {
        Admission::Accept
    }
}
