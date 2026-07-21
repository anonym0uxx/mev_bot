//! hot-core — Reducer, fixedpoint, scalp position state, lock-free primitives. HOT: the hotpath_lint gate bans async, tokio, floats (outside *boundary* adapters), syscall clocks, sleeps, allocating macros, and panic paths here.
//! Constitution: §22 §24 §57. Built under docs/HERMES_ONE_SHOT_PROMPT.md (criteria 1–110).
//! Dossier-governed modules use run_reinforcement; every unsafe block carries a
//! dossier-registered safety argument (criterion 109).
