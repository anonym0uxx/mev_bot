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
    /// When the instruction was observed, for diagnostics.
    pub recv_unix_ms: Option<i64>,
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
        if buyer_entity == 0 || pubkey == [0u8; 32] {
            return;
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
                recv_unix_ms,
            });
        } else {
            self.dropped += 1;
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
                entry.remove(matching[0]);
                if entry.is_empty() {
                    self.pending.remove(&key);
                }
                self.joined += 1;
                JoinOutcome::Identity { entity, pubkey }
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
        for key in stale {
            self.pending.remove(&key);
            self.dropped += 1;
        }
    }
}

#[cfg(test)]
mod tests {
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
                pubkey: [0xCD; 32]
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
                pubkey: [3u8; 32]
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
                pubkey: [7u8; 32]
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
}
