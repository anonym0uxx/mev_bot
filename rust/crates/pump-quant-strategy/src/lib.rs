//! pump-quant-strategy — workspace crate. Modules below are filled in leaf-by-leaf by the build;
//! each module's functions are implemented against locked dossier property tests.
// HOT crate: the hotpath_lint gate bans async/await, tokio, floats outside boundary adapters,
// syscall clocks, sleeps, allocating macros, and panic paths in these modules.

pub mod scalp_position;
pub mod exit_ladder;
pub mod economic_gate;
pub mod safety_integrity;
