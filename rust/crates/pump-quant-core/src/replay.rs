//! Append-only binary journal codec and the deterministic replayer.
//!
//! This module implements the write/read path for the "hot journal" (constitution
//! §43): length-prefixed, CRC-protected frames carrying schema version, connection
//! epoch and sequence number; a crash-recovery scan that returns exactly the
//! sealed-frame prefix while truncating any torn tail; a deterministic
//! cross-epoch merge into a single apply order; and the parity replayer that folds
//! journaled events through the SAME reducer the live path uses and asserts
//! state-hash parity against recorded checkpoints, reporting the FIRST divergence.
//!
//! Constitution §22 compliance: NO floating point anywhere in outcome-controlling
//! logic. All checksums, hashes and ordering are integer / fixed-point. Overflow
//! is always explicit (checked / wrapping-by-contract). Every code path is
//! bounds-checked so that arbitrary bytes can never panic the recovery scan.

// ----------------------------------------------------------------------------
// Constants describing the on-disk frame layout.
//
//   [u32 len][u8 schema][u32 epoch][u64 seq][payload ...][u32 crc32]
//
// `len` counts every byte that FOLLOWS the len field itself, i.e.
// schema(1) + epoch(4) + seq(8) + payload + crc(4). The CRC is computed over
// everything after the len field EXCEPT the trailing CRC word (schema, epoch,
// seq and payload). `len` is validated by bounds-checking, not by the CRC, so a
// corrupt length is caught structurally (Truncated / BadLen) rather than by CRC.
// ----------------------------------------------------------------------------

/// Bytes of fixed header that live after the length prefix: schema + epoch + seq.
const HEADER_TAIL: usize = 1 + 4 + 8;
/// Bytes occupied by the trailing CRC-32 word.
const CRC_LEN: usize = 4;
/// Minimum legal value of the `len` field: fixed header tail + CRC, zero payload.
const MIN_LEN: usize = HEADER_TAIL + CRC_LEN;
/// Byte offset of the payload within a frame (after len + fixed header tail).
const PAYLOAD_OFF: usize = 4 + HEADER_TAIL;

/// Largest payload a single frame may carry (16 MiB). Keeps `len` well inside u32.
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

/// Preallocated capacity for a fresh segment buffer, so the steady-state write
/// path performs zero per-frame allocations (invariant: "journal write path
/// allocates zero per frame").
const SEGMENT_CAP: usize = 1 << 20;

/// Errors produced by the journal codec / replayer. `Copy` so callers can match
/// freely without moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JErr {
    /// The input ended before a full frame could be read.
    Truncated,
    /// The frame's stored CRC did not match the recomputed CRC (corruption).
    Crc,
    /// The length prefix is structurally impossible (smaller than a header).
    BadLen,
    /// The payload exceeds `MAX_PAYLOAD`.
    TooLarge,
    /// A duplicate `(epoch, seq)` was seen while merging epochs.
    DupSeq,
}

// ----------------------------------------------------------------------------
// CRC-32 (IEEE 802.3, reflected, polynomial 0xEDB88320) — pure integer.
// ----------------------------------------------------------------------------

/// Standard reflected CRC-32. Deterministic and integer-only; any single-bit
/// flip within `data` changes the result.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        let mut k = 0;
        while k < 8 {
            // mask = 0xFFFFFFFF if the low bit is set, else 0 (branchless).
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            k += 1;
        }
    }
    !crc
}

// ----------------------------------------------------------------------------
// SegBuf: a preallocated, append-only segment buffer.
// ----------------------------------------------------------------------------

/// A preallocated, append-only segment buffer that frames are encoded into.
/// Backed by a single `Vec<u8>` reserved up front so encoding a frame appends
/// without reallocating in the steady state.
#[derive(Debug, Clone, Default)]
pub struct SegBuf {
    buf: Vec<u8>,
}

impl SegBuf {
    /// Create an empty segment buffer with a preallocated backing store.
    pub fn new() -> Self {
        SegBuf {
            buf: Vec::with_capacity(SEGMENT_CAP),
        }
    }

    /// Create a segment buffer with an explicit reserved capacity.
    pub fn with_capacity(cap: usize) -> Self {
        SegBuf {
            buf: Vec::with_capacity(cap),
        }
    }

    /// Borrow the encoded bytes accumulated so far.
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Number of encoded bytes currently held.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether the buffer holds no bytes.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Reset the buffer to empty, retaining its allocation for reuse.
    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

// ----------------------------------------------------------------------------
// leaf rp_frame_codec: encode/decode one journal frame.
// ----------------------------------------------------------------------------

/// A decoded frame. The payload is borrowed directly out of the input slice
/// (zero-copy) for the lifetime of that slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Schema version byte.
    pub schema: u8,
    /// Connection / writer epoch.
    pub epoch: u32,
    /// Monotonic sequence number within the epoch.
    pub seq: u64,
    /// Borrowed payload bytes (zero-copy).
    pub payload: &'a [u8],
}

/// Encode one frame into `buf`, appending
/// `[len][schema][epoch][seq][payload][crc]`. The CRC covers the fixed header
/// tail plus the payload. Returns `JErr::TooLarge` if the payload exceeds
/// `MAX_PAYLOAD`.
pub fn encode_frame(
    buf: &mut SegBuf,
    schema: u8,
    epoch: u32,
    seq: u64,
    payload: &[u8],
) -> Result<(), JErr> {
    if payload.len() > MAX_PAYLOAD {
        return Err(JErr::TooLarge);
    }
    // len = everything after the len field.
    let len = HEADER_TAIL + payload.len() + CRC_LEN;
    if len > u32::MAX as usize {
        return Err(JErr::TooLarge);
    }

    let start = buf.buf.len();
    buf.buf.extend_from_slice(&(len as u32).to_le_bytes());
    buf.buf.push(schema);
    buf.buf.extend_from_slice(&epoch.to_le_bytes());
    buf.buf.extend_from_slice(&seq.to_le_bytes());
    buf.buf.extend_from_slice(payload);

    // CRC over schema + epoch + seq + payload (everything after len so far).
    let crc = crc32(&buf.buf[start + 4..]);
    buf.buf.extend_from_slice(&crc.to_le_bytes());
    Ok(())
}

/// Decode the frame at the start of `bytes`. Returns the borrowed `Frame` and
/// the number of bytes consumed. Fully bounds-checked: never panics on
/// arbitrary input. Verifies the CRC before exposing the payload.
pub fn decode_frame(bytes: &[u8]) -> Result<(Frame<'_>, usize), JErr> {
    // Need at least the length prefix.
    if bytes.len() < 4 {
        return Err(JErr::Truncated);
    }
    let len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;

    // Structurally impossible length (too small to even hold header + crc).
    if len < MIN_LEN {
        return Err(JErr::BadLen);
    }
    // Total frame size = len prefix + declared tail. Guard the add for safety.
    let total = match 4usize.checked_add(len) {
        Some(t) => t,
        None => return Err(JErr::BadLen),
    };
    if bytes.len() < total {
        return Err(JErr::Truncated);
    }

    let schema = bytes[4];
    let epoch = u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]);
    let seq = u64::from_le_bytes([
        bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15], bytes[16],
    ]);

    let payload_end = total - CRC_LEN;
    let payload = &bytes[PAYLOAD_OFF..payload_end];

    let crc_stored = u32::from_le_bytes([
        bytes[payload_end],
        bytes[payload_end + 1],
        bytes[payload_end + 2],
        bytes[payload_end + 3],
    ]);
    // CRC region: everything after len except the trailing CRC word.
    let crc_calc = crc32(&bytes[4..payload_end]);
    if crc_calc != crc_stored {
        return Err(JErr::Crc);
    }

    Ok((
        Frame {
            schema,
            epoch,
            seq,
            payload,
        },
        total,
    ))
}

// ----------------------------------------------------------------------------
// leaf rp_recovery_scan: crash-recovery scan.
// ----------------------------------------------------------------------------

/// Metadata for one recovered frame: its byte extent `[start, end)` plus its
/// header fields. Payload bytes are not retained here (only the extent is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMeta {
    /// Byte offset where the frame begins.
    pub start: usize,
    /// Byte offset one past the frame's end.
    pub end: usize,
    /// Schema version byte.
    pub schema: u8,
    /// Connection / writer epoch.
    pub epoch: u32,
    /// Monotonic sequence number within the epoch.
    pub seq: u64,
}

impl FrameMeta {
    /// Construct a placeholder `FrameMeta` carrying only `(epoch, seq)`, used to
    /// exercise ordering logic that depends solely on those fields.
    pub fn test(epoch: u32, seq: u64) -> FrameMeta {
        FrameMeta {
            start: 0,
            end: 0,
            schema: 0,
            epoch,
            seq,
        }
    }
}

/// Result of a recovery scan: the metadata of every fully-sealed frame, the
/// length of the valid prefix, and whether a torn / corrupt tail was seen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoverResult {
    /// Metadata for each fully-sealed frame, in file order.
    pub frames: Vec<FrameMeta>,
    /// Byte length of the valid (fully-sealed) prefix.
    pub valid_len: usize,
    /// True if the scan stopped early on a torn tail or corrupt frame.
    pub torn: bool,
}

/// Scan `bytes` and return exactly the prefix of fully-sealed frames. A torn
/// tail (`Truncated`) or a corrupt / structurally-invalid frame stops the scan
/// at the last good frame boundary with `torn = true`. Never panics on
/// arbitrary bytes: all bounds-checking is delegated to `decode_frame`, and
/// `recover` only ever advances forward.
pub fn recover(bytes: &[u8]) -> RecoverResult {
    let mut frames = Vec::new();
    let mut off = 0usize;
    let mut torn = false;

    loop {
        if off == bytes.len() {
            break; // clean end: consumed exactly the sealed prefix
        }
        match decode_frame(&bytes[off..]) {
            Ok((f, used)) => {
                // Defensive: a zero-length advance would loop forever. By the
                // codec contract `used >= 4 + MIN_LEN`, but guard anyway.
                if used == 0 {
                    torn = true;
                    break;
                }
                frames.push(FrameMeta {
                    start: off,
                    end: off + used,
                    schema: f.schema,
                    epoch: f.epoch,
                    seq: f.seq,
                });
                off += used;
            }
            // Any error (torn tail, CRC failure, bad length) halts the scan at
            // the last good boundary. Sealed data before `off` is preserved.
            Err(_) => {
                torn = true;
                break;
            }
        }
    }

    RecoverResult {
        frames,
        valid_len: off,
        torn,
    }
}

// ----------------------------------------------------------------------------
// leaf rp_epoch_order: deterministic cross-epoch merge.
// ----------------------------------------------------------------------------

/// Merge frames across connection / writer epochs into a single deterministic
/// apply order. The result is a permutation of the input indices sorted by
/// `(epoch, seq)` lexicographically. A duplicate `(epoch, seq)` is a hard error
/// (`JErr::DupSeq`) — it is never silently kept or dropped.
pub fn epoch_merge(frames: &[FrameMeta]) -> Result<Vec<usize>, JErr> {
    let mut order: Vec<usize> = (0..frames.len()).collect();
    // Stable sort by (epoch, seq). Determinism does not depend on prior order
    // because the keys are unique once the duplicate check passes; ties (which
    // would only be exact (epoch, seq) duplicates) are rejected below.
    order.sort_by(|&a, &b| {
        (frames[a].epoch, frames[a].seq).cmp(&(frames[b].epoch, frames[b].seq))
    });

    // Scan adjacent pairs for an identical (epoch, seq).
    let mut i = 1;
    while i < order.len() {
        let prev = &frames[order[i - 1]];
        let cur = &frames[order[i]];
        if prev.epoch == cur.epoch && prev.seq == cur.seq {
            return Err(JErr::DupSeq);
        }
        i += 1;
    }

    Ok(order)
}

// ----------------------------------------------------------------------------
// leaf rp_parity_assert: replay through the reducer and assert hash parity.
//
// The reducer (`WorldState` / `apply_world` / `state_hash`) is defined here as
// the single canonical implementation; the live path and the replayer both call
// the SAME functions, so replay cannot diverge from live by using different
// code. All state evolution and hashing are integer-only and deterministic.
// ----------------------------------------------------------------------------

/// A canonical, fully-normalized event fed to the reducer. Fields are the
/// deterministic integer inputs the reducer folds into world state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonEvent {
    /// First canonical field.
    pub a: i64,
    /// Second canonical field.
    pub b: i64,
    /// Third canonical field.
    pub c: i64,
    /// Fourth canonical field.
    pub d: i64,
}

impl CanonEvent {
    /// Construct a canonical event from its four integer fields.
    pub fn test(a: i64, b: i64, c: i64, d: i64) -> CanonEvent {
        CanonEvent { a, b, c, d }
    }
}

/// Deterministic world state accumulated by the reducer. Integer-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldState {
    /// Number of events applied so far.
    seq_no: u64,
    /// Running integer checksum of applied events.
    checksum: u64,
}

impl WorldState {
    /// The canonical genesis state.
    pub fn new() -> WorldState {
        WorldState {
            seq_no: 0,
            checksum: 0x1234_5678_9ABC_DEF0,
        }
    }
}

impl Default for WorldState {
    fn default() -> Self {
        WorldState::new()
    }
}

/// The reducer: apply one event to the state, producing the next state. Pure,
/// deterministic and integer-only (wrapping arithmetic is explicit and part of
/// the contract). This is the SAME function the live path uses.
pub fn apply_world(state: &WorldState, ev: &CanonEvent) -> WorldState {
    let mut cs = state.checksum;
    // Mix each field with a golden-ratio multiplier and an odd additive
    // constant so distinct events yield distinct, well-spread checksums.
    let fields = [ev.a, ev.b, ev.c, ev.d];
    let mut i = 0;
    while i < fields.len() {
        let v = fields[i] as u64;
        cs = cs.rotate_left(7) ^ v.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        cs = cs.wrapping_add(0xC2B2_AE3D_27D4_EB4F);
        i += 1;
    }
    WorldState {
        seq_no: state.seq_no.wrapping_add(1),
        checksum: cs,
    }
}

/// Compute the canonical 32-byte state hash. Deterministic, integer-only
/// (a fixed avalanche mix expanded across four 64-bit lanes). This is the SAME
/// hash the live snapshot path records.
pub fn state_hash(state: &WorldState) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut h = state
        .checksum
        .wrapping_mul(0x0000_0100_0000_01B3) // 64-bit FNV prime
        ^ state.seq_no;
    let mut off = 0;
    while off < out.len() {
        // splitmix64-style avalanche for good bit diffusion.
        h ^= h >> 33;
        h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        h ^= h >> 33;
        h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
        h ^= h >> 33;
        let lane = h.to_le_bytes();
        let mut j = 0;
        while j < 8 {
            out[off + j] = lane[j];
            j += 1;
        }
        // Perturb before the next lane so lanes differ.
        h = h.wrapping_add(state.seq_no).rotate_left(17) ^ state.checksum;
        off += 8;
    }
    out
}

/// The verdict of a parity replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayVerdict {
    /// Every checkpoint matched the replayed state hash.
    Match,
    /// The replay diverged from the recorded hash at the FIRST mismatching
    /// checkpoint (by event index).
    Diverged {
        /// Event index at which the divergence was detected.
        at_event: usize,
        /// The recorded (live) state hash expected at that checkpoint.
        expected: [u8; 32],
        /// The replayed state hash actually produced.
        got: [u8; 32],
    },
}

/// Replay `events` through the reducer from genesis and assert hash parity
/// against the recorded `checkpoints` (`(event_index, hash)` pairs). Reports the
/// FIRST divergent checkpoint's event index — parity failures are never
/// summarized away as merely "mismatch at end". A `Match` verdict requires every
/// in-range checkpoint to match.
pub fn replay_assert(
    events: &[CanonEvent],
    checkpoints: &[(usize, [u8; 32])],
) -> ReplayVerdict {
    // Process checkpoints in ascending event-index order so the FIRST divergence
    // reported is truly the earliest in the event stream, regardless of the
    // order the caller supplied them.
    let mut cps: Vec<&(usize, [u8; 32])> = checkpoints.iter().collect();
    cps.sort_by_key(|c| c.0);

    let mut st = WorldState::new();
    let mut ci = 0usize;

    for (i, e) in events.iter().enumerate() {
        st = apply_world(&st, e);
        // A checkpoint may target this event index; compare all such.
        while ci < cps.len() && cps[ci].0 == i {
            let got = state_hash(&st);
            let expected = cps[ci].1;
            if got != expected {
                return ReplayVerdict::Diverged {
                    at_event: i,
                    expected,
                    got,
                };
            }
            ci += 1;
        }
        // Skip any checkpoints whose index is somehow < i (out of order / dup
        // handled by the ascending scan above); they cannot be satisfied.
        while ci < cps.len() && cps[ci].0 < i {
            ci += 1;
        }
    }

    // Any checkpoint referencing an event index beyond the stream can never
    // match and is a divergence: report it against the final replayed state.
    if ci < cps.len() {
        return ReplayVerdict::Diverged {
            at_event: cps[ci].0,
            expected: cps[ci].1,
            got: state_hash(&st),
        };
    }

    ReplayVerdict::Match
}
