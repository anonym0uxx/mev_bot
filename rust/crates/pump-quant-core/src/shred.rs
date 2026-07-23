//! Earliest-observation shred-class ingestion behind a neutral observation contract.
//!
//! This module implements the four leaves of the `shred` component:
//!
//! * [`decode_header`]  — zero-copy, strictly bounds-checked shred-header decode.
//! * [`track`]          — FEC-set completion bookkeeping with bounded memory and
//!   conflict detection.
//! * [`reassemble`]     — order-exact reassembly of a completed FEC set into bytes,
//!   preserving the earliest local arrival timestamp.
//! * [`slot_parity`]    — §18.3.5 parity gate: shred-derived tx set vs. canonical.
//!
//! Design constraints (constitution): no floating point in outcome-controlling
//! logic (integer / fixed-point only), all arithmetic overflow is explicit,
//! decode paths never panic on arbitrary bytes, and a transaction is only ever
//! produced from a *complete, verified* reassembly — partial data never becomes a
//! transaction, missing shreds surface as explicit incompleteness.

use std::collections::BTreeMap;

// ===========================================================================
// Shared error type
// ===========================================================================

/// Errors produced by the shred decode / reassembly paths.
///
/// Every decode path returns one of these on malformed input rather than
/// panicking (fuzz obligation on every decode path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShredErr {
    /// Buffer is shorter than required, or a length field points past the buffer.
    Short,
    /// The `shred_type` discriminant is not a known value.
    Type,
    /// A data-shred index expected by a "complete" set is absent (asserted
    /// unreachable for a genuinely `CompleteSet`, surfaced defensively).
    Missing,
}

// ===========================================================================
// Leaf: sh_header_decode
// ===========================================================================

/// Wire length of the fixed shred header, in bytes.
///
/// Layout (all little-endian):
/// `slot: u64 | index: u32 | shred_type: u8 | fec_set_index: u32 | payload_len: u16`
/// at byte offsets `0 | 8 | 12 | 13 | 17`.
pub const HEADER_LEN: usize = 19;

/// Byte offset of the high byte of the `payload_len` field.
///
/// Used by tests to corrupt the declared payload length so it exceeds the
/// buffer; decoding such a header must fail (never over-read).
pub const BAD_PAYLOAD_LEN_OFF: usize = 18;

// Individual field offsets within the header.
const OFF_SLOT: usize = 0;
const OFF_INDEX: usize = 8;
const OFF_TYPE: usize = 12;
const OFF_FEC: usize = 13;
const OFF_PLEN: usize = 17;

/// Kind of shred, decoded from the single `shred_type` discriminant byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShredType {
    /// A data shred (carries reconstructable entry/transaction bytes).
    Data = 0,
    /// A coding (erasure/parity) shred.
    Code = 1,
}

impl ShredType {
    /// Map a raw discriminant byte to a known [`ShredType`], or `None` if unknown.
    pub fn from_u8(v: u8) -> Option<ShredType> {
        match v {
            0 => Some(ShredType::Data),
            1 => Some(ShredType::Code),
            _ => None,
        }
    }
}

/// Fixed portion of a shred, decoded from the wire.
///
/// `expected` is the expected data-shred count for the shred's FEC set; it is
/// not carried in this minimal wire layout, so [`decode_header`] leaves it `0`.
/// Test/synthetic constructors set it explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShredHeader {
    /// Absolute slot number the shred belongs to.
    pub slot: u64,
    /// Index of this shred within its FEC set / slot.
    pub index: u32,
    /// Kind of shred.
    pub shred_type: ShredType,
    /// Index identifying the FEC set within the slot.
    pub fec_set_index: u32,
    /// Declared payload length (bytes following the header).
    pub payload_len: u16,
    /// Expected data-shred count for the owning FEC set (0 when unknown).
    pub expected: u8,
}

impl ShredHeader {
    /// Byte offset (relative to the shred buffer start) at which the payload ends.
    ///
    /// Returns `None` on arithmetic overflow (treated as out-of-bounds).
    pub fn payload_end(&self) -> Option<usize> {
        HEADER_LEN.checked_add(self.payload_len as usize)
    }

    /// Range of the payload bytes within the shred buffer, if it fits `buf_len`.
    pub fn payload_range(&self, buf_len: usize) -> Option<std::ops::Range<usize>> {
        let end = self.payload_end()?;
        if end > buf_len {
            None
        } else {
            Some(HEADER_LEN..end)
        }
    }

    /// Expected data-shred count for this shred's FEC set.
    pub fn expected_data_count(&self) -> u8 {
        self.expected
    }

    /// Construct a header directly (synthetic / test use).
    pub fn test(slot: u64, index: u32, fec: u32, expected: u8) -> ShredHeader {
        ShredHeader {
            slot,
            index,
            shred_type: ShredType::Data,
            fec_set_index: fec,
            payload_len: 0,
            expected,
        }
    }

    /// Build a valid, fully-populated wire buffer (header + a default payload)
    /// for the given identifiers. The returned buffer decodes successfully.
    pub fn test_bytes(slot: u64, index: u32, fec: u32) -> Vec<u8> {
        const PAYLOAD: usize = 64;
        let mut b = vec![0u8; HEADER_LEN + PAYLOAD];
        b[OFF_SLOT..OFF_SLOT + 8].copy_from_slice(&slot.to_le_bytes());
        b[OFF_INDEX..OFF_INDEX + 4].copy_from_slice(&index.to_le_bytes());
        b[OFF_TYPE] = ShredType::Data as u8;
        b[OFF_FEC..OFF_FEC + 4].copy_from_slice(&fec.to_le_bytes());
        b[OFF_PLEN..OFF_PLEN + 2].copy_from_slice(&(PAYLOAD as u16).to_le_bytes());
        b
    }
}

/// Decode a shred header from `bytes` with strict bounds checking.
///
/// Never indexes past `bytes.len()`. Returns:
/// * [`ShredErr::Short`] if the buffer is smaller than [`HEADER_LEN`], or if the
///   declared payload would extend past the end of the buffer.
/// * [`ShredErr::Type`] if the `shred_type` discriminant is unknown.
pub fn decode_header(bytes: &[u8]) -> Result<ShredHeader, ShredErr> {
    if bytes.len() < HEADER_LEN {
        return Err(ShredErr::Short);
    }
    // All reads below are within `HEADER_LEN <= bytes.len()`. Each subslice has
    // exactly the array width, so `try_into` is infallible here.
    let slot = u64::from_le_bytes(bytes[OFF_SLOT..OFF_SLOT + 8].try_into().unwrap()); // LINT-ALLOW(hot_panic): infallible fixed-width subslice (len pre-checked)
    let index = u32::from_le_bytes(bytes[OFF_INDEX..OFF_INDEX + 4].try_into().unwrap()); // LINT-ALLOW(hot_panic): infallible fixed-width subslice (len pre-checked)
    let shred_type = ShredType::from_u8(bytes[OFF_TYPE]).ok_or(ShredErr::Type)?;
    let fec_set_index = u32::from_le_bytes(bytes[OFF_FEC..OFF_FEC + 4].try_into().unwrap()); // LINT-ALLOW(hot_panic): infallible fixed-width subslice (len pre-checked)
    let payload_len = u16::from_le_bytes(bytes[OFF_PLEN..OFF_PLEN + 2].try_into().unwrap()); // LINT-ALLOW(hot_panic): infallible fixed-width subslice (len pre-checked)

    let header = ShredHeader {
        slot,
        index,
        shred_type,
        fec_set_index,
        payload_len,
        expected: 0,
    };

    // The declared payload must fit inside the buffer (no over-read).
    match header.payload_end() {
        Some(end) if end <= bytes.len() => Ok(header),
        _ => Err(ShredErr::Short),
    }
}

// ===========================================================================
// Leaf: sh_fec_track
// ===========================================================================

/// Identifier for an FEC set: the pair `(slot, fec_set_index)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FecSetId {
    /// Slot the set belongs to.
    pub slot: u64,
    /// FEC-set index within the slot.
    pub fec: u32,
}

/// Outcome of tracking a single shred against the FEC table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    /// Newly recorded data shred for its slot/index.
    Stored,
    /// Exact replay of an already-seen (slot, index) with identical payload.
    Duplicate,
    /// Same (slot, index) seen with a *different* payload — never applied.
    Conflicting,
    /// The set just reached its expected data-shred count (fires exactly once).
    SetComplete(FecSetId),
    /// An older set was evicted to bound memory before inserting this shred.
    Evicted(FecSetId),
}

/// One tracked FEC set: which data indices have been seen and their fingerprints.
struct FecSet {
    id: FecSetId,
    expected: u8,
    /// data index -> payload fingerprint (for duplicate/conflict detection).
    slots: BTreeMap<usize, u64>,
    received: u32,
    completed: bool,
}

impl FecSet {
    fn new(id: FecSetId, expected: u8) -> FecSet {
        FecSet {
            id,
            expected,
            slots: BTreeMap::new(),
            received: 0,
            completed: false,
        }
    }
}

/// Fixed-capacity table of in-flight FEC sets.
///
/// When at capacity, inserting a new set first evicts the oldest-slot set
/// (emitting its incompleteness via [`Track::Evicted`]), keeping memory bounded.
pub struct FecTable {
    sets: Vec<FecSet>,
    cap: usize,
}

impl FecTable {
    /// Create a table holding at most `cap` concurrent FEC sets (min 1).
    pub fn with_capacity(cap: usize) -> FecTable {
        FecTable {
            sets: Vec::new(),
            cap: cap.max(1),
        }
    }

    /// Number of sets currently tracked.
    pub fn len(&self) -> usize {
        self.sets.len()
    }

    /// Whether the table currently tracks no sets.
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    fn position(&self, id: &FecSetId) -> Option<usize> {
        self.sets.iter().position(|s| &s.id == id)
    }

    /// Evict the oldest-slot set (smallest `(slot, fec)`), returning its id.
    fn evict_oldest(&mut self) -> Option<FecSetId> {
        if self.sets.is_empty() {
            return None;
        }
        let mut min_i = 0usize;
        for i in 1..self.sets.len() {
            if (self.sets[i].id.slot, self.sets[i].id.fec)
                < (self.sets[min_i].id.slot, self.sets[min_i].id.fec)
            {
                min_i = i;
            }
        }
        Some(self.sets.remove(min_i).id)
    }
}

/// 64-bit FNV-1a fingerprint of a payload — a cheap, deterministic, integer-only
/// content fingerprint used to distinguish duplicates from conflicts.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Track one data shred against the FEC table.
///
/// * Records a new `(slot, index)` as [`Track::Stored`].
/// * A repeat of the same `(slot, index)` with identical payload is
///   [`Track::Duplicate`]; with a different payload it is [`Track::Conflicting`]
///   (never last-write-wins). Both are counted, never re-applied.
/// * When the set's distinct data-shred count reaches its expected count,
///   returns [`Track::SetComplete`] — exactly once per set.
/// * If a new set must be created while at capacity, the oldest-slot set is
///   evicted first and [`Track::Evicted`] is returned (unless the new shred
///   simultaneously completes its set, which takes precedence).
pub fn track(sets: &mut FecTable, h: &ShredHeader, payload: &[u8]) -> Track {
    let id = FecSetId {
        slot: h.slot,
        fec: h.fec_set_index,
    };
    let idx = h.index as usize;
    let fp = fnv1a64(payload);

    // Bound memory: if this shred introduces a new set and we're full, evict
    // the oldest-slot set before inserting.
    let mut evicted: Option<FecSetId> = None;
    if sets.position(&id).is_none() && sets.sets.len() >= sets.cap {
        evicted = sets.evict_oldest();
    }

    // Find or create the set (preserving the first-seen `expected`).
    let pos = match sets.position(&id) {
        Some(p) => p,
        None => {
            sets.sets.push(FecSet::new(id, h.expected_data_count()));
            sets.sets.len() - 1
        }
    };
    let set = &mut sets.sets[pos];

    match set.slots.get(&idx).copied() {
        Some(existing) if existing == fp => return Track::Duplicate,
        Some(_) => return Track::Conflicting,
        None => {
            set.slots.insert(idx, fp);
            set.received = set.received.saturating_add(1);
        }
    }

    if !set.completed && set.expected as u32 > 0 && set.received == set.expected as u32 {
        set.completed = true;
        return Track::SetComplete(id);
    }

    if let Some(ev) = evicted {
        return Track::Evicted(ev);
    }
    Track::Stored
}

// ===========================================================================
// Leaf: sh_reassemble
// ===========================================================================

/// A single data shred belonging to a completed set.
#[derive(Debug, Clone)]
struct DataShred {
    index: u32,
    payload: Vec<u8>,
    arrival_ns: u64,
}

/// A verified-complete FEC set, ready for order-exact reassembly.
///
/// Construction is only via verified paths (or [`CompleteSet::test`]); holding a
/// value of this type is the proof that reassembly is legitimate.
#[derive(Debug, Clone)]
pub struct CompleteSet {
    shreds: Vec<DataShred>,
}

impl CompleteSet {
    /// Build a complete set from `(index, payload, arrival_ns)` tuples.
    ///
    /// Accepts any payload type convertible to a byte slice (arrays, slices,
    /// `Vec`, …).
    pub fn test<P: AsRef<[u8]>>(shreds: &[(u32, P, u64)]) -> CompleteSet {
        let shreds = shreds
            .iter()
            .map(|(index, payload, arrival_ns)| DataShred {
                index: *index,
                payload: payload.as_ref().to_vec(),
                arrival_ns: *arrival_ns,
            })
            .collect();
        CompleteSet { shreds }
    }

    /// Minimum local arrival timestamp across constituent shreds (0 if empty).
    pub fn min_arrival_ns(&self) -> u64 {
        self.shreds.iter().map(|s| s.arrival_ns).min().unwrap_or(0)
    }
}

/// Growable output buffer for reassembled entry bytes.
#[derive(Debug, Default, Clone)]
pub struct SegBuf {
    buf: Vec<u8>,
}

impl SegBuf {
    /// Create an empty buffer.
    pub fn new() -> SegBuf {
        SegBuf { buf: Vec::new() }
    }

    /// Append raw bytes to the buffer.
    pub fn extend(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Borrow the accumulated bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Number of bytes accumulated.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// Metadata describing a completed reassembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReassemblyMeta {
    /// Earliest local arrival timestamp across constituent shreds (preserved).
    pub first_local_arrival_ns: u64,
    /// Total number of bytes written to the output buffer.
    pub len: usize,
    /// Number of data shreds concatenated.
    pub shred_count: usize,
}

/// Reassemble a completed FEC set's data shreds into entry bytes, order-exact.
///
/// Payloads are concatenated in ascending shred-index order (not arrival order),
/// with no padding invented — the output length equals the exact sum of payload
/// lengths. The returned [`ReassemblyMeta`] carries `first_local_arrival_ns` =
/// the minimum arrival timestamp over all constituent shreds.
///
/// Indices must be contiguous `0..n`; a gap yields [`ShredErr::Missing`]
/// (unreachable for a genuinely complete set, checked defensively).
pub fn reassemble(set: &CompleteSet, out: &mut SegBuf) -> Result<ReassemblyMeta, ShredErr> {
    // Ascending index order.
    let mut order: Vec<usize> = (0..set.shreds.len()).collect();
    order.sort_by_key(|&i| set.shreds[i].index);

    // Verify contiguous 0..n and no duplicate indices; accumulate bytes/timing.
    let mut min_arrival = u64::MAX;
    for (expected_idx, &i) in order.iter().enumerate() {
        if set.shreds[i].index as usize != expected_idx {
            return Err(ShredErr::Missing);
        }
        out.extend(&set.shreds[i].payload);
        if set.shreds[i].arrival_ns < min_arrival {
            min_arrival = set.shreds[i].arrival_ns;
        }
    }

    Ok(ReassemblyMeta {
        first_local_arrival_ns: if order.is_empty() { 0 } else { min_arrival },
        len: out.len(),
        shred_count: order.len(),
    })
}

// ===========================================================================
// Leaf: sh_parity_gate
// ===========================================================================

/// A transaction signature (fixed 64 bytes on the wire; compared by value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxSig(pub [u8; 64]);

impl TxSig {
    /// Deterministic synthetic signature seeded by `n` (test / fixture use).
    pub fn test(n: u64) -> TxSig {
        let mut b = [0u8; 64];
        b[..8].copy_from_slice(&n.to_le_bytes());
        TxSig(b)
    }
}

/// Per-signature local arrival timestamps, used to compute shred-vs-canon deltas.
#[derive(Debug, Clone, Default)]
pub struct ArrivalMap {
    /// sig -> (shred_arrival_ns, canon_arrival_ns)
    m: BTreeMap<TxSig, (i64, i64)>,
}

impl ArrivalMap {
    /// An empty map.
    pub fn new() -> ArrivalMap {
        ArrivalMap { m: BTreeMap::new() }
    }

    /// An empty map (test fixture entry point).
    pub fn test() -> ArrivalMap {
        ArrivalMap::new()
    }

    /// Record shred and canonical arrival timestamps for a signature.
    pub fn insert(&mut self, sig: TxSig, shred_ns: i64, canon_ns: i64) {
        self.m.insert(sig, (shred_ns, canon_ns));
    }

    /// Signed arrival delta `shred - canon` for a matched signature, if known.
    /// Negative means shreds arrived earlier (the whole point of the source).
    fn delta(&self, sig: &TxSig) -> Option<i64> {
        self.m.get(sig).map(|(s, c)| s.wrapping_sub(*c))
    }
}

/// Verdict of a per-slot parity comparison (honest labeling, criterion 56).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParityVerdict {
    /// Shred-derived and canonical sets matched exactly.
    Pass,
    /// The sets differed (see the counts on [`SlotParity`]).
    Fail,
}

/// Result of comparing the shred-derived tx set against the canonical set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotParity {
    /// Signatures present in both sets.
    pub matched: u32,
    /// Signatures present only in the shred-derived set.
    pub shred_only: u32,
    /// Signatures present only in the canonical set.
    pub canon_only: u32,
    /// Median (p50) signed arrival delta over matched signatures, in ns.
    pub arrival_delta_ns_p50: i64,
    /// Overall verdict.
    pub verdict: ParityVerdict,
}

/// Sort and de-duplicate a slice of signatures into an owned, unique vec.
fn sorted_unique(sigs: &[TxSig]) -> Vec<TxSig> {
    let mut v = sigs.to_vec();
    v.sort_unstable();
    v.dedup();
    v
}

/// Lower-median (p50) of the values via `select_nth_unstable`; 0 if empty.
fn median_i64(v: &mut [i64]) -> i64 {
    if v.is_empty() {
        return 0;
    }
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable(mid);
    *m
}

/// §18.3.5 parity comparison: shred-derived tx set vs. canonical, per slot.
///
/// Both signature slices are de-duplicated before comparison, so the result is
/// order-independent and duplicate-safe. Counts are exact set differences. The
/// verdict is [`ParityVerdict::Pass`] **only** when both `shred_only` and
/// `canon_only` are zero; any discrepancy is [`ParityVerdict::Fail`] with counts.
pub fn slot_parity(shred_txs: &[TxSig], canon_txs: &[TxSig], arrivals: &ArrivalMap) -> SlotParity {
    let s = sorted_unique(shred_txs);
    let c = sorted_unique(canon_txs);

    let (mut i, mut j) = (0usize, 0usize);
    let (mut matched, mut shred_only, mut canon_only) = (0u32, 0u32, 0u32);
    let mut deltas: Vec<i64> = Vec::new();

    while i < s.len() && j < c.len() {
        match s[i].cmp(&c[j]) {
            std::cmp::Ordering::Less => {
                shred_only += 1;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                canon_only += 1;
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                matched += 1;
                if let Some(d) = arrivals.delta(&s[i]) {
                    deltas.push(d);
                }
                i += 1;
                j += 1;
            }
        }
    }
    shred_only += (s.len() - i) as u32;
    canon_only += (c.len() - j) as u32;

    let arrival_delta_ns_p50 = median_i64(&mut deltas);
    let verdict = if shred_only == 0 && canon_only == 0 {
        ParityVerdict::Pass
    } else {
        ParityVerdict::Fail
    };

    SlotParity {
        matched,
        shred_only,
        canon_only,
        arrival_delta_ns_p50,
        verdict,
    }
}
