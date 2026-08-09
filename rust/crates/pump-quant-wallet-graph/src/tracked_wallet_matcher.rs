//! Tracked wallet matcher — O(1) lookup over the curated candidate list.
//!
//! This module implements the **wallet identity pipeline** (G1/G2 fix): given a
//! 32-byte pubkey extracted from a LaserStream transaction, determine whether it
//! belongs to the curated candidate list of known memecoin trading whales and
//! pump.fun dev deploy wallets.
//!
//! The candidate list is a **watch list**, not a trust list — inclusion means
//! the wallet's activity is observed and routed through the algo for
//! determination, NOT that the wallet is followed automatically. The §28 PnL
//! truth screen gates "followable" status.
//!
//! ## Determinism (§22)
//!
//! No floating-point, no RNG, no wall-clock, no external deps. The matcher is
//! a pure lookup. The entity-id hash (`wallet_entity_id`) uses splitmix64
//! (same as the LaserStream adapter) for deterministic `u64` handles.

use std::collections::HashMap;

/// Per-wallet tier in the tracking taxonomy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackedWalletTier {
    /// T0 — Proven whale with verified PnL edge via §28 lagged-shadow.
    Proven,
    /// T1 — Dev deploy wallet observed across multiple launches.
    DevDeploy,
    /// T2 — Suspected smart money (heuristically identified).
    SuspectedSmartMoney,
    /// T3 — Candidate whale from the curated list (default).
    Candidate,
    /// T4 — Unverified. Activity logged, no signal weight.
    Unverified,
}

impl Default for TrackedWalletTier {
    fn default() -> Self {
        Self::Candidate
    }
}

/// Metadata for a single tracked wallet.
#[derive(Clone, Debug)]
pub struct TrackedWalletInfo {
    /// The wallet's 32-byte pubkey (raw, not base58).
    pub pubkey: [u8; 32],
    /// Human-readable name from the curated list.
    pub name: String,
    /// Tier classification (T0-T4).
    pub tier: TrackedWalletTier,
}

/// O(1) matcher over the curated candidate wallet list.
///
/// Constructed from decoded pubkeys at daemon startup. The `contains` and
/// `contains_entity` methods are the hot-path entry points called for every
/// LaserStream transaction — both must be O(1).
#[derive(Debug, Default)]
pub struct TrackedWalletMatcher {
    /// Raw pubkey → info map. O(1) lookup by 32-byte pubkey.
    map: HashMap<[u8; 32], TrackedWalletInfo>,
    /// Pre-computed entity-id → tier map. O(1) lookup by u64 entity id.
    /// This is the critical hot-path: the LaserStream adapter already computed
    /// `buyer_entity` as a `u64`, so this avoids re-hashing on every trade.
    entity_ids: HashMap<u64, TrackedWalletTier>,
}

impl TrackedWalletMatcher {
    /// Construct an empty matcher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct from a slice of decoded pubkeys. All wallets get the default
    /// `Candidate` tier. This is the primary constructor used by the wallet
    /// loader at daemon startup.
    #[must_use]
    pub fn from_pubkeys(pubkeys: &[[u8; 32]]) -> Self {
        let mut m = Self::new();
        for pk in pubkeys {
            m.insert(*pk, String::new(), TrackedWalletTier::Candidate);
        }
        m
    }

    /// Register a tracked wallet. Overwrites any existing entry for the same
    /// pubkey. Also updates the entity-id fast-path map.
    pub fn insert(&mut self, pubkey: [u8; 32], name: String, tier: TrackedWalletTier) {
        let entity_id = wallet_entity_id(&pubkey);
        self.entity_ids.insert(entity_id, tier);
        self.map.insert(pubkey, TrackedWalletInfo { pubkey, name, tier });
    }

    /// O(1) check: is this pubkey in the tracked list?
    #[must_use]
    pub fn contains(&self, pubkey: &[u8; 32]) -> bool {
        self.map.contains_key(pubkey)
    }

    /// O(1) lookup: return the info for this pubkey, if tracked.
    pub fn get(&self, pubkey: &[u8; 32]) -> Option<&TrackedWalletInfo> {
        self.map.get(pubkey)
    }

    /// O(1) fast-path check by entity-id (the u64 hash from the LaserStream
    /// adapter's `wallet_entity_id` function). This avoids re-hashing the
    /// 32-byte pubkey on every trade event.
    #[must_use]
    pub fn contains_entity(&self, entity_id: u64) -> bool {
        entity_id != 0 && self.entity_ids.contains_key(&entity_id)
    }

    /// O(1) tier lookup by entity-id.
    pub fn tier_of_entity(&self, entity_id: u64) -> Option<TrackedWalletTier> {
        if entity_id == 0 {
            return None;
        }
        self.entity_ids.get(&entity_id).copied()
    }

    /// Number of tracked wallets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Is the matcher empty?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Iterated view of all tracked pubkeys (for persistence / debugging).
    pub fn iter(&self) -> impl Iterator<Item = (&[u8; 32], &TrackedWalletInfo)> {
        self.map.iter()
    }
}

/// Deterministic `u64` entity id from a 32-byte pubkey.
///
/// Uses splitmix64 mixing — the SAME algorithm as the LaserStream adapter's
/// `wallet_entity_id` function — so the entity-id produced here matches the
/// `buyer_entity` field in `MarketTrade` events exactly. This is critical for
/// the hot-path: the engine can look up a tracked wallet by `buyer_entity`
/// without ever re-decoding the 32-byte pubkey.
#[must_use]
pub fn wallet_entity_id(pubkey: &[u8; 32]) -> u64 {
    let lo = u64::from_le_bytes(pubkey[..8].try_into().unwrap_or([0; 8]));
    let hi = u64::from_le_bytes(pubkey[24..32].try_into().unwrap_or([0; 8]));
    let mut z = lo.wrapping_add(hi);
    z = z.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let z = (z >> (z >> 61).wrapping_add(4)) ^ z;
    let z = z.wrapping_mul(0xC2B9_5A82_79D4_CEA2);
    let z = (z >> (z >> 61).wrapping_add(4)) ^ z;
    z.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_id_consistency() {
        let pk = [0x42; 32];
        let id1 = wallet_entity_id(&pk);
        let id2 = wallet_entity_id(&pk);
        assert_eq!(id1, id2);
        assert_ne!(id1, 0);
    }

    #[test]
    fn test_entity_id_distinct() {
        assert_ne!(
            wallet_entity_id(&[0x42; 32]),
            wallet_entity_id(&[0x43; 32])
        );
    }

    #[test]
    fn test_matcher_basic() {
        let mut m = TrackedWalletMatcher::new();
        let pk = [0x42; 32];
        m.insert(pk, "Test Whale".to_string(), TrackedWalletTier::Candidate);
        assert!(m.contains(&pk));
        assert_eq!(m.len(), 1);

        let eid = wallet_entity_id(&pk);
        assert!(m.contains_entity(eid));
        assert_eq!(m.tier_of_entity(eid), Some(TrackedWalletTier::Candidate));
    }

    #[test]
    fn test_entity_id_zero_never_matches() {
        let m = TrackedWalletMatcher::new();
        assert!(!m.contains_entity(0));
    }

    #[test]
    fn test_matcher_not_found() {
        let m = TrackedWalletMatcher::new();
        assert!(!m.contains(&[0x99; 32]));
        assert!(!m.contains_entity(999));
        assert!(m.is_empty());
    }
}
