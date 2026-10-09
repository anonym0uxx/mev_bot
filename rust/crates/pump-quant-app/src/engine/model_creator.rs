//! M2 — engine wiring of the persistent causal observed-launch creator registry
//! (see [`crate::creator_registry`]).
//!
//! Startup order (before the first tick, i.e. before entry inference resumes):
//! 1. the pre-cutoff SEED rows are inserted in receive order (stable by file position), each count
//!    computed before its own insert (count-before-insert lives in `CreatorHistory::observe`);
//! 2. the append-only log of our own `LaunchObserved` is replayed in file order — exactly the order
//!    a no-restart run inserted them — and each logged launch is again marked feed-observed.
//!
//! At runtime a `LaunchObserved` for a mint NOT already held is appended (fsync) BEFORE it is
//! counted; a mint already held (seed or log; replay overlap) changes nothing and appends nothing.
//! Any failure is a named entry refusal, never a silently empty or partial registry.

use super::Engine;
use crate::creator_registry::{
    load_seed, LaunchLog, LaunchRow, Provenance, RegistryRefusal, RegistryStartup, SeedSpec,
};

impl Engine {
    /// Attach the persistent creator registry. Call on a FRESH engine before the first tick.
    /// `cutoff_ms` is the replay/run boundary: only seed rows strictly before it are loaded.
    pub fn model_creator_registry_attach(
        &mut self,
        seed: Option<&SeedSpec>,
        log_path: &std::path::Path,
        cutoff_ms: i64,
        session: &str,
    ) -> Result<RegistryStartup, RegistryRefusal> {
        let r = self.creator_registry_attach_inner(seed, log_path, cutoff_ms, session);
        match &r {
            Ok(st) => {
                self.model_cache
                    .set_creator_provenance(st.provenance.record_tag());
                self.mrep_add("creator_registry:seeded", st.seeded);
                self.mrep_add("creator_registry:restored_from_log", st.restored_from_log);
                self.mrep_add("creator_registry:dedup_overlap", st.dedup_overlap);
            }
            Err(e) => {
                self.model_cache.set_creator_registry_refusal(e.as_str());
                self.mrep(format!("creator_registry:refused:{}", e.as_str()));
            }
        }
        r
    }

    fn creator_registry_attach_inner(
        &mut self,
        seed: Option<&SeedSpec>,
        log_path: &std::path::Path,
        cutoff_ms: i64,
        session: &str,
    ) -> Result<RegistryStartup, RegistryRefusal> {
        let mut st = RegistryStartup {
            provenance: Provenance::unseeded(cutoff_ms),
            seeded: 0,
            seed_rows_at_or_after_cutoff: 0,
            restored_from_log: 0,
            dedup_overlap: 0,
        };
        if let Some(spec) = seed {
            if spec.cutoff_ms != cutoff_ms {
                return Err(RegistryRefusal::LogSeedMismatch);
            }
            let mut s = load_seed(spec)?;
            st.provenance = Provenance {
                seed_sha256: s.sha256.clone(),
                seed_label: s.label.clone(),
                seed_cutoff_ms: s.cutoff_ms,
            };
            st.seed_rows_at_or_after_cutoff = s.rows_at_or_after_cutoff;
            // Receive order (stable on file position): each count is frozen at its own insert.
            s.rows.sort_by_key(|r| r.recv_unix_ms);
            for r in &s.rows {
                if self.model_cache.creator_launch_held(&r.mint) {
                    st.dedup_overlap += 1;
                    continue;
                }
                if !self
                    .model_cache
                    .seed_creator_launch(r.mint, r.creator, r.recv_unix_ms)
                {
                    return Err(RegistryRefusal::SeedRowMalformed { line: 0 });
                }
                st.seeded += 1;
            }
        }
        let (log, records) = LaunchLog::open(log_path, &st.provenance)?;
        for (i, r) in records.iter().enumerate() {
            if self.model_cache.creator_launch_held(&r.mint) {
                // A logged mint was only appended when it was NOT held, so a held mint here is
                // either an exact replay (harmless) or a contradiction (refuse by name).
                let same = self.model_cache.creator_dev_history(&r.mint).creator_known == 1
                    && self
                        .model_cache
                        .observe_launch(r.mint, r.creator, r.recv_unix_ms);
                if !same {
                    return Err(RegistryRefusal::LogConflict { line: i as u64 + 2 });
                }
                st.dedup_overlap += 1;
                continue;
            }
            if !self
                .model_cache
                .observe_launch(r.mint, r.creator, r.recv_unix_ms)
            {
                return Err(RegistryRefusal::LogConflict { line: i as u64 + 2 });
            }
            st.restored_from_log += 1;
        }
        self.creator_log = Some(log);
        self.creator_log_session = session.to_string();
        Ok(st)
    }

    /// One `LaunchObserved` from our feed. Durable append BEFORE the count changes, so a crash can
    /// lose at most an un-counted launch, never a counted one.
    pub(super) fn model_observe_launch(
        &mut self,
        mint: [u8; 32],
        creator: [u8; 32],
        launch_unix_ms: i64,
    ) {
        // Append when OUR FEED has not yet observed this mint (a seed-only mint observed live is
        // logged once, so a restart still knows the feed saw it); a re-delivery appends nothing.
        let fresh = !self.model_cache.launch_known(&mint);
        if fresh {
            if let Some(log) = self.creator_log.as_mut() {
                let row = LaunchRow {
                    mint,
                    creator,
                    recv_unix_ms: launch_unix_ms,
                };
                if log.append(&row, &self.creator_log_session).is_err() {
                    self.model_cache
                        .set_creator_registry_refusal(RegistryRefusal::LogAppendFailed.as_str());
                    self.mrep("creator_registry:append_failed");
                } else {
                    self.mrep("creator_registry:appended");
                }
            }
        } else if self.creator_log.is_some() {
            self.mrep("creator_registry:launch_already_held");
        }
        self.model_cache
            .observe_launch(mint, creator, launch_unix_ms);
    }

    /// M2 read-only view: the registry's `DEV HISTORY` inputs for `mint`.
    #[must_use]
    pub fn model_creator_dev_history(
        &self,
        mint: &[u8; 32],
    ) -> pump_quant_proposal::decision::DevHistoryDecision {
        self.model_cache.creator_dev_history(mint)
    }

    /// M2 read-only view: launches held by the registry.
    #[must_use]
    pub fn model_creator_registry_len(&self) -> usize {
        self.model_cache.creator_registry_len()
    }

    /// M2 read-only view: whether OUR FEED observed the launch (the cohort-freeze predicate).
    #[must_use]
    pub fn model_launch_feed_observed(&self, mint: &[u8; 32]) -> bool {
        self.model_cache.launch_known(mint)
    }

    /// M2 read-only view: the registry's named refusal, if any.
    #[must_use]
    pub fn model_creator_registry_refusal(&self) -> Option<&'static str> {
        self.model_cache.creator_registry_refusal()
    }

    /// M2: snapshot for `mint` at `t_dec_ms` (entry), for offline prompt/provenance checks.
    pub fn model_entry_snapshot(
        &self,
        mint: &[u8; 32],
        t_dec_ms: i64,
    ) -> Result<crate::decision_join::PromptSnapshot, crate::decision_join::JoinRefusal> {
        self.model_cache.snapshot(mint, t_dec_ms)
    }
}
