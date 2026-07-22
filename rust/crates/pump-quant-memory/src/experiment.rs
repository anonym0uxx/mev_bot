//! Sealed-experiment immutability logic (§56.1, §56.4, §56.9).
//!
//! Responsibility: implement the "seal → hash → reject mutation" contract on
//! [`Experiment`]. Registered experiments are immutable once sealed: sealing
//! records a deterministic content fingerprint, the safe mutation API then refuses
//! every change, and [`Experiment::verify_integrity`] recomputes the fingerprint
//! so any out-of-band tamper (e.g. a direct write to a public field) is detected.
//! This is the property tests rely on: *sealed segments immutable* and *future
//! events cannot alter past decisions* (§59 property tests).

use crate::hashing::{fnv1a_64, push_bytes, push_u32, push_u64, SealHash};
use crate::rows::Experiment;

/// Errors returned by the sealing API (§56.1). Every variant is a refusal, never
/// a silent no-op — an attempt to mutate sealed research is a governance error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExperimentError {
    /// `seal` was called on an already-sealed experiment.
    AlreadySealed,
    /// A content-mutating operation was attempted after sealing (immutability).
    SealedImmutable,
}

impl core::fmt::Display for ExperimentError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            ExperimentError::AlreadySealed => "experiment is already sealed",
            ExperimentError::SealedImmutable => "sealed experiment is immutable",
        };
        f.write_str(s)
    }
}

impl std::error::Error for ExperimentError {}

impl Experiment {
    /// Canonical, deterministic byte encoding of the experiment's *content
    /// identity* — every persisted field except the seal flag and the stored hash
    /// itself. This is the pre-image of the [`SealHash`]. Field order and
    /// length-prefixing are fixed so the encoding is unambiguous and stable
    /// across platforms (§22, §56.9).
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        push_u64(&mut buf, self.id.0);
        push_u64(&mut buf, self.hypothesis_id.0);
        push_u32(&mut buf, self.schema_version);
        push_bytes(&mut buf, &self.title_hash);
        push_bytes(&mut buf, &self.causal_mechanism_hash);
        push_bytes(&mut buf, &self.dataset_hash);
        push_u64(&mut buf, self.config_hash);
        push_u64(&mut buf, self.created_at_ns);
        buf
    }

    /// Compute the content fingerprint of the current field values (independent of
    /// whether the experiment is sealed).
    #[must_use]
    pub fn compute_seal_hash(&self) -> SealHash {
        SealHash(fnv1a_64(&self.canonical_bytes()))
    }

    /// Seal the experiment: fingerprint the current content and record it
    /// (§56.1). Idempotency is deliberately refused — resealing is
    /// [`ExperimentError::AlreadySealed`], because a second seal would silently
    /// re-baseline mutated content and defeat immutability.
    ///
    /// # Errors
    /// [`ExperimentError::AlreadySealed`] if already sealed.
    pub fn seal(&mut self) -> Result<SealHash, ExperimentError> {
        if self.sealed {
            return Err(ExperimentError::AlreadySealed);
        }
        let hash = self.compute_seal_hash();
        self.seal_hash = Some(hash);
        self.sealed = true;
        Ok(hash)
    }

    /// Safe mutation of the dataset manifest fingerprint. Permitted only while
    /// unsealed; a sealed experiment refuses (§56.4 — no post-hoc change to a
    /// registered experiment).
    ///
    /// # Errors
    /// [`ExperimentError::SealedImmutable`] if the experiment is sealed.
    pub fn set_dataset_hash(
        &mut self,
        dataset_hash: crate::rows::ContentHash,
    ) -> Result<(), ExperimentError> {
        if self.sealed {
            return Err(ExperimentError::SealedImmutable);
        }
        self.dataset_hash = dataset_hash;
        Ok(())
    }

    /// Safe mutation of the config hash. Permitted only while unsealed.
    ///
    /// # Errors
    /// [`ExperimentError::SealedImmutable`] if the experiment is sealed.
    pub fn set_config_hash(&mut self, config_hash: u64) -> Result<(), ExperimentError> {
        if self.sealed {
            return Err(ExperimentError::SealedImmutable);
        }
        self.config_hash = config_hash;
        Ok(())
    }

    /// Verify a sealed experiment has not been tampered with: recompute the
    /// fingerprint over the current field values and compare it to the hash
    /// recorded at seal time.
    ///
    /// Returns `true` only for a sealed experiment whose current content still
    /// matches its seal hash. An unsealed experiment returns `false` (there is
    /// nothing to verify against), and a sealed experiment whose public fields
    /// were mutated out-of-band returns `false` — that is the tamper-evidence
    /// guarantee (§56.1, §59 *sealed segments immutable*).
    #[must_use]
    pub fn verify_integrity(&self) -> bool {
        match self.seal_hash {
            Some(recorded) if self.sealed => self.compute_seal_hash() == recorded,
            _ => false,
        }
    }
}
