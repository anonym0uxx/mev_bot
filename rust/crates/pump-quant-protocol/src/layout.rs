//! Versioned account layouts, on-chain fixture parity, and the gate that makes
//! an unverified layout **unbuildable**.
//!
//! ## Why this module exists (the 2026-08-02 falsification)
//! `venue_accounts` shipped the bonding-curve `buy` list as a hardcoded 17-push
//! sequence, sourced from `VENUE_TX_LAYOUTS.md` §4.1 and corroborated against
//! the published IDL. A live-chain check found **18** accounts in 100% of
//! sampled transactions. The builder was wrong, and every document and IDL it
//! was built from agreed with it.
//!
//! The defect is not the missing account. It is the *shape*: the pump program
//! has appended accounts to `buy` at least four times (creator_vault replacing
//! the rent sysvar; the two volume accumulators; fee_config + fee_program;
//! bonding_curve_v2), and a hardcoded list cannot notice the fifth. It goes
//! stale **silently**, and the failure surfaces as a failed transaction with
//! real capital rather than as a red test.
//!
//! An IDL does not close this. The IDL's *named* account list stops before the
//! trailing `remaining_accounts`, so a program can add a required trailing
//! account without the IDL's named list changing at all. That is exactly what
//! happened: `bonding_curve_v2` is not in the IDL's named list either.
//!
//! ## The control
//! A layout is identified by [`LayoutKey`] and may only be built if the
//! [`LayoutRegistry`] holds a [`VerifiedLayout`] for it — a record carrying the
//! **slot and signature of a real successful mainnet transaction** the builder
//! was proven byte-identical against. An empty registry builds nothing.
//!
//! This inverts the previous default. Before: build unless something objects,
//! with the caveat living in a doc comment. Now: refuse unless a fixture
//! proves the layout, with the proof carrying its own provenance. A doc comment
//! reading "STATUS: UNVERIFIED ON-CHAIN" does not stop a caller; a
//! `Result::Err` does.
//!
//! ## What this does NOT claim
//! Verification is per `(venue, side, variant)` and pinned to a slot. It says a
//! layout matched the chain at that slot. It does not predict the next program
//! upgrade. [`VerifiedLayout::verifying_slot`] exists so staleness is
//! measurable and a re-verification cadence can be enforced, not so it can be
//! claimed once and forgotten.
//!
//! ## Constitution
//! * §18.2 — fail closed; account identity from decoded evidence, never a
//!   document's assertion.
//! * §22 — integer only, deterministic, no I/O.
//! * criterion 77(a) — [`diff_layout`] is the byte-level differential; it
//!   compares pubkeys **and** signer/writable flags, in order.

use crate::venue_accounts::AccountMeta;

/// Which venue program an instruction targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Venue {
    /// pump.fun bonding curve.
    PumpFun,
    /// PumpSwap AMM.
    PumpSwap,
}

/// Which side of the trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// Buy.
    Buy,
    /// Sell.
    Sell,
}

/// The account-count-affecting state of the market being traded.
///
/// These are the permutations that change the account list. Each is a
/// **decoded on-chain fact**, never inferred: cashback is byte 82 of the
/// bonding curve / byte 244 of the pool; the token program is the mint
/// account's owner; the quote mint is a field, not an assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Variant {
    /// `is_cashback_coin`. Inserts a writable `user_volume_accumulator` on a
    /// bonding-curve sell.
    pub cashback: bool,
    /// The mint is owned by Token-2022 rather than spl-token. Changes every
    /// ATA address (the token program is an ATA seed), so it changes the
    /// account *values* even where it does not change the count.
    pub token_2022: bool,
    /// The market's quote mint is not native SOL (USDC-quoted curves exist).
    pub non_sol_quote: bool,
    /// PumpSwap only: the traded token sits on the pool's **quote** side.
    /// Empirically the majority case (~81%), and it flips which discriminator
    /// expresses the trade.
    pub reversed_pool: bool,
}

impl Variant {
    /// The plain case: SOL-quoted, spl-token, no cashback, normal pool order.
    pub const fn plain() -> Self {
        Self {
            cashback: false,
            token_2022: false,
            non_sol_quote: false,
            reversed_pool: false,
        }
    }
}

/// Identifies one account layout that must be independently verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayoutKey {
    /// Target program.
    pub venue: Venue,
    /// Trade side.
    pub side: Side,
    /// Market state permutation.
    pub variant: Variant,
}

/// A layout proven against a real successful mainnet transaction.
///
/// Constructed only by [`LayoutRegistry::record_verified`], which is the single
/// place provenance is admitted. The signature and slot are mandatory: a
/// verification with no transaction to point at is an assertion, not evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedLayout {
    /// What was verified.
    pub key: LayoutKey,
    /// Account count observed on chain.
    pub account_count: usize,
    /// Slot of the verifying transaction.
    pub verifying_slot: u64,
    /// Signature of the verifying transaction, for re-audit.
    pub verifying_signature: [u8; 64],
}

/// Why a layout could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// No verified fixture exists for this layout. The builder refuses.
    Unverified(LayoutKey),
    /// A fixture exists but its account count disagrees with what the builder
    /// produced. The builder is wrong, or the fixture is stale.
    CountDisagrees {
        key: LayoutKey,
        built: usize,
        verified: usize,
    },
    /// The verifying fixture is older than the caller's staleness budget.
    Stale {
        key: LayoutKey,
        verified_at: u64,
        now: u64,
        max_age_slots: u64,
    },
}

/// How a built account list differs from an observed one, at one position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutDelta {
    /// The lists are different lengths. Reported once, before positional deltas.
    CountMismatch {
        /// What the builder produced.
        built: usize,
        /// What the chain showed.
        observed: usize,
    },
    /// Same position, different account.
    PubkeyMismatch {
        /// Index in the account list.
        index: usize,
        /// Builder's account.
        built: [u8; 32],
        /// Chain's account.
        observed: [u8; 32],
    },
    /// Same account, different signer/writable flags. A writable account built
    /// read-only fails on chain; a read-only account built writable silently
    /// widens the transaction's write-lock set and serialises against every
    /// other writer of it.
    FlagMismatch {
        /// Index in the account list.
        index: usize,
        /// Builder's (is_signer, is_writable).
        built: (bool, bool),
        /// Chain's (is_signer, is_writable).
        observed: (bool, bool),
    },
    /// The chain carried a trailing account the builder never emits. This is
    /// the shape of the 2026-08-02 finding.
    MissingTail {
        /// Index in the observed list.
        index: usize,
        /// The account the builder omitted.
        observed: [u8; 32],
    },
    /// The builder emitted a trailing account the chain does not carry.
    ExtraTail {
        /// Index in the built list.
        index: usize,
        /// The account the builder invented.
        built: [u8; 32],
    },
}

/// One account as observed in a decoded on-chain instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedAccount {
    /// Resolved 32-byte account key. For a v0 transaction this must be the
    /// **resolved** key, with address-lookup-table entries already expanded.
    pub pubkey: [u8; 32],
    /// Whether the message header marks it a signer.
    pub is_signer: bool,
    /// Whether the message header marks it writable.
    pub is_writable: bool,
}

/// Compare a built account list against an observed one, in order.
///
/// Returns every difference rather than the first, so one run produces the
/// whole repair list instead of N round trips. An empty result is byte-level
/// parity across pubkeys, flags and ordering — criterion 77(a).
///
/// Positional comparison stops at the shorter list; the tail beyond it is
/// reported as [`LayoutDelta::MissingTail`] or [`LayoutDelta::ExtraTail`], so a
/// trailing-append upgrade produces a precise, actionable diff rather than a
/// cascade of mismatches.
pub fn diff_layout(built: &[AccountMeta], observed: &[ObservedAccount]) -> Vec<LayoutDelta> {
    let mut out = Vec::new();
    if built.len() != observed.len() {
        out.push(LayoutDelta::CountMismatch {
            built: built.len(),
            observed: observed.len(),
        });
    }
    let common = built.len().min(observed.len());
    for i in 0..common {
        let b = &built[i];
        let o = &observed[i];
        if b.pubkey != o.pubkey {
            out.push(LayoutDelta::PubkeyMismatch {
                index: i,
                built: b.pubkey,
                observed: o.pubkey,
            });
        }
        if b.is_signer != o.is_signer || b.is_writable != o.is_writable {
            out.push(LayoutDelta::FlagMismatch {
                index: i,
                built: (b.is_signer, b.is_writable),
                observed: (o.is_signer, o.is_writable),
            });
        }
    }
    for (i, o) in observed.iter().enumerate().skip(common) {
        out.push(LayoutDelta::MissingTail {
            index: i,
            observed: o.pubkey,
        });
    }
    for (i, b) in built.iter().enumerate().skip(common) {
        out.push(LayoutDelta::ExtraTail {
            index: i,
            built: b.pubkey,
        });
    }
    out
}

/// The set of layouts proven against the chain.
///
/// Fail-closed by construction: a default registry is **empty**, so every
/// `require` call fails until a fixture is recorded. There is deliberately no
/// `allow_all`, no `skip_verification` flag and no `Default` that pre-populates
/// anything — an escape hatch here would be the whole control.
#[derive(Debug, Clone, Default)]
pub struct LayoutRegistry {
    entries: Vec<VerifiedLayout>,
}

impl LayoutRegistry {
    /// An empty registry. Builds nothing until a fixture is recorded.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Record that a layout was proven against a real transaction.
    ///
    /// Refuses an all-zero signature: that is a default, not a transaction, and
    /// admitting it would let a caller manufacture provenance (§18.2).
    /// Re-recording the same key replaces the entry, so a re-verification after
    /// a program upgrade moves the slot forward rather than accumulating
    /// contradictory records.
    pub fn record_verified(&mut self, v: VerifiedLayout) -> Result<(), LayoutError> {
        if v.verifying_signature == [0u8; 64] {
            return Err(LayoutError::Unverified(v.key));
        }
        self.entries.retain(|e| e.key != v.key);
        self.entries.push(v);
        Ok(())
    }

    /// Look up a verified layout.
    pub fn get(&self, key: &LayoutKey) -> Option<&VerifiedLayout> {
        self.entries.iter().find(|e| &e.key == key)
    }

    /// The gate. Returns the verified layout or refuses.
    ///
    /// `built_count` is checked against the fixture, so a builder that drifts
    /// away from a previously-verified layout is caught at build time rather
    /// than on chain.
    pub fn require(
        &self,
        key: &LayoutKey,
        built_count: usize,
    ) -> Result<&VerifiedLayout, LayoutError> {
        let v = self.get(key).ok_or(LayoutError::Unverified(*key))?;
        if v.account_count != built_count {
            return Err(LayoutError::CountDisagrees {
                key: *key,
                built: built_count,
                verified: v.account_count,
            });
        }
        Ok(v)
    }

    /// The gate, plus a staleness bound in slots.
    ///
    /// A layout verified once and trusted forever is the same defect class as a
    /// gate that cannot fail. `max_age_slots` forces re-verification on a
    /// cadence the operator sets.
    pub fn require_fresh(
        &self,
        key: &LayoutKey,
        built_count: usize,
        now_slot: u64,
        max_age_slots: u64,
    ) -> Result<&VerifiedLayout, LayoutError> {
        let v = self.require(key, built_count)?;
        if now_slot.saturating_sub(v.verifying_slot) > max_age_slots {
            return Err(LayoutError::Stale {
                key: *key,
                verified_at: v.verifying_slot,
                now: now_slot,
                max_age_slots,
            });
        }
        Ok(v)
    }

    /// Every layout currently verified.
    pub fn verified(&self) -> &[VerifiedLayout] {
        &self.entries
    }

    /// Which of `required` have no fixture. The coverage report: an empty
    /// result means every permutation the caller cares about is proven.
    pub fn missing(&self, required: &[LayoutKey]) -> Vec<LayoutKey> {
        required
            .iter()
            .copied()
            .filter(|k| self.get(k).is_none())
            .collect()
    }
}

/// The permutation matrix a venue must cover before it can be trusted broadly.
///
/// Enumerating this is the difference between "we verified a buy" and "we
/// verified the buy path". The 2026-08-02 check sampled the plain case and the
/// cashback sell; the rest of this matrix is unproven, and unproven means
/// unbuildable under [`LayoutRegistry`].
pub fn required_layouts(venue: Venue) -> Vec<LayoutKey> {
    let mut out = Vec::new();
    let cashbacks = [false, true];
    let t22s = [false, true];
    // Non-SOL quote and reversed pools are venue-specific dimensions.
    let quotes: &[bool] = &[false, true];
    let reversed: &[bool] = match venue {
        Venue::PumpFun => &[false],
        Venue::PumpSwap => &[false, true],
    };
    for side in [Side::Buy, Side::Sell] {
        for &cashback in &cashbacks {
            for &token_2022 in &t22s {
                for &non_sol_quote in quotes {
                    for &rev in reversed {
                        out.push(LayoutKey {
                            venue,
                            side,
                            variant: Variant {
                                cashback,
                                token_2022,
                                non_sol_quote,
                                reversed_pool: rev,
                            },
                        });
                    }
                }
            }
        }
    }
    out
}
