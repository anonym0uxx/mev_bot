//! Canonical, injective, deterministic encoding of a configuration value.
//!
//! ## Responsibility
//! Turn a strategy or evaluator configuration into a single, stable byte string
//! so that two logically-equal configs — even built with different map
//! insertion orders — produce byte-identical encodings, and any change to any
//! value changes the encoding. This is the preimage under the reproducible
//! [`crate::hashing`] digests (§56.3 StrategyHash / EvaluatorReleaseHash,
//! §19 reproducibility).
//!
//! ## §22 compliance
//! The value model deliberately has **no floating-point variant**. Numbers are
//! carried as `u64` or `i128` fixed-point (the caller picks the scale), matching
//! the integer/lamports/basis-point money discipline of §22 / §705.
//!
//! ## Canonical form (why it is injective)
//! Every value is written as a one-byte type tag followed by a length-prefixed,
//! self-delimiting body. Because the tag disambiguates the type and every
//! variable-length body is length-prefixed, no two distinct values can share an
//! encoding, and no value is a prefix of another. Map keys are emitted in sorted
//! (`BTreeMap`) order, so insertion order is irrelevant.

use std::collections::BTreeMap;

/// Type tags. Stable wire constants — never renumber; doing so would silently
/// change every historical [`crate::hashing::StrategyHash`].
mod tag {
    pub const BOOL: u8 = 0x01;
    pub const U64: u8 = 0x02;
    pub const I128: u8 = 0x03;
    pub const BYTES: u8 = 0x04;
    pub const TEXT: u8 = 0x05;
    pub const LIST: u8 = 0x06;
    pub const MAP: u8 = 0x07;
}

/// A configuration value in the canonical, float-free model.
///
/// ## Constitution
/// The neutral config representation hashed for §56.3 registry pinning. Uses a
/// [`BTreeMap`] for maps so key ordering is canonical by construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanonicalValue {
    /// A boolean flag.
    Bool(bool),
    /// An unsigned integer (counts, versions, unsigned fixed-point).
    U64(u64),
    /// A signed 128-bit integer / fixed-point quantity (lamports, basis points,
    /// signed thresholds). The widest integer used by the envelope guard.
    I128(i128),
    /// Opaque bytes (e.g. a program id, a nested precomputed hash).
    Bytes(Vec<u8>),
    /// UTF-8 text (identifiers, labels).
    Text(String),
    /// An ordered list; order is significant and preserved.
    List(Vec<CanonicalValue>),
    /// A keyed map; keys are emitted in sorted order, so insertion order does
    /// not affect the encoding.
    Map(BTreeMap<String, CanonicalValue>),
}

impl CanonicalValue {
    /// Append this value's canonical byte encoding to `out`.
    ///
    /// Length prefixes use a fixed 8-byte big-endian `u64`, so the encoding is
    /// architecture-independent and deterministic. `len` is a container/byte
    /// count and always fits in `u64`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            CanonicalValue::Bool(b) => {
                out.push(tag::BOOL);
                out.push(if *b { 1 } else { 0 });
            }
            CanonicalValue::U64(n) => {
                out.push(tag::U64);
                out.extend_from_slice(&n.to_be_bytes());
            }
            CanonicalValue::I128(n) => {
                out.push(tag::I128);
                out.extend_from_slice(&n.to_be_bytes());
            }
            CanonicalValue::Bytes(bytes) => {
                out.push(tag::BYTES);
                write_len(out, bytes.len());
                out.extend_from_slice(bytes);
            }
            CanonicalValue::Text(text) => {
                out.push(tag::TEXT);
                let bytes = text.as_bytes();
                write_len(out, bytes.len());
                out.extend_from_slice(bytes);
            }
            CanonicalValue::List(items) => {
                out.push(tag::LIST);
                write_len(out, items.len());
                for item in items {
                    item.encode_into(out);
                }
            }
            CanonicalValue::Map(map) => {
                out.push(tag::MAP);
                write_len(out, map.len());
                // BTreeMap iterates in sorted key order: canonical by design.
                for (key, value) in map {
                    let key_bytes = key.as_bytes();
                    write_len(out, key_bytes.len());
                    out.extend_from_slice(key_bytes);
                    value.encode_into(out);
                }
            }
        }
    }

    /// Return the full canonical byte encoding of this value.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }
}

/// Write a length as a fixed 8-byte big-endian prefix.
///
/// `len` is a Rust collection length (`usize`); on any platform it fits in
/// `u64`, so the `as u64` cast is total and deterministic.
fn write_len(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(&(len as u64).to_be_bytes());
}
