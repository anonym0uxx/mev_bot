//! THE engine books: the single place that reads or writes `bankroll_realized` / `bankroll_committed`.
//!
//! * `paper_fill_v1_observed` (default, unarmed): the engine's own fields ARE the books (unchanged behaviour).
//! * `paper_fill_v2_shadow` (armed): the [`crate::settlement::SettlementLedger`] IS the books. Every read
//!   ([`Engine::books_realized`], [`Engine::books_committed`]) returns the ledger's value and the v1 fields are
//!   never written. A booking site that runs under v2 must be inside a declared settlement scope (the code path
//!   that settles the same increment through the ledger); one outside a scope is a BYPASS and latches the named
//!   settlement fault `books_bypass:<site>` (SAFETY_OFF `settlement_fault`). Nothing is silently dropped.
//!
//! `tests/books_single_path.rs` scans the sources: no other file may name these two fields outside the struct
//! declaration and constructor.
#![forbid(unsafe_code)]

use super::Engine;

/// Every site that books cash/committed capital (the inventory in proc/SLICE_wire_v3_REPORT.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookSite {
    /// `open_pending`: committed += entry cost.
    EntryOpen,
    /// `book_exit`: realized += net.
    ExitRealize,
    /// `book_exit`: committed release on a full close.
    ExitRelease,
    /// `model_mgmt_book`: partial sell releases committed pro rata.
    ReduceRelease,
    /// `model_mgmt_book_add`: committed += ADD all-in cost.
    AddCommit,
    /// `model_resolve_recon_fault`: authority says the order never filled; its committed cost is released.
    ReconUnwind,
    /// `rollback_unsubmitted_buy`: a buy that never reached the venue (live sink only).
    SinkRollback,
    /// `model_held_apply`: restore realized + committed from the held ledger.
    Restore,
}

impl BookSite {
    const fn bit(self) -> u16 {
        1 << (self as u16)
    }

    /// Stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::EntryOpen => "entry_open",
            Self::ExitRealize => "exit_realize",
            Self::ExitRelease => "exit_release",
            Self::ReduceRelease => "reduce_release",
            Self::AddCommit => "add_commit",
            Self::ReconUnwind => "recon_unwind",
            Self::SinkRollback => "sink_rollback",
            Self::Restore => "restore",
        }
    }
}

/// Where the engine's books come from.
pub const BOOKS_SOURCE_V1: &str = "engine_fields_v1";
/// Where the engine's books come from under v2.
pub const BOOKS_SOURCE_LEDGER: &str = "settlement_ledger";

impl Engine {
    /// Realized PnL of the books, lamports (v2: the settlement ledger's; v1: the engine field).
    #[must_use]
    pub(super) fn books_realized(&self) -> i128 {
        match self.model_pf.settle.as_ref() {
            Some(l) => l.realized,
            None => self.bankroll_realized,
        }
    }

    /// Committed capital of the books, lamports (v2: the settlement ledger's; v1: the engine field).
    #[must_use]
    pub(super) fn books_committed(&self) -> u128 {
        match self.model_pf.settle.as_ref() {
            Some(l) => u128::try_from(l.committed.max(0)).unwrap_or(u128::MAX),
            None => self.bankroll_committed,
        }
    }

    /// Which store the books are read from.
    #[must_use]
    pub fn model_books_source(&self) -> &'static str {
        if self.model_pf.settle.is_some() {
            BOOKS_SOURCE_LEDGER
        } else {
            BOOKS_SOURCE_V1
        }
    }

    /// Booking attempts under v2 that were outside a settlement scope (each one latched `books_bypass:<site>`).
    #[must_use]
    pub fn model_books_bypasses(&self) -> u64 {
        self.model_pf.bypasses
    }

    /// Engine-side exit nets summed under v2 (diagnostic only; the ledger's realized is the book).
    #[must_use]
    pub fn model_books_exit_net_diag(&self) -> i128 {
        self.model_pf.exit_net_diag
    }

    /// Run `f` as a declared settlement scope for exactly `sites`: bookings at those sites inside it are settled by
    /// the caller through the ledger. A booking at any OTHER site inside `f` is still a bypass.
    pub(super) fn books_scoped<R>(
        &mut self,
        sites: &[BookSite],
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let outer = self.model_pf.book_scope;
        self.model_pf.book_scope = outer | sites.iter().fold(0u16, |m, s| m | s.bit());
        let r = f(self);
        self.model_pf.book_scope = outer;
        r
    }

    /// v2 guard. `true` = the v1 fields must be written (v1). `false` = the ledger owns the booking (v2); outside a
    /// scope that is a bypass and latches the named settlement fault.
    fn books_v1_or_check(&mut self, site: BookSite) -> bool {
        if self.model_pf.settle.is_none() {
            return true;
        }
        if self.model_pf.book_scope & site.bit() == 0 {
            self.model_pf.bypasses += 1;
            self.model_settle_fault(format!("books_bypass:{}", site.label()));
        }
        false
    }

    /// Book realized PnL at `site`.
    pub(super) fn books_add_realized(&mut self, site: BookSite, net: i128) {
        if self.books_v1_or_check(site) {
            self.bankroll_realized = self.bankroll_realized.saturating_add(net);
        } else {
            self.model_pf.exit_net_diag = self.model_pf.exit_net_diag.saturating_add(net);
        }
    }

    /// Commit capital at `site`.
    pub(super) fn books_commit(&mut self, site: BookSite, amt: u64) {
        if self.books_v1_or_check(site) {
            self.bankroll_committed = self.bankroll_committed.saturating_add(u128::from(amt));
        }
    }

    /// Release committed capital at `site`.
    pub(super) fn books_release(&mut self, site: BookSite, amt: u64) {
        if self.books_v1_or_check(site) {
            self.bankroll_committed = self.bankroll_committed.saturating_sub(u128::from(amt));
        }
    }

    /// Restore the v1 books from a held ledger. Under v2 the books are DERIVED: the settlement ledger restored from
    /// the `paper_fill` section is the book (`model_pf_validate` refuses a realized total that differs from it).
    pub(super) fn books_restore(&mut self, realized: i128, committed_add: u64) {
        if self.model_pf.settle.is_none() {
            self.bankroll_realized = realized;
            self.bankroll_committed = self
                .bankroll_committed
                .saturating_add(u128::from(committed_add));
        }
    }

    /// Fresh-engine check for restore.
    #[must_use]
    pub(super) fn books_are_fresh(&self) -> bool {
        self.bankroll_realized == 0
            && self.bankroll_committed == 0
            && self
                .model_pf
                .settle
                .as_ref()
                .is_none_or(|l| l.realized == 0 && l.committed == 0 && l.holdings.is_empty())
    }

    /// TEST HOOK (integration tests only, behind `cfg(any(test, feature))`-free public name): book `amt` at `site`
    /// OUTSIDE any settlement scope, exactly as a future un-routed booking would. Under v2 this must latch
    /// `books_bypass:<site>`; under v1 it writes the engine field.
    #[doc(hidden)]
    pub fn books_unscoped_commit_probe(&mut self, site: BookSite, amt: u64) {
        self.books_commit(site, amt);
    }

    /// TEST HOOK: the RAW v1 engine fields `(bankroll_realized, bankroll_committed)`, whatever the books source.
    /// Under v2 they must stay exactly as they were before arming (the previous booking path never books a fill);
    /// `tests/paper_fill_v2_wiring.rs::v2_previous_booking_path_never_books_a_fill_no_double_booking` pins it.
    #[doc(hidden)]
    #[must_use]
    pub fn books_v1_fields_raw_probe(&self) -> (i128, u128) {
        (self.bankroll_realized, self.bankroll_committed)
    }

    #[cfg(test)]
    pub(super) fn books_set_committed_for_test(&mut self, v: u128) {
        self.bankroll_committed = v;
    }

    #[cfg(test)]
    pub(super) fn books_committed_field_for_test(&self) -> u128 {
        self.bankroll_committed
    }
}
