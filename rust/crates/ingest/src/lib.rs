//! ingest — Observation sources (LaserStream, shred-class), canonical decode, hot journal writer. Control plane: tokio permitted; hands events to hot-core via SPSC only.
//! Constitution: §18 §22 §43. Built under docs/HERMES_ONE_SHOT_PROMPT.md (criteria 1–110).
//! Dossier-governed modules use run_reinforcement; every unsafe block carries a
//! dossier-registered safety argument (criterion 109).
