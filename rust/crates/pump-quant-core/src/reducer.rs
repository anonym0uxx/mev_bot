//! Deterministic event-sourced reducer core for the pump-quant trading bot.
//!
//! Canonical decoded events are applied by *pure* transition functions to versioned
//! state. Identical event sequences produce bit-identical state and decisions
//! (constitution criterion 12); live, shadow, and replay share this exact code
//! (criterion 13).
//!
//! Design constraints enforced throughout this module:
//! - No clocks, no IO, no RNG, no ambient reads in any transition function.
//! - No `HashMap` iteration-order dependence: keyed collections are iterated in a
//!   deterministic (sorted) order wherever the order affects output.
//! - No `f32`/`f64` in outcome-controlling state (constitution §22). Money is
//!   `u64`/`u128` lamports; ratios are basis points; all math is integer/fixed-point.
//!   The single, documented exception is [`quantize_feature`], a boundary adapter
//!   that lives *outside* the hot path.
//! - Overflow is explicit (checked / saturating), never silent.
//! - State is snapshot-hashable in a stable order for replay-parity assertion.

// ============================================================================
// Common domain primitives
// ============================================================================

/// Trade direction. Encoded as a single byte in the canonical hash serialization.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// Quote-in, base-out (buying the base asset).
    Buy,
    /// Base-in, quote-out (selling the base asset).
    Sell,
}

impl Side {
    /// Canonical one-byte tag used for hashing.
    #[inline]
    pub fn tag(self) -> u8 {
        match self {
            Side::Buy => 0,
            Side::Sell => 1,
        }
    }
}

/// Whether a market's state has been reconstructed from a gap-free event stream.
///
/// A detected gap sets [`Completeness::Incomplete`]; no value is ever invented to
/// fill a gap.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Completeness {
    /// Every event up to the current watermark was observed.
    Complete,
    /// At least one gap was detected; derived features must be treated as unsafe.
    Incomplete,
}

impl Completeness {
    /// Canonical one-byte tag used for hashing.
    #[inline]
    pub fn tag(self) -> u8 {
        match self {
            Completeness::Complete => 1,
            Completeness::Incomplete => 0,
        }
    }
}

// ============================================================================
// Leaf: rd_event_key — canonical event identity & ordering key
// ============================================================================

/// Total-order identity of an on-chain event.
///
/// Ordering and equality derive purely from the event's on-chain position
/// `(slot, tx_index, inner_index)` — never from arrival time or delivery source.
/// The same logical event delivered by two different sources therefore compares
/// equal. `source_seq` is telemetry only and is deliberately *not* part of this
/// key (see [`CanonEvent`]).
///
/// `Copy`, 16 bytes (≤ 24), no allocation.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventKey {
    /// Slot (block) the event landed in — most significant ordering dimension.
    pub slot: u64,
    /// Transaction index within the slot.
    pub tx_index: u32,
    /// Inner-instruction index within the transaction.
    pub inner_index: u16,
}

impl EventKey {
    /// Test/constructor helper: build a key from raw on-chain coordinates.
    pub fn test(slot: u64, tx_index: u32, inner_index: u16) -> Self {
        Self {
            slot,
            tx_index,
            inner_index,
        }
    }
}

/// A fully decoded, canonical swap event ready to be admitted and applied.
///
/// The reducer treats the decoded post-state (`post_reserve_*`) as authoritative.
/// `source_seq` records which delivery produced this instance for telemetry, but
/// it is excluded from identity/ordering ([`EventKey`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CanonEvent {
    /// Slot the event landed in.
    pub slot: u64,
    /// Transaction index within the slot.
    pub tx_index: u32,
    /// Inner-instruction index within the transaction.
    pub inner_index: u16,
    /// Delivery sequence number — telemetry only, excluded from [`EventKey`].
    pub source_seq: u64,
    /// Trade direction.
    pub side: Side,
    /// Quote leg size in lamports.
    pub quote_lamports: u64,
    /// Base leg size in base-token base units.
    pub base_lamports: u64,
    /// Authoritative post-trade base reserve.
    pub post_reserve_base: u128,
    /// Authoritative post-trade quote reserve.
    pub post_reserve_quote: u128,
    /// On-chain block time in nanoseconds (deterministic, from the event — not a clock).
    pub ts_ns: u64,
}

impl CanonEvent {
    /// Test/constructor helper producing a deterministic event from its coordinates.
    ///
    /// Every derived field is a pure function of the arguments so that identical
    /// coordinates always yield identical events.
    pub fn test(slot: u64, tx_index: u32, inner_index: u16, source_seq: u64) -> Self {
        let side = if (tx_index.wrapping_add(inner_index as u32)).is_multiple_of(2) {
            Side::Buy
        } else {
            Side::Sell
        };
        Self {
            slot,
            tx_index,
            inner_index,
            source_seq,
            side,
            quote_lamports: 1_000_000,
            base_lamports: 500_000,
            post_reserve_base: 1_000_000_000u128 + slot as u128,
            post_reserve_quote: 2_000_000_000u128 + slot as u128,
            ts_ns: 1_700_000_000_000_000_000u64 + slot,
        }
    }
}

/// Canonical event identity and ordering key for dedup and sequencing.
///
/// Two distinct on-chain events never share a key; the same event from two
/// sources shares one. Pure, `Copy`-returning, no allocation.
#[inline]
pub fn event_key(ev: &CanonEvent) -> EventKey {
    EventKey {
        slot: ev.slot,
        tx_index: ev.tx_index,
        inner_index: ev.inner_index,
    }
}

// ============================================================================
// Leaf: rd_dedup_gap — deterministic dedup + gap detection
// ============================================================================

/// Fixed capacity of the recent-key ring. Power of two so eviction is a mask.
pub const RECENT_CAP: usize = 1024;

/// Outcome of admitting a key into the sequenced event stream.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Admit {
    /// Contiguous, first-seen event — apply it.
    Apply,
    /// Already applied (present in the recent window) — drop it.
    Duplicate,
    /// Below the watermark / arrived too late to be a valid fill — reject.
    Regression,
    /// Forward jump: `missed_count` slots were skipped. Apply this event but mark
    /// the affected market [`Completeness::Incomplete`] — never silently `Apply`.
    GapThenApply(u64),
}

/// Bounded, deterministic sequencing state for one event stream.
///
/// Memory is fixed: the recent-key window is a fixed-capacity ring with
/// deterministic (oldest-first) eviction. No allocation occurs after construction.
#[derive(Clone)]
pub struct SeqState {
    /// Whether any event has been admitted yet (no watermark exists before the first).
    initialized: bool,
    /// Slots strictly below this are unconditionally [`Admit::Regression`].
    low_watermark: u64,
    /// The next slot expected to be contiguous.
    next_expected: u64,
    /// Fixed ring of recently applied keys, for duplicate detection.
    recent: [Option<EventKey>; RECENT_CAP],
    /// Write cursor into `recent`.
    head: usize,
}

impl Default for SeqState {
    fn default() -> Self {
        Self::new()
    }
}

impl SeqState {
    /// Fresh state with an empty recent window and no established watermark.
    pub fn new() -> Self {
        Self {
            initialized: false,
            low_watermark: 0,
            next_expected: 0,
            recent: [None; RECENT_CAP],
            head: 0,
        }
    }

    /// Deterministic membership test over the recent window.
    #[inline]
    fn seen(&self, k: EventKey) -> bool {
        self.recent.iter().flatten().any(|&r| r == k)
    }

    /// Record a key, evicting the oldest slot in the ring if full.
    #[inline]
    fn remember(&mut self, k: EventKey) {
        self.recent[self.head] = Some(k);
        self.head = (self.head + 1) & (RECENT_CAP - 1);
    }

    /// Advance the watermarks after admitting a key at `slot`.
    #[inline]
    fn advance(&mut self, slot: u64) {
        self.next_expected = slot.saturating_add(1);
        self.low_watermark = self.next_expected.saturating_sub(RECENT_CAP as u64);
    }
}

/// Deterministic dedup + gap detection over the stream watermark.
///
/// Sequencing is keyed by slot (the coarse ordering dimension the gap count is
/// measured in). The recent ring keys off the full [`EventKey`] so duplicates
/// within a slot are still recognized.
pub fn admit(seq: &mut SeqState, key: EventKey) -> Admit {
    let slot = key.slot;

    // No watermark exists before the very first event: it is not a "jump".
    if !seq.initialized {
        seq.initialized = true;
        seq.remember(key);
        seq.advance(slot);
        return Admit::Apply;
    }

    // Below the low watermark: unconditionally a regression.
    if slot < seq.low_watermark {
        return Admit::Regression;
    }

    // Behind the expectation but within the window: a known duplicate, or a
    // late arrival we can no longer place — reject rather than reorder.
    if slot < seq.next_expected {
        return if seq.seen(key) {
            Admit::Duplicate
        } else {
            Admit::Regression
        };
    }

    // Exactly the expected slot: contiguous apply.
    if slot == seq.next_expected {
        seq.remember(key);
        seq.advance(slot);
        return Admit::Apply;
    }

    // Strictly ahead: forward jump. Count the skipped slots so the caller marks
    // the market incomplete, then apply.
    let missed = slot - seq.next_expected;
    seq.remember(key);
    seq.advance(slot);
    Admit::GapThenApply(missed)
}

// ============================================================================
// Leaf: rd_apply — pure market-state transition
// ============================================================================

/// Fixed-point integer `a * num / den` for signed decay factors, saturating on
/// overflow. In production this delegates to the shared fixedpoint component;
/// it is inlined here so the reducer has zero cross-component runtime deps.
#[inline]
fn mul_div_i128(a: i128, num: i128, den: i128) -> i128 {
    debug_assert!(den != 0);
    match a.checked_mul(num) {
        Some(p) => p / den,
        None => (a / den).saturating_mul(num),
    }
}

/// Decay numerator (per-event geometric decay factor `num/den`).
const DECAY_NUM: i128 = 15;
/// Decay denominator.
const DECAY_DEN: i128 = 16;

/// Fixed-size decayed flow summary. No allocation, no floats.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FlowAgg {
    /// Cumulative buy-side quote volume in lamports (saturating).
    pub buy_quote: u128,
    /// Cumulative sell-side quote volume in lamports (saturating).
    pub sell_quote: u128,
    /// Number of trades folded in (saturating).
    pub count: u32,
    /// Timestamp of the most recent trade (from the event, not a clock).
    pub last_ts_ns: u64,
    /// Geometrically decayed signed net flow (buy positive), fixed-point lamports.
    pub decayed_net: i128,
}

impl Default for FlowAgg {
    fn default() -> Self {
        Self::new()
    }
}

impl FlowAgg {
    /// Empty aggregate.
    pub fn new() -> Self {
        Self {
            buy_quote: 0,
            sell_quote: 0,
            count: 0,
            last_ts_ns: 0,
            decayed_net: 0,
        }
    }

    /// Fold one trade into the summary. Pure integer/fixed-point update.
    #[inline]
    pub fn apply(&mut self, side: Side, quote_lamports: u64, ts_ns: u64) {
        // Geometric decay of the running net before folding the new trade.
        self.decayed_net = mul_div_i128(self.decayed_net, DECAY_NUM, DECAY_DEN);
        let q = quote_lamports as u128;
        match side {
            Side::Buy => {
                self.buy_quote = self.buy_quote.saturating_add(q);
                self.decayed_net = self.decayed_net.saturating_add(quote_lamports as i128);
            }
            Side::Sell => {
                self.sell_quote = self.sell_quote.saturating_add(q);
                self.decayed_net = self.decayed_net.saturating_sub(quote_lamports as i128);
            }
        }
        self.count = self.count.saturating_add(1);
        self.last_ts_ns = ts_ns;
    }

    /// Append canonical little-endian field bytes to a hasher.
    fn write_canon(&self, h: &mut Hasher) {
        h.update(&self.buy_quote.to_le_bytes());
        h.update(&self.sell_quote.to_le_bytes());
        h.update(&self.count.to_le_bytes());
        h.update(&self.last_ts_ns.to_le_bytes());
        h.update(&self.decayed_net.to_le_bytes());
    }
}

/// Snapshot of the most recent trade in a market. Fixed-size, all integer.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct LastTrade {
    /// Slot of the last trade.
    pub slot: u64,
    /// Transaction index of the last trade.
    pub tx_index: u32,
    /// Direction of the last trade.
    pub side: Side,
    /// Quote size of the last trade in lamports.
    pub quote_lamports: u64,
    /// Timestamp of the last trade (from the event).
    pub ts_ns: u64,
}

impl Default for LastTrade {
    fn default() -> Self {
        Self {
            slot: 0,
            tx_index: 0,
            side: Side::Buy,
            quote_lamports: 0,
            ts_ns: 0,
        }
    }
}

impl LastTrade {
    /// Build a last-trade snapshot from a canonical event.
    #[inline]
    pub fn from_event(ev: &CanonEvent) -> Self {
        Self {
            slot: ev.slot,
            tx_index: ev.tx_index,
            side: ev.side,
            quote_lamports: ev.quote_lamports,
            ts_ns: ev.ts_ns,
        }
    }

    /// Append canonical little-endian field bytes to a hasher.
    fn write_canon(&self, h: &mut Hasher) {
        h.update(&self.slot.to_le_bytes());
        h.update(&self.tx_index.to_le_bytes());
        h.update(&[self.side.tag()]);
        h.update(&self.quote_lamports.to_le_bytes());
        h.update(&self.ts_ns.to_le_bytes());
    }
}

/// Fixed-size, allocation-free per-market state. All fields are integer.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct MarketState {
    /// Base-token reserve (authoritative decoded post-state).
    pub reserve_base: u128,
    /// Quote-token reserve (authoritative decoded post-state).
    pub reserve_quote: u128,
    /// Decayed flow summary.
    pub flow: FlowAgg,
    /// Most recent trade snapshot.
    pub last: LastTrade,
    /// Whether this market's history is gap-free.
    pub completeness: Completeness,
}

impl MarketState {
    /// A deterministic default market for tests.
    pub fn test() -> Self {
        Self {
            reserve_base: 1_000_000_000,
            reserve_quote: 2_000_000_000,
            flow: FlowAgg::new(),
            last: LastTrade::default(),
            completeness: Completeness::Complete,
        }
    }

    /// A deterministic market parameterized by `i`, distinct per `i`.
    pub fn test_with(i: u64) -> Self {
        let mut m = Self::test();
        m.reserve_base = 1_000_000_000u128 + i as u128;
        m.reserve_quote = 2_000_000_000u128 + (i as u128).saturating_mul(3);
        m.last.slot = i;
        m
    }

    /// Append canonical little-endian field bytes to a hasher.
    fn write_canon(&self, h: &mut Hasher) {
        h.update(&self.reserve_base.to_le_bytes());
        h.update(&self.reserve_quote.to_le_bytes());
        self.flow.write_canon(h);
        self.last.write_canon(h);
        h.update(&[self.completeness.tag()]);
    }
}

/// Pure market-state transition for one admitted canonical swap.
///
/// Referentially transparent: the input `state` is never mutated, and applying
/// the same event to the same state always yields the same result. No allocation.
pub fn apply(state: &MarketState, ev: &CanonEvent) -> MarketState {
    let mut s = *state; // MarketState: Copy — no allocation, input untouched.
                        // Decoded post-state is authoritative for reserves.
    s.reserve_base = ev.post_reserve_base;
    s.reserve_quote = ev.post_reserve_quote;
    // Fold the trade into the fixed-size decayed summary.
    s.flow.apply(ev.side, ev.quote_lamports, ev.ts_ns);
    s.last = LastTrade::from_event(ev);
    s
}

// ============================================================================
// Leaf: rd_snapshot_hash — order-stable state hash
// ============================================================================

/// Schema version byte prefixed to every snapshot hash. Bump on any change to
/// the canonical field serialization.
pub const SCHEMA_V: u8 = 1;

/// Deterministic, order-independent 256-bit hasher.
///
/// Four independent FNV-1a-style 64-bit lanes are mixed per byte and concatenated
/// little-endian into 32 output bytes. Pure and free of any ambient state, so the
/// same byte stream always yields the same digest within and across processes.
struct Hasher {
    lanes: [u64; 4],
}

impl Hasher {
    /// Four distinct FNV offset-basis seeds.
    const SEEDS: [u64; 4] = [
        0xcbf2_9ce4_8422_2325,
        0x1000_0000_0000_01b3,
        0x9e37_79b9_7f4a_7c15,
        0xff51_afd7_ed55_8ccd,
    ];
    /// Four distinct odd multiplicative primes.
    const PRIMES: [u64; 4] = [
        0x0000_0100_0000_01b3,
        0x0000_0100_0000_01c9,
        0x0000_0100_0000_01d3,
        0x0000_0100_0000_01e7,
    ];

    #[inline]
    fn new() -> Self {
        Self { lanes: Self::SEEDS }
    }

    #[inline]
    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            for lane in 0..4 {
                self.lanes[lane] ^= b as u64;
                self.lanes[lane] = self.lanes[lane].wrapping_mul(Self::PRIMES[lane]);
            }
        }
    }

    #[inline]
    fn finalize(self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for lane in 0..4 {
            // Final avalanche to spread low-bit structure.
            let mut x = self.lanes[lane];
            x ^= x >> 33;
            x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
            x ^= x >> 33;
            out[lane * 8..lane * 8 + 8].copy_from_slice(&x.to_le_bytes());
        }
        out
    }
}

/// Default hard cap on the number of concurrently-tracked markets. A fixed bound so
/// the world state cannot grow without limit across a long-running session (§99);
/// callers that know their active-universe size can set a tighter one via
/// [`WorldState::with_capacity`]. Chosen well above any realistic concurrent-market
/// count so it never perturbs normal operation.
pub const DEFAULT_MARKET_CAP: usize = 65_536;

/// The full multi-market world state.
///
/// Markets are stored in an index vector (never a `HashMap`) so there is no
/// iteration-order nondeterminism; hashing sorts keys explicitly. The vector is
/// bounded at `cap`: inserting a new market when full evicts the oldest-inserted one
/// (FIFO), so a bot that has seen millions of mints over a long session retains only
/// the most recent `cap` (§99). Re-`upsert`ing an existing key never evicts.
#[derive(Clone, Debug)]
pub struct WorldState {
    markets: Vec<(u64, MarketState)>,
    cap: usize,
}

impl Default for WorldState {
    fn default() -> Self {
        Self::new()
    }
}

impl WorldState {
    /// Empty world with the default market cap.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_MARKET_CAP)
    }

    /// Empty world with an explicit market cap (clamped to `>= 1`).
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            markets: Vec::new(),
            cap: cap.max(1),
        }
    }

    /// The market cap in force.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Insert or replace the market under `key`. If `key` is new and the world is at
    /// capacity, the oldest-inserted market is evicted first (FIFO, deterministic).
    pub fn upsert_market(&mut self, key: u64, state: MarketState) {
        if let Some(slot) = self.markets.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = state;
        } else {
            if self.markets.len() >= self.cap {
                self.markets.remove(0);
            }
            self.markets.push((key, state));
        }
    }

    /// Immutable access to a market by key.
    pub fn market(&self, key: u64) -> Option<&MarketState> {
        self.markets.iter().find(|(k, _)| *k == key).map(|(_, m)| m)
    }

    /// All market keys, in insertion order (callers that need order must sort).
    pub fn market_keys(&self) -> impl Iterator<Item = u64> + '_ {
        self.markets.iter().map(|(k, _)| *k)
    }

    /// Number of markets.
    pub fn len(&self) -> usize {
        self.markets.len()
    }

    /// Whether the world holds no markets.
    pub fn is_empty(&self) -> bool {
        self.markets.is_empty()
    }
}

/// Order-stable snapshot hash for replay-parity assertion.
///
/// Iterates markets in sorted key order, so insertion order is irrelevant: two
/// hashes of the same logical state are equal within and across processes. The
/// input is a canonical little-endian field serialization prefixed with a schema
/// byte. No floats are hashed (there are none in state).
pub fn state_hash(state: &WorldState) -> [u8; 32] {
    let mut keys: Vec<u64> = state.market_keys().collect();
    keys.sort_unstable();
    keys.dedup();

    let mut h = Hasher::new();
    h.update(&[SCHEMA_V]);
    h.update(&(keys.len() as u64).to_le_bytes());
    for k in keys {
        h.update(&k.to_le_bytes());
        if let Some(m) = state.market(k) {
            m.write_canon(&mut h);
        }
    }
    h.finalize()
}

// ============================================================================
// Leaf: rd_quantize_boundary — fixed-point quantization boundary
// ============================================================================

/// Fixed-point quantization boundary for off-hot-path features entering the reducer.
///
/// This is the ONLY function in the module where an `f64` may appear: it is the
/// boundary adapter that converts an externally-computed feature into the integer
/// domain the reducer operates in. It is deliberately outside the hot path.
///
/// Contract:
/// - `NaN`, `±inf`, or an out-of-range magnitude yield `None` (the feature is
///   marked incomplete) — never a fabricated value.
/// - Rounding is round-half-away-from-zero (Rust's `f64::round`), the single
///   rounding convention used everywhere in the system.
pub fn quantize_feature(raw_f64: f64, scale: u32) -> Option<i64> {
    if !raw_f64.is_finite() {
        return None;
    }
    let scaled = raw_f64 * scale as f64;
    // Guard the round-trip into i64 with a conservative bound.
    if scaled.abs() > (i64::MAX as f64) / 2.0 {
        return None;
    }
    Some(scaled.round() as i64)
}
