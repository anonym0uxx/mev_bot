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
    management_fraction_bps, parse_decision_payload, resolve_entry_clip_lamports, route, Decision,
    DriftLedger, OffContract, Route, SizeError, SizeTier, DEPLOY_LAMPORTS_CANONICAL,
    FEE_BUFFER_LAMPORTS,
};
use pump_quant_inference::{InferenceClient, InferenceError};

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
    /// Free (uncommitted) lamports available to deploy.
    pub free_cash_lamports: u64,
    /// Lamports that must remain free after the clip is deployed (survival floor).
    pub bankroll_floor_lamports: u64,
}

/// What the authority concluded. `Buy` is the only variant that may move capital.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAuthority {
    /// The model chose to buy; the clip is what the account can actually pay.
    Buy {
        /// The model's own size tier (kept for the journal — it is the model's answer).
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
            let Some(tier) = decision.size else {
                // The corpus asked for a size on all 13,376 trained BUYs and got one every
                // time. Refuse; never guess.
                ledger.record(OffContract::BuyWithoutSize);
                return EntryAuthority::NoTrade(NoTradeReason::OffContract(
                    OffContract::BuyWithoutSize,
                ));
            };
            match resolve_entry_clip_lamports(
                tier,
                req.free_cash_lamports,
                DEPLOY_LAMPORTS_CANONICAL,
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

    fn req(free: u64, floor: u64) -> EntryRequest<'static> {
        EntryRequest {
            system_prompt: "SYSTEM",
            user_prompt: "USER",
            free_cash_lamports: free,
            bankroll_floor_lamports: floor,
        }
    }

    /// Real corpus completions, verbatim shapes from the c11 fixtures.
    pub(super) const BUY_FULL: &str = "DECISION: BUY\nSIZE: FULL\nPRICE LIMIT: 0.02445740498411998\nINVALIDATION: exit if net_flow_lamports turns negative\nEVIDENCE: round-trip cost floor 92 bp must be cleared";
    const BUY_SMALL: &str = "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.02\nINVALIDATION: none\nEVIDENCE: flow sustained";
    const BUY_NO_SIZE: &str = "DECISION: BUY\nPRICE LIMIT: 0.02\nINVALIDATION: none\nEVIDENCE: flow sustained";
    const WATCH: &str = "DECISION: WATCH\nSIZE: NONE\nINVALIDATION: re-evaluate when net_flow_lamports > 0\nEVIDENCE: net_flow_lamports is not decisively positive";
    const SKIP: &str = "DECISION: SKIP\nSIZE: NONE\nINVALIDATION: none for this snapshot\nEVIDENCE: 52 sells vs 738 buys; top1 share 0.4";
    pub(super) const GARBAGE: &str = "I think this looks pretty good, maybe buy a little?";
    pub(super) const ADD: &str = "DECISION: ADD\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: scale in";
    pub(super) const REDUCE: &str = "DECISION: REDUCE\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: trim";
    pub(super) const EXIT: &str = "DECISION: EXIT\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: close it";
    pub(super) const HOLD: &str = "DECISION: HOLD\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: nothing to manage here";

    #[test]
    fn a_buy_deploys_the_models_own_tier_capped_by_payable_cash() {
        let mut l = DriftLedger::new();
        let a = decide_entry(&Stub(BUY_FULL), &req(2_000_000_000, 0), &mut l);
        // FULL = 100% of the 1 SOL canonical notional; cash 2 SOL so nothing caps it.
        assert_eq!(
            a,
            EntryAuthority::Buy {
                tier: SizeTier::Full,
                clip_lamports: 1_000_000_000
            }
        );
        assert_eq!(l.accepted(), 1);
    }

    #[test]
    fn cash_caps_the_clip_but_never_enlarges_it() {
        let mut l = DriftLedger::new();
        // SMALL = 0.25 SOL of a 1 SOL notional: the account having 5 SOL does not make it
        // bigger. Rust resolves the model's tier; it does not re-size it upward.
        let a = decide_entry(&Stub(BUY_SMALL), &req(5_000_000_000, 0), &mut l);
        assert_eq!(
            a,
            EntryAuthority::Buy {
                tier: SizeTier::Small,
                clip_lamports: 250_000_000
            }
        );
        // And the fee buffer is respected: 0.30 SOL free cannot fund FULL's 1.00 SOL.
        let a = decide_entry(&Stub(BUY_FULL), &req(300_000_000, 0), &mut l);
        assert_eq!(
            a,
            EntryAuthority::Buy {
                tier: SizeTier::Full,
                clip_lamports: 250_000_000
            }
        );
    }

    #[test]
    fn a_buy_without_a_size_is_no_trade_and_flags_the_drift_alarm() {
        let mut l = DriftLedger::new();
        let a = decide_entry(&Stub(BUY_NO_SIZE), &req(2_000_000_000, 0), &mut l);
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
        let a = decide_entry(&Stub(GARBAGE), &req(2_000_000_000, 0), &mut l);
        assert!(matches!(
            a,
            EntryAuthority::NoTrade(NoTradeReason::OffContract(_))
        ));

        let dead = Unreachable(InferenceClient::new("http://127.0.0.1:1", Duration::from_millis(50)));
        let a = decide_entry(&dead, &req(2_000_000_000, 0), &mut l);
        assert_eq!(a, EntryAuthority::NoTrade(NoTradeReason::ModelUnreachable));
    }

    #[test]
    fn the_bankroll_floor_vetoes_a_trade_that_would_strand_the_account() {
        let mut l = DriftLedger::new();
        // 1 SOL free, floor 0.9 SOL: FULL's 1 SOL clip leaves 0 free → refused.
        let a = decide_entry(&Stub(BUY_FULL), &req(1_000_000_000, 900_000_000), &mut l);
        assert_eq!(a, EntryAuthority::NoTrade(NoTradeReason::BreachesBankrollFloor));
        // Floor 0.2 SOL: the same trade leaves 0.25 SOL free (cash 1.25) → allowed.
        let a = decide_entry(&Stub(BUY_FULL), &req(1_250_000_000, 200_000_000), &mut l);
        assert_eq!(
            a,
            EntryAuthority::Buy {
                tier: SizeTier::Full,
                clip_lamports: 1_000_000_000
            }
        );
    }

    #[test]
    fn unwatchable_cash_is_refused_not_rounded_up() {
        let mut l = DriftLedger::new();
        // 0.05 SOL free is exactly the fee buffer: nothing is payable.
        let a = decide_entry(&Stub(BUY_SMALL), &req(50_000_000, 0), &mut l);
        assert!(matches!(
            a,
            EntryAuthority::NoTrade(NoTradeReason::UnpayableClip(_))
        ));
        // 0.06 SOL leaves 0.01 SOL payable == the venue minimum → a real (tiny) clip.
        let a = decide_entry(&Stub(BUY_SMALL), &req(60_000_000, 0), &mut l);
        assert_eq!(
            a,
            EntryAuthority::Buy {
                tier: SizeTier::Small,
                clip_lamports: 10_000_000
            }
        );
    }

    #[test]
    fn watch_and_skip_pass_through_without_capital() {
        let mut l = DriftLedger::new();
        assert_eq!(
            decide_entry(&Stub(WATCH), &req(2_000_000_000, 0), &mut l),
            EntryAuthority::Watch
        );
        assert_eq!(
            decide_entry(&Stub(SKIP), &req(2_000_000_000, 0), &mut l),
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
            decide_entry(&Stub(EXIT), &req(2_000_000_000, 0), &mut l),
            EntryAuthority::NoTrade(NoTradeReason::ManagementVerb)
        );
        // HOLD on an empty book is the seam's NoOp — a deliberate non-trade, not an error.
        assert_eq!(
            decide_entry(&Stub(HOLD), &req(2_000_000_000, 0), &mut l),
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
    /// `ADD` — scale in by the corpus fraction of current inventory.
    ScaleIn {
        /// Basis points of current inventory to add (5_000 = +50%).
        inventory_fraction_bps: u32,
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
        Route::AddToPosition => match management_fraction_bps(decision.action) {
            Some(bps) => {
                ledger.record_accepted();
                ManagementAuthority::ScaleIn {
                    inventory_fraction_bps: bps,
                }
            }
            // Unreachable while the seam is the single source of the magnitudes; refusing
            // beats inventing one if that ever changes.
            None => ManagementAuthority::NoAction(ManagementNoAction::OffContract(
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
                inventory_fraction_bps: 5_000
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
