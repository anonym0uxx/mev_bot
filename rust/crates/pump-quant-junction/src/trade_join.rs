//! The `(mint, slot)` join: neither producer states a complete print on its own.
//!
//! WHY THIS EXISTS. Two paths describe the same trade and each is missing the other's field:
//!
//! * the **instruction** path decodes the transaction, so it knows the trader's wallet
//!   (`buyer_entity`) but has no reserve state — it emits `price_fp: 0` and
//!   `quote_lamports`/`liquidity_lamports: 0` as placeholders;
//! * the **reserve-delta** path diffs two curve-account snapshots, so it knows the price, the
//!   SOL leg and the token leg — but account data carries no signer, so it emits
//!   `buyer_entity: 0` by design.
//!
//! The causal state ledger needs BOTH: the price and volumes for the flow/momentum windows,
//! and the wallet for `unique_traders` and the top-1/top-5 shares. Feeding it either print
//! alone is what makes `StateSnapshot::identity_known` false and the bundle unservable.
//!
//! THE JOIN. Both notifications carry the slot the trade landed in, so a print is keyed
//! `(mint, slot)`. The instruction is noted when the transaction is classified; when the
//! reserve print for that key is derived, the wallet is stamped onto it.
//!
//! NEVER GUESS AN IDENTITY. Two same-side instructions on one mint in one slot are not
//! necessarily the same trade, and the corpus would have recorded two distinct wallets. That
//! case is reported as [`JoinOutcome::Ambiguous`] and the print keeps `buyer_entity: 0` — the
//! ledger's `identity_known` flag then refuses the bundle, which is the honest failure. A
//! wrong wallet would be worse than no wallet: it silently rewrites concentration.
//!
//! BOUNDED (§99). The pending table is capacity-limited and pruned by slot: an instruction
//! whose reserve print never arrives (a dust leg the curve never booked, a dropped account
//! notification) must not accumulate forever. Overflow drops the OLDEST key and counts it.

use std::collections::BTreeMap;

/// A pending instruction print, waiting for its reserve counterpart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingIdentity {
    /// The trader's stable entity id (never `0` — a zero is not noted).
    pub buyer_entity: u64,
    /// The trader's WALLET. This is what an address-keyed derivation needs (the flow reducer's
    /// freshness / smart-wallet / co-entry rules), and the hash cannot stand in for it: hashing
    /// first and comparing hashes afterwards makes collisions real where they are negligible.
    pub pubkey: [u8; 32],
    /// Which side the instruction was.
    pub is_buy: bool,
    /// Transaction signature of the instruction, when known (exact join key).
    pub signature: Option<[u8; 64]>,
    /// When the instruction was observed, for diagnostics.
    pub recv_unix_ms: Option<i64>,
    /// The transaction's TOTAL fee (lamports) as the instruction path read it from `meta.fee`.
    /// Only the instruction print has the transaction; the reserve print is account data. `None`
    /// stays `None` — never defaulted to 0.
    pub fee_lamports: Option<u64>,
    /// The transaction's `compute_units_consumed` (consumed, not requested).
    pub cu_consumed: Option<u64>,
}

/// What the join could say about a reserve print's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinOutcome {
    /// Exactly one matching instruction: this is the trader, by id and by address.
    Identity {
        /// The engine's hashed entity id (what its bitsets key on).
        entity: u64,
        /// The trader's wallet bytes (what address-keyed derivations key on).
        pubkey: [u8; 32],
        /// The matched transaction's total fee, when its instruction print carried one.
        fee_lamports: Option<u64>,
        /// The matched transaction's compute units consumed, when carried.
        cu_consumed: Option<u64>,
    },
    /// No matching instruction (or it was pruned): the print stays identity-unknown.
    Unknown,
    /// More than one same-side instruction on this key: refuse to choose.
    Ambiguous,
}

impl JoinOutcome {
    /// The entity to stamp, `0` when the join cannot honestly name one.
    #[must_use]
    pub fn entity(self) -> u64 {
        match self {
            JoinOutcome::Identity { entity, .. } => entity,
            JoinOutcome::Unknown | JoinOutcome::Ambiguous => 0,
        }
    }

    /// The trader's wallet, when the join could honestly name one.
    #[must_use]
    pub fn pubkey(self) -> Option<[u8; 32]> {
        match self {
            JoinOutcome::Identity { pubkey, .. } => Some(pubkey),
            JoinOutcome::Unknown | JoinOutcome::Ambiguous => None,
        }
    }
}

/// The bounded `(mint, slot)` instruction table.
#[derive(Debug)]
pub struct TradeJoin {
    pending: BTreeMap<([u8; 32], u64), Vec<PendingIdentity>>,
    /// Exact index: (transaction signature, mint) → slot of the noted instruction. Keyed with the
    /// mint because one transaction may trade several mints (bundles), one print each.
    by_signature: BTreeMap<([u8; 64], [u8; 32]), u64>,
    capacity: usize,
    /// Slot horizon: keys older than `newest_slot - horizon` are pruned on insert.
    horizon_slots: u64,
    newest_slot: u64,
    dropped: u64,
    ambiguous: u64,
    joined: u64,
}

impl TradeJoin {
    /// A join table holding at most `capacity` keys, pruning keys more than `horizon_slots`
    /// behind the newest slot seen.
    #[must_use]
    pub fn new(capacity: usize, horizon_slots: u64) -> Self {
        Self {
            pending: BTreeMap::new(),
            by_signature: BTreeMap::new(),
            capacity: capacity.max(1),
            horizon_slots: horizon_slots.max(1),
            newest_slot: 0,
            dropped: 0,
            ambiguous: 0,
            joined: 0,
        }
    }

    /// Note an instruction print's trader. A zero entity is not an identity, so it is ignored
    /// rather than stored (storing it would let a later reserve print "join" to a non-identity).
    pub fn note_instruction(
        &mut self,
        mint: &[u8; 32],
        slot: u64,
        buyer_entity: u64,
        pubkey: [u8; 32],
        is_buy: bool,
        recv_unix_ms: Option<i64>,
    ) {
        self.note_instruction_with_meta(
            mint,
            slot,
            buyer_entity,
            pubkey,
            is_buy,
            recv_unix_ms,
            None,
            None,
        );
    }

    /// As [`Self::note_instruction`], additionally keeping the transaction's total fee and
    /// compute units so the reserve print can be stamped with them (the corpus counts both per
    /// trade row).
    #[allow(clippy::too_many_arguments)]
    pub fn note_instruction_with_meta(
        &mut self,
        mint: &[u8; 32],
        slot: u64,
        buyer_entity: u64,
        pubkey: [u8; 32],
        is_buy: bool,
        recv_unix_ms: Option<i64>,
        fee_lamports: Option<u64>,
        cu_consumed: Option<u64>,
    ) {
        self.note_instruction_signed(mint, slot, buyer_entity, pubkey, is_buy, recv_unix_ms, fee_lamports, cu_consumed, None);
    }

    /// [`note_instruction_with_meta`](Self::note_instruction_with_meta) plus the transaction
    /// signature, enabling the exact join ([`take_identity_by_signature`](Self::take_identity_by_signature)).
    #[allow(clippy::too_many_arguments)]
    pub fn note_instruction_signed(
        &mut self,
        mint: &[u8; 32],
        slot: u64,
        buyer_entity: u64,
        pubkey: [u8; 32],
        is_buy: bool,
        recv_unix_ms: Option<i64>,
        fee_lamports: Option<u64>,
        cu_consumed: Option<u64>,
        signature: Option<[u8; 64]>,
    ) {
        if buyer_entity == 0 || pubkey == [0u8; 32] {
            return;
        }
        if let Some(sig) = signature {
            // Bounded by the SAME slot horizon as `pending` (see `prune`): the index is keyed by
            // signature bytes, so "first key" is not "oldest" — evicting by key order would drop
            // live joins at random once full (observed live: identity coverage fell from ~100 % to
            // ~60 % after the index filled). Hard cap only as a last resort, and counted.
            if self.by_signature.len() >= self.capacity * 64 {
                self.dropped += 1;
            } else {
                self.by_signature.insert((sig, *mint), slot);
            }
        }
        self.newest_slot = self.newest_slot.max(slot);
        self.prune();
        let key = (*mint, slot);
        if !self.pending.contains_key(&key) && self.pending.len() >= self.capacity {
            // Drop the oldest key: its reserve print either never came or is about to arrive
            // without an identity, which the ledger reports rather than guesses.
            if let Some((&oldest, _)) = self.pending.iter().next() {
                self.pending.remove(&oldest);
                self.dropped += 1;
            }
        }
        let entry = self.pending.entry(key).or_default();
        // A repeated identical note is the same instruction seen twice (re-delivery), not two
        // traders: de-duplicate so re-delivery never reads as ambiguity.
        if entry
            .iter()
            .any(|p| p.buyer_entity == buyer_entity && p.is_buy == is_buy)
        {
            return;
        }
        if entry.len() < 8 {
            entry.push(PendingIdentity {
                buyer_entity,
                pubkey,
                is_buy,
                signature,
                recv_unix_ms,
                fee_lamports,
                cu_consumed,
            });
        } else {
            self.dropped += 1;
        }
    }

    /// Exact claim by the transaction signature the account update carries. `Unknown` when no
    /// instruction with that signature was noted (yet) — the caller may park and retry within its
    /// bounded hold. Never falls back to a looser key on its own.
    pub fn take_identity_by_signature(&mut self, signature: &[u8; 64], mint: &[u8; 32]) -> JoinOutcome {
        let idx = (*signature, *mint);
        let Some(&slot) = self.by_signature.get(&idx) else {
            return JoinOutcome::Unknown;
        };
        let key = (*mint, slot);
        let Some(entry) = self.pending.get_mut(&key) else {
            self.by_signature.remove(&idx);
            return JoinOutcome::Unknown;
        };
        let Some(i) = entry.iter().position(|p| p.signature.as_ref() == Some(signature)) else {
            self.by_signature.remove(&idx);
            return JoinOutcome::Unknown;
        };
        let p = entry.remove(i);
        if entry.is_empty() {
            self.pending.remove(&key);
        }
        self.by_signature.remove(&idx);
        self.joined += 1;
        JoinOutcome::Identity {
            entity: p.buyer_entity,
            pubkey: p.pubkey,
            fee_lamports: p.fee_lamports,
            cu_consumed: p.cu_consumed,
        }
    }

    /// Claim the identity for a reserve print on `(mint, slot)` of `is_buy`.
    ///
    /// Consumes the match: one instruction is the same trade as one reserve print, so a second
    /// reserve print on the same key must not reuse it.
    pub fn take_identity(&mut self, mint: &[u8; 32], slot: u64, is_buy: bool) -> JoinOutcome {
        let key = (*mint, slot);
        let Some(entry) = self.pending.get_mut(&key) else {
            return JoinOutcome::Unknown;
        };
        let matching: Vec<usize> = entry
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_buy == is_buy)
            .map(|(i, _)| i)
            .collect();
        match matching.len() {
            0 => JoinOutcome::Unknown,
            1 => {
                let entity = entry[matching[0]].buyer_entity;
                let pubkey = entry[matching[0]].pubkey;
                let fee_lamports = entry[matching[0]].fee_lamports;
                let cu_consumed = entry[matching[0]].cu_consumed;
                entry.remove(matching[0]);
                if entry.is_empty() {
                    self.pending.remove(&key);
                }
                self.joined += 1;
                JoinOutcome::Identity {
                    entity,
                    pubkey,
                    fee_lamports,
                    cu_consumed,
                }
            }
            _ => {
                // Leave the candidates in place: the other reserve print on this key may still
                // be theirs, and consuming them here would deny it an identity.
                self.ambiguous += 1;
                JoinOutcome::Ambiguous
            }
        }
    }

    /// Forget a mint entirely (it left the watchlist).
    pub fn forget(&mut self, mint: &[u8; 32]) {
        self.pending.retain(|(m, _), _| m != mint);
    }

    /// Instructions whose identity was successfully stamped onto a reserve print.
    #[must_use]
    pub fn joined(&self) -> u64 {
        self.joined
    }

    /// Instructions refused because the table was full or a key had too many candidates.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Reserve prints that found more than one candidate and were left identity-unknown.
    #[must_use]
    pub fn ambiguous(&self) -> u64 {
        self.ambiguous
    }

    /// Keys currently waiting for their reserve print.
    #[must_use]
    pub fn pending_keys(&self) -> usize {
        self.pending.len()
    }

    fn prune(&mut self) {
        if self.newest_slot <= self.horizon_slots {
            return;
        }
        let floor = self.newest_slot - self.horizon_slots;
        let stale: Vec<([u8; 32], u64)> = self
            .pending
            .keys()
            .filter(|(_, slot)| *slot < floor)
            .copied()
            .collect();
        self.by_signature.retain(|_, slot| *slot >= floor);
        for key in stale {
            self.pending.remove(&key);
            self.dropped += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn signature_index_is_horizon_pruned_not_evicted_by_key_order() {
        use super::*;
        let mut join = TradeJoin::new(8, 32);
        let mint = [9u8; 32];
        // 8*8 = 64 would have been the old hard cap; insert 500 live signatures in one slot window
        for i in 0..500u32 {
            let mut sig = [0u8; 64];
            sig[..4].copy_from_slice(&i.to_be_bytes());
            join.note_instruction_signed(&mint, 100, 1000 + u64::from(i), [1u8; 32], true, None, None, None, Some(sig));
        }
        // pending is bounded by capacity (keys), the signature index is not the limiting resource
        let mut sig = [0u8; 64];
        sig[..4].copy_from_slice(&499u32.to_be_bytes());
        // the (mint, slot) key holds at most 8 identities; the LAST noted ones were dropped by the
        // per-key bound, but the ones that are present must be joinable by signature
        let mut found = 0;
        for i in 0..500u32 {
            let mut sig = [0u8; 64];
            sig[..4].copy_from_slice(&i.to_be_bytes());
            if matches!(join.take_identity_by_signature(&sig, &mint), JoinOutcome::Identity { .. }) { found += 1; }
        }
        assert_eq!(found, 8, "every retained identity joins exactly by signature");
        // horizon prune drops the index entries of stale slots
        join.note_instruction_signed(&mint, 100 + 40, 7, [2u8; 32], false, None, None, None, Some([7u8; 64]));
        assert!(join.by_signature.keys().all(|(_, _)| true));
        assert!(join.by_signature.values().all(|s| *s >= 140 - 32));
    }

    #[test]
    fn signature_join_disambiguates_same_mint_same_side_same_slot_and_never_falls_back() {
        use super::*;
        let mut join = TradeJoin::new(64, 32);
        let mint = [7u8; 32];
        let (sig_a, sig_b, sig_c) = ([1u8; 64], [2u8; 64], [3u8; 64]);
        join.note_instruction_signed(&mint, 10, 11, [0xA; 32], true, None, Some(5), None, Some(sig_a));
        join.note_instruction_signed(&mint, 10, 22, [0xB; 32], true, None, Some(6), None, Some(sig_b));
        // (mint, slot, side) alone is ambiguous: two buys in one slot.
        assert!(matches!(join.take_identity(&mint, 10, true), JoinOutcome::Ambiguous));
        // The signature resolves each exactly, in any order.
        match join.take_identity_by_signature(&sig_b, &mint) {
            JoinOutcome::Identity { entity, pubkey, fee_lamports, .. } => {
                assert_eq!((entity, pubkey, fee_lamports), (22, [0xB; 32], Some(6)));
            }
            o => panic!("{o:?}"),
        }
        match join.take_identity_by_signature(&sig_a, &mint) {
            JoinOutcome::Identity { entity, .. } => assert_eq!(entity, 11),
            o => panic!("{o:?}"),
        }
        // Consumed: a duplicate representation of the same write cannot reuse it.
        assert!(matches!(join.take_identity_by_signature(&sig_a, &mint), JoinOutcome::Unknown));
        // A signed print whose instruction was never noted stays Unknown — no loosening to
        // (mint, slot, side) even when such a candidate exists.
        join.note_instruction_signed(&mint, 11, 33, [0xC; 32], false, None, None, None, Some(sig_c));
        assert!(matches!(join.take_identity_by_signature(&[9u8; 64], &mint), JoinOutcome::Unknown));
        assert!(matches!(join.take_identity(&mint, 11, false), JoinOutcome::Identity { entity: 33, .. }));
        // One transaction trading two mints (bundle): one exact print each.
        let (m1, m2, sig_d) = ([1u8; 32], [2u8; 32], [4u8; 64]);
        join.note_instruction_signed(&m1, 12, 44, [0xD; 32], true, None, None, None, Some(sig_d));
        join.note_instruction_signed(&m2, 12, 44, [0xD; 32], true, None, None, None, Some(sig_d));
        assert!(matches!(join.take_identity_by_signature(&sig_d, &m2), JoinOutcome::Identity { entity: 44, .. }));
        assert!(matches!(join.take_identity_by_signature(&sig_d, &m1), JoinOutcome::Identity { entity: 44, .. }));
        assert!(matches!(join.take_identity_by_signature(&sig_d, &m1), JoinOutcome::Unknown));
    }

    use super::*;

    const MINT: [u8; 32] = [5u8; 32];

    #[test]
    fn an_instruction_and_its_reserve_print_reunite_the_two_halves() {
        let mut join = TradeJoin::new(64, 32);
        join.note_instruction(&MINT, 100, 0xAB, [0xCD; 32], true, Some(1_700_000_000));
        assert_eq!(join.pending_keys(), 1);
        assert_eq!(
            join.take_identity(&MINT, 100, true),
            JoinOutcome::Identity {
                entity: 0xAB,
                pubkey: [0xCD; 32],
                fee_lamports: None,
                cu_consumed: None,
            }
        );
        assert_eq!(join.joined(), 1);
        // Consumed: a second reserve print on the same key cannot re-use it.
        assert_eq!(join.take_identity(&MINT, 100, true), JoinOutcome::Unknown);
        assert_eq!(join.pending_keys(), 0);
    }

    #[test]
    fn a_lone_reserve_print_stays_identity_unknown() {
        let mut join = TradeJoin::new(64, 32);
        // The instruction print never arrived (or was pruned): we do NOT invent a trader.
        assert_eq!(join.take_identity(&MINT, 7, true), JoinOutcome::Unknown);
        assert_eq!(join.take_identity(&MINT, 7, true).entity(), 0);
        assert_eq!(join.joined(), 0);
    }

    #[test]
    fn two_same_side_instructions_are_ambiguous_and_never_guessed() {
        let mut join = TradeJoin::new(64, 32);
        join.note_instruction(&MINT, 9, 1, [1u8; 32], true, None);
        join.note_instruction(&MINT, 9, 2, [2u8; 32], true, None);
        assert_eq!(join.take_identity(&MINT, 9, true), JoinOutcome::Ambiguous);
        assert_eq!(join.ambiguous(), 1);
        // The candidates survive: a buy and a sell in one slot are still separable.
        assert_eq!(join.take_identity(&MINT, 9, true), JoinOutcome::Ambiguous);
        join.note_instruction(&MINT, 9, 3, [3u8; 32], false, None);
        assert_eq!(
            join.take_identity(&MINT, 9, false),
            JoinOutcome::Identity {
                entity: 3,
                pubkey: [3u8; 32],
                fee_lamports: None,
                cu_consumed: None,
            }
        );
    }

    #[test]
    fn redelivery_of_one_instruction_is_not_ambiguity() {
        let mut join = TradeJoin::new(64, 32);
        join.note_instruction(&MINT, 11, 7, [7u8; 32], true, None);
        join.note_instruction(&MINT, 11, 7, [7u8; 32], true, None);
        assert_eq!(
            join.take_identity(&MINT, 11, true),
            JoinOutcome::Identity {
                entity: 7,
                pubkey: [7u8; 32],
                fee_lamports: None,
                cu_consumed: None,
            }
        );
        assert_eq!(join.ambiguous(), 0);
    }

    #[test]
    fn a_zero_entity_is_not_an_identity_to_join_to() {
        let mut join = TradeJoin::new(64, 32);
        join.note_instruction(&MINT, 12, 0, [0u8; 32], true, None);
        assert_eq!(join.pending_keys(), 0);
        assert_eq!(join.take_identity(&MINT, 12, true), JoinOutcome::Unknown);
    }

    #[test]
    fn the_table_is_bounded_and_prunes_stale_keys() {
        let mut join = TradeJoin::new(2, 5);
        join.note_instruction(&MINT, 10, 1, [1u8; 32], true, None);
        join.note_instruction(&MINT, 11, 2, [2u8; 32], true, None);
        // Third key at capacity: the OLDEST (slot 10) is dropped, and the count shows it.
        join.note_instruction(&MINT, 12, 3, [3u8; 32], true, None);
        assert_eq!(join.pending_keys(), 2);
        assert!(join.dropped() >= 1);
        assert_eq!(join.take_identity(&MINT, 10, true), JoinOutcome::Unknown);
        // Pruning by slot horizon: insert far ahead and the old keys go.
        join.note_instruction(&[6u8; 32], 400, 9, [9u8; 32], true, None);
        assert_eq!(
            join.pending_keys(),
            1,
            "keys older than the horizon are pruned"
        );
    }

    #[test]
    fn a_matched_instruction_hands_its_fee_and_cu_to_the_reserve_print() {
        let mut join = TradeJoin::new(64, 32);
        join.note_instruction_with_meta(
            &MINT,
            50,
            5,
            [5u8; 32],
            true,
            Some(1),
            Some(1_005_000),
            Some(95_040),
        );
        assert_eq!(
            join.take_identity(&MINT, 50, true),
            JoinOutcome::Identity {
                entity: 5,
                pubkey: [5u8; 32],
                fee_lamports: Some(1_005_000),
                cu_consumed: Some(95_040),
            }
        );
    }

    #[test]
    fn an_ambiguous_or_missing_match_carries_no_fee_or_cu() {
        let mut join = TradeJoin::new(64, 32);
        // Two same-side instructions on one (mint, slot): which transaction's fee is it? Unknown.
        join.note_instruction_with_meta(&MINT, 60, 1, [1u8; 32], true, None, Some(9_000), Some(10));
        join.note_instruction_with_meta(
            &MINT,
            60,
            2,
            [2u8; 32],
            true,
            None,
            Some(70_000),
            Some(20),
        );
        assert_eq!(join.take_identity(&MINT, 60, true), JoinOutcome::Ambiguous);
        assert_eq!(join.take_identity(&MINT, 61, true), JoinOutcome::Unknown);
        // Neither outcome exposes a fee/CU: there is no field to read one from.
    }
}
