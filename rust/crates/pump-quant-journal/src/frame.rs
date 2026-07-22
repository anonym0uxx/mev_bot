//! Length-prefixed, checksummed frame codec (§12 "append-only binary frames,
//! length prefix, frame CRC, ... schema version, connection epoch, sequence
//! tracking").
//!
//! ## On-wire layout (little-endian, integer fields only)
//!
//! | offset | size | field             |
//! |--------|------|-------------------|
//! | 0      | 4    | magic `PQJ1`      |
//! | 4      | 4    | payload_len (u32) | ← the length prefix
//! | 8      | 2    | schema_version    |
//! | 10     | 4    | connection_epoch  |
//! | 14     | 8    | sequence          |
//! | 22     | N    | payload           |
//! | 22+N   | 4    | CRC32 over [0 .. 22+N) | ← the frame CRC
//!
//! The magic prefix lets [`crate::recovery::recovery_scan`] positively identify a
//! frame boundary, and the trailing CRC covers the entire header *and* payload so
//! that any truncation or bit-flip is caught on decode. Decoding is strictly
//! bounded: a declared `payload_len` above the caller's cap is rejected *before* any
//! allocation, and a buffer shorter than the full declared frame is reported as
//! truncated rather than partially decoded.

use crate::checksum::crc32;

/// Frame magic prefix, ASCII `"PQJ1"` stored little-endian.
pub const FRAME_MAGIC: u32 = 0x314A_5150;

/// Bytes preceding the payload: magic(4) + payload_len(4) + schema(2) + epoch(4) + seq(8).
pub const HEADER_LEN: usize = 22;

/// Bytes following the payload: the CRC32 trailer.
pub const TRAILER_LEN: usize = 4;

/// Fixed per-frame overhead ([`HEADER_LEN`] + [`TRAILER_LEN`]).
pub const FRAME_OVERHEAD: usize = HEADER_LEN + TRAILER_LEN;

/// Default strict upper bound on a single frame's payload (16 MiB).
///
/// Callers may pass a tighter bound to [`Frame::decode`]; this is the conservative
/// default that prevents a corrupt length prefix from triggering a huge allocation.
pub const DEFAULT_MAX_PAYLOAD_LEN: u32 = 16 * 1024 * 1024;

/// A single decoded journal frame: the durable unit of the append-only log.
///
/// Responsibility (§12): carry one observation payload together with the metadata
/// needed for ordering and provenance — `schema_version`, `connection_epoch`, and a
/// monotonic `sequence`. All fields are integers or bytes (§22, no float).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Journal schema version this frame was written under.
    pub schema_version: u16,
    /// Connection epoch (which stream connection produced this frame).
    pub connection_epoch: u32,
    /// Monotonic per-journal sequence number.
    pub sequence: u64,
    /// Opaque payload bytes.
    pub payload: Vec<u8>,
}

/// Result of a successful [`Frame::decode`]: the frame plus the number of input
/// bytes it consumed (so a caller can advance to the next frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    /// The decoded frame.
    pub frame: Frame,
    /// Total bytes consumed from the input buffer (header + payload + trailer).
    pub consumed: usize,
}

/// Reasons a frame failed to encode or decode.
///
/// Every variant is a *bounded, explicit* failure — the codec never panics on
/// hostile input and never partially decodes (§12 "corruption behavior").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Buffer is smaller than the fixed header; `have` bytes present, [`HEADER_LEN`] needed.
    TooShortForHeader { have: usize },
    /// The magic prefix did not match [`FRAME_MAGIC`].
    BadMagic { found: u32 },
    /// The declared `payload_len` exceeded the caller's `max_payload_len`.
    PayloadTooLarge { len: u32, max: u32 },
    /// The full declared frame does not fit in the buffer; `need` total bytes required.
    Truncated { have: usize, need: usize },
    /// The stored CRC did not match the CRC recomputed over the frame.
    BadChecksum { expected: u32, found: u32 },
    /// Encoding overflowed the addressable byte space (payload too large for `usize`).
    LengthOverflow,
}

/// Total encoded size of a frame carrying `payload_len` payload bytes, or [`None`]
/// if that would overflow `usize`.
///
/// Responsibility: single source of truth for frame sizing, used by both the encoder
/// and by [`Segment`](crate::segment::Segment) capacity accounting. Overflow is
/// explicit (§22).
#[must_use]
pub fn encoded_len(payload_len: usize) -> Option<usize> {
    payload_len
        .checked_add(HEADER_LEN)?
        .checked_add(TRAILER_LEN)
}

impl Frame {
    /// Construct a frame from its fields.
    #[must_use]
    pub fn new(
        schema_version: u16,
        connection_epoch: u32,
        sequence: u64,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            schema_version,
            connection_epoch,
            sequence,
            payload,
        }
    }

    /// Encoded byte length of this frame, or [`None`] on `usize` overflow.
    #[must_use]
    pub fn encoded_len(&self) -> Option<usize> {
        encoded_len(self.payload.len())
    }

    /// Append the encoded frame to `buf`, returning the number of bytes written.
    ///
    /// Responsibility (§12 "buffered append"): serialize header, payload and CRC in
    /// wire order. Returns [`FrameError::PayloadTooLarge`] if the payload length does
    /// not fit in the `u32` length prefix, or [`FrameError::LengthOverflow`] on
    /// `usize` overflow — both checked before any bytes are written.
    pub fn encode_into(&self, buf: &mut Vec<u8>) -> Result<usize, FrameError> {
        let payload_len = self.payload.len();
        let payload_len_u32 =
            u32::try_from(payload_len).map_err(|_| FrameError::PayloadTooLarge {
                len: u32::MAX,
                max: u32::MAX,
            })?;
        let total = encoded_len(payload_len).ok_or(FrameError::LengthOverflow)?;

        let start = buf.len();
        buf.reserve(total);
        buf.extend_from_slice(&FRAME_MAGIC.to_le_bytes());
        buf.extend_from_slice(&payload_len_u32.to_le_bytes());
        buf.extend_from_slice(&self.schema_version.to_le_bytes());
        buf.extend_from_slice(&self.connection_epoch.to_le_bytes());
        buf.extend_from_slice(&self.sequence.to_le_bytes());
        buf.extend_from_slice(&self.payload);

        // CRC covers everything written so far for this frame (header + payload).
        let crc = crc32(&buf[start..]);
        buf.extend_from_slice(&crc.to_le_bytes());

        Ok(total)
    }

    /// Encode this frame into a fresh `Vec<u8>`.
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        let mut buf = Vec::new();
        self.encode_into(&mut buf)?;
        Ok(buf)
    }

    /// Decode the frame at the start of `buf`, enforcing `max_payload_len`.
    ///
    /// Responsibility (§12 "crash-recovery scan" primitive): strictly validate one
    /// frame. Checks, in order: header presence, magic, payload bound, full-frame
    /// presence, then CRC. Any failure returns a typed [`FrameError`] and consumes
    /// nothing. On success returns the frame and its consumed length so the caller
    /// can advance.
    pub fn decode(buf: &[u8], max_payload_len: u32) -> Result<DecodedFrame, FrameError> {
        if buf.len() < HEADER_LEN {
            return Err(FrameError::TooShortForHeader { have: buf.len() });
        }

        let magic = read_u32(buf, 0);
        if magic != FRAME_MAGIC {
            return Err(FrameError::BadMagic { found: magic });
        }

        let payload_len = read_u32(buf, 4);
        if payload_len > max_payload_len {
            return Err(FrameError::PayloadTooLarge {
                len: payload_len,
                max: max_payload_len,
            });
        }
        let payload_len_usize = payload_len as usize;

        let total = encoded_len(payload_len_usize).ok_or(FrameError::LengthOverflow)?;
        if buf.len() < total {
            return Err(FrameError::Truncated {
                have: buf.len(),
                need: total,
            });
        }

        // Recompute CRC over header+payload and compare with the stored trailer.
        let crc_region_end = HEADER_LEN + payload_len_usize;
        let expected = crc32(&buf[..crc_region_end]);
        let found = read_u32(buf, crc_region_end);
        if expected != found {
            return Err(FrameError::BadChecksum { expected, found });
        }

        let schema_version = read_u16(buf, 8);
        let connection_epoch = read_u32(buf, 10);
        let sequence = read_u64(buf, 14);
        let payload = buf[HEADER_LEN..crc_region_end].to_vec();

        Ok(DecodedFrame {
            frame: Frame {
                schema_version,
                connection_epoch,
                sequence,
                payload,
            },
            consumed: total,
        })
    }
}

// --- little-endian readers (bounds guaranteed by callers above) ---

#[inline]
fn read_u16(buf: &[u8], off: usize) -> u16 {
    let mut bytes = [0u8; 2];
    bytes.copy_from_slice(&buf[off..off + 2]);
    u16::from_le_bytes(bytes)
}

#[inline]
fn read_u32(buf: &[u8], off: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&buf[off..off + 4]);
    u32::from_le_bytes(bytes)
}

#[inline]
fn read_u64(buf: &[u8], off: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[off..off + 8]);
    u64::from_le_bytes(bytes)
}
