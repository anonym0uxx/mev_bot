//! Reproducible, domain-separated strategy and evaluator configuration hashes.
//!
//! ## Responsibility
//! Compute the [`StrategyHash`] and [`EvaluatorReleaseHash`] recorded per
//! strategy version in the StrategyRegistry (§56.3), and the evaluator release
//! digest pinned trust-on-first-use (§44, §62 "auto-pinned … any subsequent
//! mismatch is Tier-0"). A hash is a stable digest of a
//! [`CanonicalValue`](crate::canonical::CanonicalValue) config: identical
//! configs always hash identically (§19 reproducibility), and any change to any
//! field changes the digest.
//!
//! ## Domain separation
//! The same config hashed as a *strategy* versus an *evaluator release* must
//! never collide, so each digest is `SHA-256(domain_tag || canonical_encoding)`
//! with a distinct constant tag per domain. This prevents a config from being
//! replayed across governance roles with the same identity.
//!
//! ## §22 compliance
//! Integer-only throughout ([`crate::sha256`]); no floating point.

use crate::canonical::CanonicalValue;
use crate::sha256;

/// Domain tag mixed in before a strategy config. Stable constant — changing it
/// re-keys every historical StrategyHash, so it is versioned in the string.
pub const STRATEGY_DOMAIN: &[u8] = b"pump_quant_governance.strategy_hash.v1";

/// Domain tag mixed in before an evaluator-release config.
pub const EVALUATOR_DOMAIN: &[u8] = b"pump_quant_governance.evaluator_release_hash.v1";

/// A reproducible strategy-configuration digest (§56.3 `StrategyHash`).
///
/// Two `StrategyHash` values are equal iff their source configs encode to the
/// same canonical bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StrategyHash(pub [u8; 32]);

/// A reproducible evaluator-release digest (§56.3 `EvaluatorReleaseHash`, §44
/// trust-on-first-use pinning).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvaluatorReleaseHash(pub [u8; 32]);

impl StrategyHash {
    /// Lower-case hex form, for registry records and logs (not outcome logic).
    pub fn to_hex(&self) -> String {
        sha256::to_hex(&self.0)
    }
}

impl EvaluatorReleaseHash {
    /// Lower-case hex form, for manifest records and logs (not outcome logic).
    pub fn to_hex(&self) -> String {
        sha256::to_hex(&self.0)
    }
}

/// Digest of `config` under `domain`: `SHA-256(domain || canonical(config))`.
///
/// Shared, deterministic core of both governance hashes. The domain tag is
/// length-prefixed so it cannot be confused with the start of the canonical
/// body (defense against domain/body boundary ambiguity).
fn domain_digest(domain: &[u8], config: &CanonicalValue) -> [u8; 32] {
    let mut hasher = sha256::Sha256::new();
    // Length-prefix the domain (fixed 8-byte big-endian) for injectivity.
    hasher.update(&(domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    hasher.update(&config.encode());
    hasher.finalize()
}

/// Compute the [`StrategyHash`] of a strategy configuration.
///
/// ## Constitution §56.3
/// Deterministic: byte-equivalent configs (any map order) map to equal hashes.
pub fn strategy_hash(config: &CanonicalValue) -> StrategyHash {
    StrategyHash(domain_digest(STRATEGY_DOMAIN, config))
}

/// Compute the [`EvaluatorReleaseHash`] of an evaluator-release configuration.
///
/// ## Constitution §56.3 / §44
/// Deterministic and domain-separated from [`strategy_hash`]; the same config
/// yields a different digest in each domain.
pub fn evaluator_release_hash(config: &CanonicalValue) -> EvaluatorReleaseHash {
    EvaluatorReleaseHash(domain_digest(EVALUATOR_DOMAIN, config))
}
