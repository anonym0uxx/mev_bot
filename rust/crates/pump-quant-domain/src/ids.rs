//! Canonical identifier newtypes.
//!
//! ## Responsibility
//! Opaque, copyable, orderable identity types shared across the whole system:
//! on-chain mint addresses, chain slots, and the internal opaque ids that tag
//! trades, observation sources, and providers. These carry no behaviour beyond
//! identity, ordering, hashing, and (for [`Mint`]) a total, dependency-free
//! hex codec.
//!
//! ## Constitution alignment
//! Section 17 — these are the neutral id types used inside `RawObservation`
//! (`ProviderId`, `ObservationSourceId`), `CanonicalTransaction`, and
//! `DecisionRecord` (`mint`, `slot`). No provider SDK type leaks through them.

use core::fmt;

/// A Solana SPL token mint address: the raw 32-byte Ed25519 public key.
///
/// Stored as raw bytes (never a provider string type) so it is `Copy`, cheap to
/// hash/compare, and free of any base58/allocation dependency on the hot path.
/// A dependency-free lowercase-hex [`fmt::Display`]/[`Mint::to_hex`] and
/// [`Mint::from_hex`] codec is provided for logs, fixtures, and journals.
///
/// Constitution Section 17 / 18: the canonical token identity in every schema.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mint(pub [u8; 32]);

impl Mint {
    /// The all-zero mint (canonical "unset"/sentinel value, e.g. an unset quote
    /// mint slot before a market's quote is decoded on chain).
    pub const ZERO: Mint = Mint([0u8; 32]);

    /// Borrow the raw 32 address bytes.
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Construct from raw bytes (total; every 32-byte array is a valid `Mint`).
    #[inline]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Mint(bytes)
    }

    /// Encode as a 64-character lowercase hex string.
    ///
    /// Allocation-free `Display` is also available via [`fmt::Display`]; this
    /// helper is the owned-`String` convenience for callers off the hot path.
    pub fn to_hex(&self) -> String {
        // Independent, explicit nibble expansion (no external hex crate).
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for &byte in &self.0 {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }

    /// Parse a 64-character (optionally `0x`-prefixed) hex string into a `Mint`.
    ///
    /// Total over its `Result`: rejects wrong length and non-hex characters
    /// rather than panicking, so untrusted journal/fixture input is safe.
    pub fn from_hex(input: &str) -> Result<Self, ParseMintError> {
        let s = input.strip_prefix("0x").unwrap_or(input);
        let bytes = s.as_bytes();
        if bytes.len() != 64 {
            return Err(ParseMintError::BadLength { found: bytes.len() });
        }
        let mut out = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            let hi = decode_nibble(bytes[i * 2])?;
            let lo = decode_nibble(bytes[i * 2 + 1])?;
            out[i] = (hi << 4) | lo;
            i += 1;
        }
        Ok(Mint(out))
    }
}

#[inline]
fn decode_nibble(c: u8) -> Result<u8, ParseMintError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(ParseMintError::BadChar { byte: c }),
    }
}

impl fmt::Display for Mint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Mint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mint({self})")
    }
}

/// Error returned by [`Mint::from_hex`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParseMintError {
    /// The input, after stripping any `0x`, was not exactly 64 hex digits.
    BadLength {
        /// Number of characters actually seen.
        found: usize,
    },
    /// A character outside `[0-9a-fA-F]` was encountered.
    BadChar {
        /// The offending byte.
        byte: u8,
    },
}

impl fmt::Display for ParseMintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseMintError::BadLength { found } => {
                write!(f, "mint hex must be 64 chars, found {found}")
            }
            ParseMintError::BadChar { byte } => {
                write!(f, "invalid hex byte 0x{byte:02x}")
            }
        }
    }
}

impl std::error::Error for ParseMintError {}

/// A Solana slot number: the monotonic ledger-position clock the strategy core
/// treats as its canonical ordering key (Section 22 uses slots, never wall time,
/// for outcome-controlling ordering).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Slot(pub u64);

impl Slot {
    /// Slots elapsed from `self` to `later`, saturating at zero if `later`
    /// precedes `self` (never underflows).
    #[inline]
    pub const fn distance_to(self, later: Slot) -> u64 {
        later.0.saturating_sub(self.0)
    }

    /// The next slot, saturating at [`u64::MAX`].
    #[inline]
    pub const fn next(self) -> Slot {
        Slot(self.0.saturating_add(1))
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Opaque identifier for one round-trip trade / order-intent lineage created by
/// the strategy core. Monotonic within a run; compared and hashed, never
/// interpreted arithmetically. Constitution Section 17 (`DecisionRecord`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct TradeId(pub u64);

/// Opaque identifier for an observation source instance (e.g. one adapter/feed
/// connection). Corresponds to `ObservationSourceId` in Section 17 / 18.8; the
/// numeric value is a registry handle with no arithmetic meaning.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct SourceId(pub u32);

/// Opaque identifier for a data/execution provider (e.g. Helius, Jito, canonical
/// RPC). Distinct from [`SourceId`]: one provider can back several sources.
/// Constitution Section 17 (`ProviderId`) / 18.8 capability-based role model.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct ProviderId(pub u32);
