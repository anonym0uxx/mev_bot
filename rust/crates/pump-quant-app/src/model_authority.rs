//! R3 — Qwen is the entry **authority**; Rust only resolves and vetoes.
//!
//! This module is the plan's §14.3 target architecture, in code:
//!
//! ```text
//! Qwen (free action + size, unfettered inference)
//!         │
//!         ▼
//! this module — resolve the clip, then the capital-safety shell   ← veto only
//!         │
//!         ▼
//! pump-quant-execution routes (buy / add / reduce / exit)
//! ```
//!
//! WHAT THIS DELIBERATELY DOES NOT DO. It never second-guesses the model's *direction*.
//! There is no ranking of candidates, no setup classifier, no hazard gate, no "better"
//! answer — those were the inference-control layer that outranked the model, and they are
//! retired. `Tier` is the model's; the clip is the account's.
//!
//! THE THREE VETOES THAT REMAIN (and why each is execution, not inference):
//! 1. **Fail-closed on anything off-contract.** An unparseable answer, an unreachable
//!    model, or a `BUY` with no size yields *no trade* — never a defaulted tier. A
//!    defaulted tier would be Rust inventing a decision the model did not make.
//! 2. **Payability.** The model may want 1.00 SOL; if the account cannot fund the clip
//!    after the priority-fee buffer, the clip is capped, and refused outright below the
//!    venue minimum.
//! 3. **Bankroll floor.** A trade that would leave the account under its survival floor is
//!    refused. This is capital protection; it never chooses a *different* trade.
//!
//! Every refusal is typed, so telemetry can count causes instead of parsing log text.

use pump_quant_inference::seam::{
    management_base, management_fraction_bps, parse_decision_payload, route, Decision, DriftLedger,
    ManagementBase, OffContract, Route, SizeError, SizeTier, FEE_BUFFER_LAMPORTS,
};
use pump_quant_inference::{
    resolve_clip_at_fraction_bps, EntryVenue, InferenceClient, InferenceError,
};

use crate::portfolio::{Admission, AdmissionRefusal, PortfolioCap};

/// Anything that can answer a completion request — the live llama-server client, or a
/// stub in tests. Kept as a trait so the authority logic is testable without a model.
pub trait ModelSource {
    /// Send the pair of prompts, return the raw completion text.
    fn complete(&self, system: &str, user: &str) -> Result<String, InferenceError>;
}

impl ModelSource for InferenceClient {
    fn complete(&self, system: &str, user: &str) -> Result<String, InferenceError> {
        InferenceClient::complete(self, system, user)
    }
}

/// One entry decision's inputs. The prompts are built by the proposal layer (P1) so this
/// module owns no prompt wording — parity is the renderer's job, not the authority's.
#[derive(Debug, Clone, Copy)]
pub struct EntryRequest<'a> {
    /// The decision family's system prompt (byte-parity with the c11 corpus).
    pub system_prompt: &'a str,
    /// The rendered user prompt for this candidate, as-of-decision.
    pub user_prompt: &'a str,
    /// Free (uncommitted) lamports available to deploy. This is the PAYABLE source only: it
    /// shrinks as positions are committed, so it must never be the sizing basis — that would
    /// make each successive admit smaller than the last, which is order-dependent per-row
    /// sizing of exactly the kind `KELLY_AUDIT_C12` §2 retired.
    pub free_cash_lamports: u64,
    /// The portfolio's sizing basis: the account's deployable capital (balance − survival
    /// floor) for the episode, BEFORE this admit's commitments. Order-independent by
    /// construction — `k_max` positions at FULL deploy exactly this much, and the tenth admit
    /// is sized like the first.
    pub portfolio_deployable_lamports: u64,
    /// Lamports that must remain free after the clip is deployed (survival floor).
    pub bankroll_floor_lamports: u64,
    /// The venue this candidate would trade on — the row meta the ruled size policy reads.
    pub venue: EntryVenue,
    /// The mint under consideration — what "one position per mint episode" is keyed on.
    pub mint: [u8; 32],
    /// The mints with a live position right now: the portfolio layer's whole view.
    pub live_mints: &'a std::collections::BTreeSet<[u8; 32]>,
    /// The portfolio-layer cap in force for this decision.
    pub portfolio: PortfolioCap,
}

/// What the authority concluded. `Buy` is the only variant that may move capital.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAuthority {
    /// The model chose to buy; the clip is what the account can actually pay.
    Buy {
        /// The SERVED size tier — the ruled venue size (AMM FULL / curve SMALL), which is
        /// what the corpus was labelled under. The model's own emitted token is graded
        /// against it in the drift ledger, never used to size.
        tier: SizeTier,
        /// Lamports to deploy, after payability capping.
        clip_lamports: u64,
    },
    /// The model asked to watch the mint; no capital.
    Watch,
    /// The model declined; no capital.
    Skip,
    /// Fail-closed: no trade, with the cause.
    NoTrade(NoTradeReason),
}

/// Why no trade happened. Typed so the causes can be counted separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoTradeReason {
    /// The model could not be reached (transport, timeout, non-200).
    ModelUnreachable,
    /// The completion violated the trained contract (see [`OffContract`]).
    OffContract(OffContract),
    /// The clip the model asked for is not payable from free cash.
    UnpayableClip(SizeError),
    /// The clip would leave the account below its survival floor.
    BreachesBankrollFloor,
    /// The entry prompt was answered with a management verb (ADD/REDUCE/EXIT) — a routing
    /// error by the caller, not an entry decision.
    ManagementVerb,
    /// The portfolio layer refused: the mint already has a live position, or the concurrency
    /// cap is reached (`KELLY_AUDIT_C12`'s exposure control).
    Portfolio(AdmissionRefusal),
}

/// Ask the model, then resolve and veto. The single entry point of R3.
///
/// `ledger` counts off-contract completions by cause (the G4 drift hook): a `BUY` without a
/// size is impossible for a trained model, so seeing one means the prompt contract broke
/// upstream and must page rather than silently default.
pub fn decide_entry<S: ModelSource + ?Sized>(
    source: &S,
    req: &EntryRequest<'_>,
    ledger: &mut DriftLedger,
) -> EntryAuthority {
    let completion = match source.complete(req.system_prompt, req.user_prompt) {
        Ok(c) => c,
        // Unreachable model: no trade. A retry cannot help — temperature 0 returns the same
        // answer — so this is terminal for the decision, not a loop.
        Err(_) => return EntryAuthority::NoTrade(NoTradeReason::ModelUnreachable),
    };

    let decision: Decision = match parse_decision_payload(&completion) {
        Ok(d) => d,
        Err(e) => {
            ledger.record_error(&e);
            return EntryAuthority::NoTrade(NoTradeReason::OffContract(e.kind));
        }
    };

    match route(decision.action) {
        Route::Open => {
            // Portfolio layer first (`KELLY_AUDIT_C12`): whether there is ROOM is a question
            // about the book, not about the completion. Checked before the contract so a mint
            // we already hold is refused for the reason a human needs to read, whatever the
            // model said about its size.
            if let Admission::Refuse(cause) = req.portfolio.admit(req.live_mints, &req.mint) {
                return EntryAuthority::NoTrade(NoTradeReason::Portfolio(cause));
            }
            let Some(model_tier) = decision.size else {
                // The corpus asked for a size on all 13,376 trained BUYs and got one every
                // time. Refuse; never guess.
                ledger.record(OffContract::BuyWithoutSize);
                return EntryAuthority::NoTrade(NoTradeReason::OffContract(
                    OffContract::BuyWithoutSize,
                ));
            };
            // M1 (`KELLY_AUDIT_C12`): the size that SIZES the trade is the ruled venue rule
            // (amm -> FULL, curve -> SMALL), not the token the model emitted. The rule is the
            // label authority the corpus was rebuilt under, so serving it is what makes
            // train==serve structural; the model's own token is an ordinal consent that gets
            // graded against the rule and journalled. A mismatch is drift — counted, not fatal,
            // and never allowed to size the position in either direction.
            let tier = req.venue.ruled_size();
            if model_tier != tier {
                ledger.record(OffContract::BuyWithVenueMismatchedSize);
            }
            // The notional is the DEPLOYABLE budget split across the concurrency cap, not the
            // fixed 1 SOL reference: under the cap a full book is the account's deployable
            // budget rather than a multiple of it, and the survival floor is respected by
            // construction because it is carved out before the split.
            let deployable = req.portfolio_deployable_lamports;
            // No deployable capital means the survival floor is the whole account: a hard
            // veto, and the only way this path may refuse for the floor. On every other path
            // the floor is structural — the clip cannot exceed `deployable / k_max`, so it
            // can never strand the account, and the veto below stays as the belt to that
            // brace rather than as the working limit.
            if deployable == 0 {
                return EntryAuthority::NoTrade(NoTradeReason::BreachesBankrollFloor);
            }
            let notional = req.portfolio.per_position_notional(deployable);
            match resolve_clip_at_fraction_bps(
                req.portfolio.served_fraction_bps(req.venue),
                req.free_cash_lamports,
                notional,
                FEE_BUFFER_LAMPORTS,
            ) {
                Ok(clip_lamports) => {
                    if req.free_cash_lamports.saturating_sub(clip_lamports)
                        < req.bankroll_floor_lamports
                    {
                        return EntryAuthority::NoTrade(NoTradeReason::BreachesBankrollFloor);
                    }
                    ledger.record_accepted();
                    EntryAuthority::Buy {
                        tier,
                        clip_lamports,
                    }
                }
                Err(e) => EntryAuthority::NoTrade(NoTradeReason::UnpayableClip(e)),
            }
        }
        Route::Watch => {
            ledger.record_accepted();
            EntryAuthority::Watch
        }
        Route::NoOp => {
            ledger.record_accepted();
            EntryAuthority::Skip
        }
        // Management verbs are answered on the management family, not the entry prompt.
        Route::AddToPosition | Route::ReducePosition | Route::ClosePosition => {
            EntryAuthority::NoTrade(NoTradeReason::ManagementVerb)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::time::Duration;

    /// A source that returns a canned completion.
    pub(super) struct Stub(pub &'static str);

    impl ModelSource for Stub {
        fn complete(&self, _system: &str, _user: &str) -> Result<String, InferenceError> {
            Ok(self.0.to_string())
        }
    }

    /// A source backed by a real client pointed at a closed port — a genuine unreachable
    /// model, not a manufactured error variant.
    struct Unreachable(InferenceClient);

    impl ModelSource for Unreachable {
        fn complete(&self, system: &str, user: &str) -> Result<String, InferenceError> {
            self.0.complete(system, user)
        }
    }

    fn req(free: u64, floor: u64, venue: EntryVenue) -> EntryRequest<'static> {
        // The portfolio basis is the account's deployable capital, NOT the shrinking free
        // cash: sizing off `free` would make each admit depend on what the previous ones left.
        let deployable = free.saturating_sub(floor);
        EntryRequest {
            system_prompt: "SYSTEM",
            user_prompt: "USER",
            free_cash_lamports: free,
            portfolio_deployable_lamports: deployable,
            bankroll_floor_lamports: floor,
            venue,
            mint: MINT,
            live_mints: no_live(),
            portfolio: PortfolioCap::enforced(3),
        }
    }

    /// The common case in these tests: an AMM candidate (the ruled FULL venue).
    fn amm(free: u64, floor: u64) -> EntryRequest<'static> {
        req(free, floor, EntryVenue::Amm)
    }

    /// The candidate mint these tests trade.
    const MINT: [u8; 32] = [7u8; 32];

    /// The empty live book, `'static` so a request can borrow it.
    fn no_live() -> &'static BTreeSet<[u8; 32]> {
        static EMPTY: std::sync::OnceLock<BTreeSet<[u8; 32]>> = std::sync::OnceLock::new();
        EMPTY.get_or_init(BTreeSet::new)
    }

    /// A live book holding `mints`. Leaks a set per call — tests only.
    fn live_of(mints: &[[u8; 32]]) -> &'static BTreeSet<[u8; 32]> {
        Box::leak(Box::new(mints.iter().copied().collect::<BTreeSet<_>>()))
    }

    /// Real corpus completions, verbatim shapes from the c11 fixtures.
    pub(super) const BUY_FULL: &str = "DECISION: BUY\nSIZE: FULL\nPRICE LIMIT: 0.02445740498411998\nINVALIDATION: exit if net_flow_lamports turns negative\nEVIDENCE: round-trip cost floor 92 bp must be cleared";
    const BUY_SMALL: &str = "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.02\nINVALIDATION: none\nEVIDENCE: flow sustained";
    const BUY_NO_SIZE: &str =
        "DECISION: BUY\nPRICE LIMIT: 0.02\nINVALIDATION: none\nEVIDENCE: flow sustained";
    const WATCH: &str = "DECISION: WATCH\nSIZE: NONE\nINVALIDATION: re-evaluate when net_flow_lamports > 0\nEVIDENCE: net_flow_lamports is not decisively positive";
    const SKIP: &str = "DECISION: SKIP\nSIZE: NONE\nINVALIDATION: none for this snapshot\nEVIDENCE: 52 sells vs 738 buys; top1 share 0.4";
    pub(super) const GARBAGE: &str = "I think this looks pretty good, maybe buy a little?";
    pub(super) const ADD: &str =
        "DECISION: ADD\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: scale in";
    pub(super) const REDUCE: &str =
        "DECISION: REDUCE\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: trim";
    pub(super) const EXIT: &str =
        "DECISION: EXIT\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: close it";
    pub(super) const HOLD: &str =
        "DECISION: HOLD\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: nothing to manage here";

    #[test]
    fn a_buy_deploys_the_ruled_size_of_the_per_position_notional() {
        let mut l = DriftLedger::new();
        let a = decide_entry(&Stub(BUY_FULL), &amm(2_000_000_000, 0), &mut l);
        // FULL = 100% of the per-position notional, and the notional is the deployable
        // budget (2 SOL) split across the cap (3): 666,666,666 lamports.
        assert_eq!(
            a,
            EntryAuthority::Buy {
                tier: SizeTier::Full,
                clip_lamports: 666_666_666
            }
        );
        assert_eq!(l.accepted(), 1);
    }

    #[test]
    fn cash_caps_the_clip_but_never_enlarges_it() {
        let mut l = DriftLedger::new();
        // A curve candidate: the ruled size is SMALL = 0.25 SOL of the 1 SOL notional, and
        // an account holding 5 SOL does not make it bigger. Rust resolves the ruled size; it
        // never scales it upward.
        let a = decide_entry(
            &Stub(BUY_SMALL),
            &req(5_000_000_000, 0, EntryVenue::BondingCurve),
            &mut l,
        );
        assert_eq!(
            a,
            EntryAuthority::Buy {
                tier: SizeTier::Small,
                clip_lamports: 416_666_666
            }
        );
        // And the fee buffer is respected: 0.30 SOL free is split three ways, so FULL gets
        // 0.10 SOL — not the 0.25 SOL left after the buffer.
        let a = decide_entry(&Stub(BUY_FULL), &amm(300_000_000, 0), &mut l);
        assert_eq!(
            a,
            EntryAuthority::Buy {
                tier: SizeTier::Full,
                clip_lamports: 100_000_000
            }
        );
    }

    /// M1 (`KELLY_AUDIT_C12`): the venue rule sizes the trade. A model token that disagrees
    /// is drift — counted and visible — and it moves the clip in neither direction.
    #[test]
    fn the_venue_rule_sizes_the_trade_not_the_models_token() {
        let mut l = DriftLedger::new();
        // SMALL emitted on an AMM candidate: the ruled AMM size is FULL, and FULL deploys.
        let a = decide_entry(&Stub(BUY_SMALL), &amm(2_000_000_000, 0), &mut l);
        assert_eq!(
            a,
            EntryAuthority::Buy {
                tier: SizeTier::Full,
                clip_lamports: 666_666_666
            }
        );
        assert_eq!(l.count(OffContract::BuyWithVenueMismatchedSize), 1);
        // The agreeing case stays silent.
        let mut l2 = DriftLedger::new();
        let _ = decide_entry(&Stub(BUY_FULL), &amm(2_000_000_000, 0), &mut l2);
        assert_eq!(l2.count(OffContract::BuyWithVenueMismatchedSize), 0);
    }

    #[test]
    fn a_buy_without_a_size_is_no_trade_and_flags_the_drift_alarm() {
        let mut l = DriftLedger::new();
        let a = decide_entry(&Stub(BUY_NO_SIZE), &amm(2_000_000_000, 0), &mut l);
        assert_eq!(
            a,
            EntryAuthority::NoTrade(NoTradeReason::OffContract(OffContract::BuyWithoutSize))
        );
        assert!(l.buy_without_size_seen());
        assert_eq!(l.count(OffContract::BuyWithoutSize), 1);
        assert_eq!(l.accepted(), 0);
    }

    #[test]
    fn garbage_and_unreachable_models_both_fail_closed() {
        let mut l = DriftLedger::new();
        let a = decide_entry(&Stub(GARBAGE), &amm(2_000_000_000, 0), &mut l);
        assert!(matches!(
            a,
            EntryAuthority::NoTrade(NoTradeReason::OffContract(_))
        ));

        let dead = Unreachable(InferenceClient::new(
            "http://127.0.0.1:1",
            Duration::from_millis(50),
        ));
        let a = decide_entry(&dead, &amm(2_000_000_000, 0), &mut l);
        assert_eq!(a, EntryAuthority::NoTrade(NoTradeReason::ModelUnreachable));
    }

    /// The audit retired per-row sizing because it is not an exposure control; the same
    /// argument kills a *shrinking* basis. An admit into an empty book and one into a
    /// half-committed book must size identically, or the last admit is systematically smallest.
    #[test]
    fn the_per_position_notional_does_not_shrink_with_the_book() {
        let mut l = DriftLedger::new();
        let empty = decide_entry(&Stub(BUY_FULL), &amm(2_000_000_000, 0), &mut l);
        // The SAME portfolio basis, but half the cash is already committed to live positions.
        // Payability still covers the clip, so the two admits must be identical: if the basis
        // were free cash, the second would come out smaller purely because it came second.
        let mut half_committed = amm(1_000_000_000, 0);
        half_committed.portfolio_deployable_lamports = 2_000_000_000;
        let later = decide_entry(&Stub(BUY_FULL), &half_committed, &mut l);
        assert_eq!(empty, later, "book occupancy must not size the next row");
        assert_eq!(
            empty,
            EntryAuthority::Buy {
                tier: SizeTier::Full,
                clip_lamports: 666_666_666
            }
        );
        // When cash genuinely cannot cover the slot, PAYABILITY caps the clip. That is a
        // different mechanism from sizing, and the only one allowed to shrink a row.
        let mut short = amm(500_000_000, 0);
        short.portfolio_deployable_lamports = 2_000_000_000;
        assert_eq!(
            decide_entry(&Stub(BUY_FULL), &short, &mut l),
            EntryAuthority::Buy {
                tier: SizeTier::Full,
                clip_lamports: 450_000_000
            },
            "the payable cap binds; the notional does not move"
        );
    }

    #[test]
    fn the_floor_is_structural_and_vetoes_only_when_nothing_is_deployable() {
        let mut l = DriftLedger::new();
        // 1 SOL free with a 0.9 SOL floor: the deployable 0.1 SOL is split across the cap, so
        // the trade SHRINKS to fit the floor instead of being refused. That is the audit's
        // point — the floor is an input to the notional, not a post-hoc clamp.
        let a = decide_entry(&Stub(BUY_FULL), &amm(1_000_000_000, 900_000_000), &mut l);
        let EntryAuthority::Buy { clip_lamports, .. } = a else {
            panic!("expected a buy sized to fit the floor, got {a:?}");
        };
        assert_eq!(clip_lamports, 33_333_333);
        // The invariant the structural floor exists to guarantee.
        assert!(1_000_000_000u64.saturating_sub(clip_lamports) >= 900_000_000);
        // Free cash at or below the floor is the one hard veto: nothing is deployable.
        let a = decide_entry(&Stub(BUY_FULL), &amm(1_000_000_000, 1_200_000_000), &mut l);
        assert_eq!(
            a,
            EntryAuthority::NoTrade(NoTradeReason::BreachesBankrollFloor)
        );
        let a = decide_entry(&Stub(BUY_FULL), &amm(1_000_000_000, 1_000_000_000), &mut l);
        assert_eq!(
            a,
            EntryAuthority::NoTrade(NoTradeReason::BreachesBankrollFloor)
        );
    }

    #[test]
    fn unwatchable_cash_is_refused_not_rounded_up() {
        let mut l = DriftLedger::new();
        // 0.10 SOL free split three ways is a 0.033 SOL notional; a quarter of that is
        // 0.008 SOL, under the 0.01 SOL minimum → refused, never rounded up.
        let a = decide_entry(
            &Stub(BUY_SMALL),
            &req(100_000_000, 0, EntryVenue::BondingCurve),
            &mut l,
        );
        assert!(matches!(
            a,
            EntryAuthority::NoTrade(NoTradeReason::UnpayableClip(_))
        ));
        // 0.13 SOL free clears it: a 0.043 SOL notional, a quarter of which is 0.0108 SOL.
        let a = decide_entry(
            &Stub(BUY_SMALL),
            &req(130_000_000, 0, EntryVenue::BondingCurve),
            &mut l,
        );
        assert_eq!(
            a,
            EntryAuthority::Buy {
                tier: SizeTier::Small,
                clip_lamports: 10_833_333
            }
        );
    }

    /// `KELLY_AUDIT_C12`'s exposure control: one position per mint episode, and the cap on
    /// the book — refused with the cause, never silently.
    #[test]
    fn the_portfolio_layer_refuses_a_held_mint_and_a_full_book() {
        let mut l = DriftLedger::new();
        let mut r = amm(2_000_000_000, 0);
        r.live_mints = live_of(&[MINT]);
        let a = decide_entry(&Stub(BUY_FULL), &r, &mut l);
        assert_eq!(
            a,
            EntryAuthority::NoTrade(NoTradeReason::Portfolio(AdmissionRefusal::AlreadyHeld))
        );
        let mut r = amm(2_000_000_000, 0);
        r.live_mints = live_of(&[[1u8; 32], [2u8; 32], [3u8; 32]]);
        let a = decide_entry(&Stub(BUY_FULL), &r, &mut l);
        assert_eq!(
            a,
            EntryAuthority::NoTrade(NoTradeReason::Portfolio(AdmissionRefusal::CapReached))
        );
        // Two live out of three: there is room, and it deploys.
        let mut r = amm(2_000_000_000, 0);
        r.live_mints = live_of(&[[1u8; 32], [2u8; 32]]);
        let a = decide_entry(&Stub(BUY_FULL), &r, &mut l);
        assert!(matches!(a, EntryAuthority::Buy { .. }));
    }

    /// The unenforceable-cap fallback: uniform half — a Rust-chosen FRACTION, not the
    /// retired MID tier the model may name.
    #[test]
    fn an_unenforceable_cap_sizes_uniformly_at_half() {
        let mut l = DriftLedger::new();
        let mut r = amm(2_000_000_000, 0);
        r.portfolio = PortfolioCap::unenforceable(3);
        let a = decide_entry(&Stub(BUY_FULL), &r, &mut l);
        // Half of the 666,666,666 per-position notional.
        assert_eq!(
            a,
            EntryAuthority::Buy {
                tier: SizeTier::Full,
                clip_lamports: 333_333_333
            }
        );
    }

    #[test]
    fn watch_and_skip_pass_through_without_capital() {
        let mut l = DriftLedger::new();
        assert_eq!(
            decide_entry(&Stub(WATCH), &amm(2_000_000_000, 0), &mut l),
            EntryAuthority::Watch
        );
        assert_eq!(
            decide_entry(&Stub(SKIP), &amm(2_000_000_000, 0), &mut l),
            EntryAuthority::Skip
        );
        assert_eq!(l.accepted(), 2);
        assert_eq!(l.total(), 0);
    }

    #[test]
    fn a_management_verb_on_the_entry_prompt_is_a_routing_error() {
        let mut l = DriftLedger::new();
        // EXIT/ADD/REDUCE are answered on the management family; on an entry prompt they are
        // a caller routing error, not an entry decision.
        assert_eq!(
            decide_entry(&Stub(EXIT), &amm(2_000_000_000, 0), &mut l),
            EntryAuthority::NoTrade(NoTradeReason::ManagementVerb)
        );
        // HOLD on an empty book is the seam's NoOp — a deliberate non-trade, not an error.
        assert_eq!(
            decide_entry(&Stub(HOLD), &amm(2_000_000_000, 0), &mut l),
            EntryAuthority::Skip
        );
        assert_eq!(l.accepted(), 1);
    }
}

// ---------------------------------------------------------------------------------------
// OPEN-POSITION MANAGEMENT — the same authority boundary, applied to a position already held.
// ---------------------------------------------------------------------------------------

/// One management decision's inputs, at a management clock.
#[derive(Debug, Clone, Copy)]
pub struct ManagementRequest<'a> {
    /// The `management_replay` family's system prompt (byte-parity with the c10 corpus).
    pub system_prompt: &'a str,
    /// The rendered management prompt for this position, as-of-decision.
    pub user_prompt: &'a str,
}

/// What the authority concluded about a held position.
///
/// MAGNITUDE UNITS. The management family was trained **without** a size line, so the model
/// returns an action only. The magnitude is therefore the corpus's own: `ADD` scales in by
/// half the current inventory, `REDUCE` trims half, `EXIT` closes all. Carrying it as basis
/// points of inventory (rather than absolute tokens) keeps this module free of price math —
/// the engine applies the fraction to the inventory it actually holds, in integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagementAuthority {
    /// Do nothing this clock. Keeps the position open — and keeps every safety trigger armed.
    Hold,
    /// `ADD` — scale in by the corpus fraction of the ACCOUNT's capital.
    ///
    /// The field NAMES its unit on purpose: a generic name is how the ~4x defect got
    /// in. `ADD` is a fraction of capital, `REDUCE` a fraction of inventory, and one
    /// shared name let the two be swapped silently.
    ScaleIn {
        /// Basis points of ACCOUNT CAPITAL committed (5_000 = +50% of the account).
        account_fraction_bps: u32,
    },
    /// `REDUCE` — trim the corpus fraction of current inventory.
    Trim {
        /// Basis points of current inventory to close (5_000 = -50%).
        inventory_fraction_bps: u32,
    },
    /// `EXIT` — close the whole position.
    CloseAll,
    /// Fail-closed: no management action was taken.
    NoAction(ManagementNoAction),
}

/// Why a management clock produced no action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagementNoAction {
    /// The model could not be reached.
    ModelUnreachable,
    /// The completion violated the trained contract.
    OffContract(OffContract),
    /// The management prompt was answered with an entry verb (`BUY`/`WATCH`/`SKIP`).
    EntryVerbOnManagementPrompt,
}

/// Ask the model what to do with a position we already hold.
///
/// WHY A PARSE FAILURE HOLDS RATHER THAN CUTS. On the entry path, fail-closed means *no
/// trade* — the safe direction is inaction. On an open position, inaction is not free: it
/// keeps inventory exposed. But cutting on a parse error would be Rust inventing a decision
/// the model did not make — precisely the inference-control layer this work retires, and it
/// would fire on a truncated response rather than on a reason. So a failure holds the
/// position **and leaves every execution-safety trigger armed** (exit ladder, rug precursor,
/// circuit breaker), which is the correct division: the model decides *direction*, the safety
/// layer decides *when survival overrides*.
///
/// PRECEDENCE, stated once: a safety trigger may always reduce exposure and may never
/// increase it. Nothing here can veto a cut; nothing here can force one.
pub fn decide_management<S: ModelSource + ?Sized>(
    source: &S,
    req: &ManagementRequest<'_>,
    ledger: &mut DriftLedger,
) -> ManagementAuthority {
    let completion = match source.complete(req.system_prompt, req.user_prompt) {
        Ok(c) => c,
        Err(_) => return ManagementAuthority::NoAction(ManagementNoAction::ModelUnreachable),
    };

    let decision: Decision = match parse_decision_payload(&completion) {
        Ok(d) => d,
        Err(e) => {
            ledger.record_error(&e);
            return ManagementAuthority::NoAction(ManagementNoAction::OffContract(e.kind));
        }
    };

    match route(decision.action) {
        Route::NoOp => {
            ledger.record_accepted();
            ManagementAuthority::Hold
        }
        Route::AddToPosition => match (
            management_fraction_bps(decision.action),
            management_base(decision.action),
        ) {
            // The unit is DERIVED from the seam, never restated here: an `ADD` that
            // resolves against inventory must not be expressible.
            (Some(bps), Some(ManagementBase::AccountCapital)) => {
                ledger.record_accepted();
                ManagementAuthority::ScaleIn {
                    account_fraction_bps: bps,
                }
            }
            // Unreachable while the seam is the single source of the magnitudes; refusing
            // beats inventing one if that ever changes. Also catches a base that is not
            // AccountCapital, so an ADD cannot quietly resolve against inventory.
            _ => ManagementAuthority::NoAction(ManagementNoAction::OffContract(
                OffContract::UntrainedAction,
            )),
        },
        Route::ReducePosition => match management_fraction_bps(decision.action) {
            Some(bps) => {
                ledger.record_accepted();
                ManagementAuthority::Trim {
                    inventory_fraction_bps: bps,
                }
            }
            None => ManagementAuthority::NoAction(ManagementNoAction::OffContract(
                OffContract::UntrainedAction,
            )),
        },
        Route::ClosePosition => {
            ledger.record_accepted();
            ManagementAuthority::CloseAll
        }
        // An entry verb on the management prompt is a caller routing error.
        Route::Open | Route::Watch => {
            ManagementAuthority::NoAction(ManagementNoAction::EntryVerbOnManagementPrompt)
        }
    }
}

#[cfg(test)]
mod management_tests {
    use super::tests::*;
    use super::*;

    fn mreq() -> ManagementRequest<'static> {
        ManagementRequest {
            system_prompt: "SYSTEM",
            user_prompt: "USER",
        }
    }

    #[test]
    fn hold_keeps_the_position_and_the_vocabulary() {
        let mut l = DriftLedger::new();
        assert_eq!(
            decide_management(&Stub(HOLD), &mreq(), &mut l),
            ManagementAuthority::Hold
        );
        assert_eq!(l.accepted(), 1);
    }

    #[test]
    fn add_and_reduce_carry_the_corpus_magnitude_in_basis_points() {
        let mut l = DriftLedger::new();
        assert_eq!(
            decide_management(&Stub(ADD), &mreq(), &mut l),
            ManagementAuthority::ScaleIn {
                account_fraction_bps: 5_000
            }
        );
        assert_eq!(
            decide_management(&Stub(REDUCE), &mreq(), &mut l),
            ManagementAuthority::Trim {
                inventory_fraction_bps: 5_000
            }
        );
        assert_eq!(
            decide_management(&Stub(EXIT), &mreq(), &mut l),
            ManagementAuthority::CloseAll
        );
    }

    #[test]
    fn a_parse_failure_holds_and_never_invents_a_cut() {
        let mut l = DriftLedger::new();
        let a = decide_management(&Stub(GARBAGE), &mreq(), &mut l);
        assert!(matches!(
            a,
            ManagementAuthority::NoAction(ManagementNoAction::OffContract(_))
        ));
        // And it is counted, so a broken management contract pages rather than drifts.
        assert_eq!(l.total(), 1);
        assert_eq!(l.accepted(), 0);
    }

    #[test]
    fn an_entry_verb_on_the_management_prompt_is_a_routing_error() {
        let mut l = DriftLedger::new();
        assert_eq!(
            decide_management(&Stub(BUY_FULL), &mreq(), &mut l),
            ManagementAuthority::NoAction(ManagementNoAction::EntryVerbOnManagementPrompt)
        );
    }
}
