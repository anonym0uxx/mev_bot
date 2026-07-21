//! Shared builders for the canonicalizer leaf tests.

use pump_quant_canonical::{
    DeliveryMode, Provider, Signature, SourceClass, TransactionObservation, TxClaim,
};

// Re-export DeliveryMode etc. is unnecessary; tests import from the crate directly.

/// A signature whose 64 bytes are all `n` — a distinct, orderable test identity.
pub fn sig(n: u8) -> Signature {
    Signature::new([n; 64])
}

/// A 32-byte payload hash filled with `n`.
pub fn phash(n: u8) -> [u8; 32] {
    [n; 32]
}

/// Builds an observation with an empty claim and benign provenance defaults.
/// Callers set `.claim`, `.reconstructed_time_ns`, etc. as needed.
pub fn base(
    observation_id: u64,
    signature: Signature,
    source_class: SourceClass,
    provider: Provider,
    delivery_mode: DeliveryMode,
    receive_time_ns: u64,
) -> TransactionObservation {
    TransactionObservation {
        observation_id,
        signature,
        source_class,
        provider,
        delivery_mode,
        receive_time_ns,
        reconstructed_time_ns: None,
        provider_timestamp_ns: None,
        source_sequence: None,
        connection_epoch: 0,
        payload_hash: phash(observation_id as u8),
        claim: TxClaim::default(),
    }
}
