pub mod evaluator_stats;

// Constitution spec-gap leaves added alongside the frozen evaluator's stats
// core. Each is a pure, deterministic verdict/guard primitive (constitution
// §44/§51 — the frozen evaluator verifies before results are accepted).
pub mod authorization_ceiling;
pub mod baseline_destruction;
pub mod champion_challenger;
pub mod evaluator_pin;
pub mod evidence_status;
pub mod holdout_ledger;
pub mod holdout_overlap;
pub mod regression_gate;
pub mod sequential_retirement;
pub mod social_ledger;
pub mod walk_forward;
