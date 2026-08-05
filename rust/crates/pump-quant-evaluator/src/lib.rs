// SAFETY POLICY (added 2026-07-29): this crate is outcome evaluation over money quantities,
// and it contained zero `unsafe` when this was added. `forbid` makes that a
// property the compiler holds rather than one a reviewer has to re-verify —
// and unlike `deny` it cannot be locally overridden by an `#[allow]`.
// Constitution §24(b): an `unsafe` block requires a dossier-registered,
// property-tested safety argument. There is no such dossier entry for this
// crate, so there is no `unsafe` this attribute could legitimately block.
#![forbid(unsafe_code)]
pub mod evaluator_state;
pub mod evaluator_stats;
pub mod deflated_sharpe;
pub mod thompson_sampling;
pub mod eight_gate;
pub mod strategy_committee;
pub mod strategy_type_sprt;
pub mod edge_attribution;
pub mod strategy_registry;
pub mod defense_in_depth;

// Constitution spec-gap leaves added alongside the frozen evaluator's stats
// core. Each is a pure, deterministic verdict/guard primitive (constitution
// §44/§51 — the frozen evaluator verifies before results are accepted).
pub mod ablation;
pub mod authorization_ceiling;
pub mod baseline_destruction;
pub mod baseline_family;
pub mod champion_challenger;
pub mod convexity_enrich;
pub mod convexity_ledger;
pub mod edge_decomposition;
pub mod entry_zone;
pub mod evaluator_pin;
pub mod evidence_status;
pub mod exit_markout;
pub mod fdr;
pub mod holdout_ledger;
pub mod holdout_overlap;
pub mod metrics;
pub mod overfitting;
pub mod promotion_verdict;
pub mod reflection_cadence;
pub mod regression_gate;
pub mod sequential_retirement;
pub mod sizing_validator;
pub mod social_ledger;
pub mod tape;
pub mod walk_forward;
