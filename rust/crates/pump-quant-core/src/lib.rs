//! pump-quant-core — workspace crate. Modules below are filled in leaf-by-leaf by the build;
//! each module's functions are implemented against locked dossier property tests.
// HOT crate: the hotpath_lint gate bans async/await, tokio, floats outside boundary adapters,
// syscall clocks, sleeps, allocating macros, and panic paths in these modules.

pub mod fixedpoint;
pub mod lockfree;
pub mod reducer;
pub mod replay;
pub mod shred;
