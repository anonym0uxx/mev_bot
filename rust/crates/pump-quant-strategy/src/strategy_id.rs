//! # strategy_id — reproducible canonical-hash primitive (criterion 33 / 51)
//!
//! A [`StrategyConfig`] is serialized into a **canonical**, length-framed byte
//! sequence and reduced to a stable 64-bit digest ([`strategy_id_hash`]). The
//! digest is the reproducible `StrategyId`: two configs that are field-for-field
//! equal hash to the same value, and any change to any field changes the digest.
//! The versioned ID/registry lives in the supervisor; this module is only the
//! deterministic leaf that produces the digest.
//!
//! ## Constitution
//! §22: no `f32`/`f64` anywhere — the config is entirely integer/enumerated and
//! the hash is pure integer arithmetic (FNV-1a, `wrapping` by contract). The
//! function is total and deterministic: identical bytes always yield the
//! identical digest with no clock, RNG, or network input.

/// FNV-1a 64-bit offset basis.
pub const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Sizing family selector — part of the canonical config.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SizingFamily {
    /// Fixed evidence-stratified probe tiers (Layer 1).
    ProbeTier,
    /// Distributional log-utility research sizing (Layer 2).
    LogUtility,
    /// Capital-sleeve mature sizing (Layer 3).
    Sleeve,
}

impl SizingFamily {
    /// Canonical discriminant byte for this variant.
    #[inline]
    pub fn tag(self) -> u8 {
        match self {
            SizingFamily::ProbeTier => 0,
            SizingFamily::LogUtility => 1,
            SizingFamily::Sleeve => 2,
        }
    }
}

/// The immutable strategy configuration that is hashed into a `StrategyId`.
///
/// Every field is integer or enumerated so the byte encoding is exact and
/// reproducible. Adding a field to the strategy must extend
/// [`StrategyConfig::canonical_bytes`] so the change is reflected in the digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrategyConfig {
    /// Human-facing strategy name (canonicalized as length-framed UTF-8 bytes).
    pub name: String,
    /// Entry-mode identifier.
    pub entry_mode: u16,
    /// Setup-archetype identifier.
    pub archetype: u16,
    /// Sizing family.
    pub sizing: SizingFamily,
    /// Registered numeric parameters, in declaration order (order is significant).
    pub params_fp: Vec<i64>,
    /// Feature-schema version this config was compiled against.
    pub feature_schema_version: u32,
}

impl StrategyConfig {
    /// A deterministic non-trivial fixture used by the property tests.
    pub fn test() -> Self {
        StrategyConfig {
            name: "active_market_scalp".to_string(),
            entry_mode: 3,
            archetype: 7,
            sizing: SizingFamily::ProbeTier,
            params_fp: vec![10_000, -250, 42],
            feature_schema_version: 11,
        }
    }

    /// Canonical, length-framed byte encoding of the config.
    ///
    /// Each variable-length field is prefixed with its length as 8 little-endian
    /// bytes, and every scalar is written little-endian, so no two distinct
    /// configs can produce the same byte string (no delimiter ambiguity). This
    /// is the exact input to [`strategy_id_hash`].
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        // Length-framed name.
        let nb = self.name.as_bytes();
        out.extend_from_slice(&(nb.len() as u64).to_le_bytes());
        out.extend_from_slice(nb);
        // Fixed scalars.
        out.extend_from_slice(&self.entry_mode.to_le_bytes());
        out.extend_from_slice(&self.archetype.to_le_bytes());
        out.push(self.sizing.tag());
        // Length-framed parameter vector.
        out.extend_from_slice(&(self.params_fp.len() as u64).to_le_bytes());
        for p in &self.params_fp {
            out.extend_from_slice(&p.to_le_bytes());
        }
        out.extend_from_slice(&self.feature_schema_version.to_le_bytes());
        out
    }
}

/// FNV-1a 64-bit digest over an arbitrary byte slice (leaf primitive).
///
/// `hash = OFFSET; for b in bytes { hash ^= b; hash *= PRIME }`. Multiplication
/// wraps by contract (that is the defined FNV behaviour, not an overflow bug).
/// Deterministic and allocation-free.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// The reproducible `StrategyId` digest of a [`StrategyConfig`].
///
/// Equal configs hash equal; any single-field change changes the digest. This is
/// exactly `fnv1a_64(config.canonical_bytes())`.
pub fn strategy_id_hash(config: &StrategyConfig) -> u64 {
    fnv1a_64(&config.canonical_bytes())
}
