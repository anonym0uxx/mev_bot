//! Rotating narrative lexicon: load the versioned artifact and resolve a
//! launch's NAME to a narrative verdict (operator ruling 2026-09-26).
//!
//! Why this lives in the junction and not in `pump-quant-narrative`: that crate
//! is deliberately dependency-free (empty `[dependencies]`) and holds no growing
//! state — every function is a pure fold over caller-owned slices. Loading JSON
//! and keeping the per-alias ring buffers are both I/O-and-state concerns, so
//! they belong at the adapter edge. The DECISIONS stay in the crate:
//! [`nv_family_resolve`] (which table fired, causality, freshness),
//! [`nv_alias_stage`] (novelty vs saturation) and [`nv_narrative_verdict`] (the
//! precondition itself).
//!
//! Causality: the lexicon artifact carries `first_seen_ms <= as_of_ms` per entry
//! and the crate refuses any entry that is not usable at `now_ms`, so no
//! look-ahead is representable here either. The ring buffers only ever count
//! OTHER mints seen strictly before the current one.

use std::collections::HashMap;

use pump_quant_ingest::json;
use pump_quant_narrative::alias_stage::{
    nv_alias_stage, AliasObservation, AliasStage, STAGE_THRESHOLDS_V1,
};
use pump_quant_narrative::dynamic_lexicon::{
    nv_family_resolve, DynFamilyEntry, DynNeedle, DynamicLexicon, Provenance,
};
use pump_quant_narrative::entry_narrative::{
    nv_narrative_verdict, NarrativeInputs, NarrativeVerdict,
};
use pump_quant_narrative::narrative_family::{MatchMode, NarrativeFamily, FAMILY_LEXICON_V1};

/// Window the alias ring keeps for crowding counts (24h) and for the weekly
/// baseline that acceleration is measured against (7d).
const WINDOW_24H_MS: u64 = 24 * 60 * 60 * 1_000;
const WINDOW_7D_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const WINDOW_1H_MS: u64 = 60 * 60 * 1_000;

/// One lexicon entry, owned (the artifact is parsed once at startup).
struct OwnedEntry {
    family: NarrativeFamily,
    alias: String,
    mode: MatchMode,
    first_seen_ms: u64,
    as_of_ms: u64,
    ttl_ms: u64,
    provenance: Provenance,
    confidence_bps: u16,
}

/// The loaded lexicon plus the per-alias observation state it needs.
pub struct NarrativeLexicon {
    version: u32,
    entries: Vec<OwnedEntry>,
    /// alias -> mint timestamps observed (kept to the 7d window).
    alias_seen: HashMap<String, Vec<u64>>,
    /// family ordinal -> (ms, entry index) observed, kept to the 1h window. Used
    /// to count DISTINCT aliases of one family, which is the misspelling-variant
    /// proxy: copycats misspell to dodge filters, so a variant wave is late-stage.
    family_seen: HashMap<u8, Vec<(u64, usize)>>,
}

/// Map the producer's family token to the enum. An unknown token yields `None`
/// and the entry is DROPPED — a family we cannot name is not guessed (§6.4).
fn family_from_str(s: &str) -> Option<NarrativeFamily> {
    match s {
        "animal" => Some(NarrativeFamily::Animal),
        "political" => Some(NarrativeFamily::Political),
        "celebrity" => Some(NarrativeFamily::Celebrity),
        "tech" => Some(NarrativeFamily::Tech),
        "derivative" => Some(NarrativeFamily::Derivative),
        "stream" => Some(NarrativeFamily::Stream),
        "seasonal" => Some(NarrativeFamily::Seasonal),
        _ => None,
    }
}

fn mode_from_str(s: &str) -> Option<MatchMode> {
    match s {
        "substring" => Some(MatchMode::Substring),
        "word" => Some(MatchMode::Word),
        _ => None,
    }
}

/// The event's verdict discriminant. Explicit rather than `as u8` so the wire
/// encoding cannot drift if the enum's declaration order ever changes.
#[must_use]
pub const fn verdict_code(v: NarrativeVerdict) -> u8 {
    match v {
        NarrativeVerdict::Eligible => 1,
        NarrativeVerdict::Saturated => 2,
        NarrativeVerdict::NoAttach => 3,
        NarrativeVerdict::Throwaway => 4,
        NarrativeVerdict::Unresolved => 5,
    }
}

/// The event's stage discriminant. 0 = not observed.
#[must_use]
pub const fn stage_code(s: Option<AliasStage>) -> u8 {
    match s {
        None => 0,
        Some(AliasStage::Novel) => 1,
        Some(AliasStage::Rising) => 2,
        Some(AliasStage::Cresting) => 3,
        Some(AliasStage::Saturated) => 4,
    }
}

impl NarrativeLexicon {
    /// Load the artifact. `None` when the file is absent or malformed — the
    /// caller then emits no narrative verdicts at all, which is a COVERAGE GAP
    /// (the gate sees the sentinel and admits), never a silent refusal.
    #[must_use]
    pub fn load(path: &str) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let root = json::parse(&bytes)?;
        let version = root
            .get("version")
            .and_then(|v| v.as_number_str())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let raw_entries = root.get("entries")?.as_array()?;
        let mut entries = Vec::with_capacity(raw_entries.len());
        for e in raw_entries {
            let Some(family) = e
                .get("family")
                .and_then(|v| v.as_str())
                .and_then(family_from_str)
            else {
                continue; // unnameable family: drop, never guess
            };
            let Some(needles) = e.get("needles").and_then(|v| v.as_array()) else {
                continue;
            };
            for n in needles {
                let Some(alias) = n.get("text").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(mode) = n
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .and_then(mode_from_str)
                else {
                    continue;
                };
                let num = |k: &str| -> u64 {
                    e.get(k)
                        .and_then(|v| v.as_number_str())
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0)
                };
                entries.push(OwnedEntry {
                    family,
                    alias: alias.to_string(),
                    mode,
                    first_seen_ms: num("first_seen_ms"),
                    as_of_ms: num("as_of_ms"),
                    ttl_ms: num("ttl_ms"),
                    provenance: Provenance::Pipeline,
                    confidence_bps: e
                        .get("confidence_bps")
                        .and_then(|v| v.as_number_str())
                        .and_then(|s| s.parse::<u16>().ok())
                        .unwrap_or(0),
                });
            }
        }
        Some(NarrativeLexicon {
            version,
            entries,
            alias_seen: HashMap::new(),
            family_seen: HashMap::new(),
        })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Artifact version, for the startup banner and the journal.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Counts of OTHER mints sharing `idx`'s alias, strictly before `now_ms`.
    fn observation(&self, idx: usize, now_ms: u64) -> AliasObservation {
        let alias = &self.entries[idx].alias;
        let mut obs = AliasObservation::default();
        if let Some(ts) = self.alias_seen.get(alias) {
            obs.alias_first_seen_ms = ts.first().copied();
            for &t in ts {
                let age = now_ms.saturating_sub(t);
                if age <= WINDOW_24H_MS {
                    obs.prior_count_24h = obs.prior_count_24h.saturating_add(1);
                    if age <= WINDOW_1H_MS {
                        obs.prior_count_1h = obs.prior_count_1h.saturating_add(1);
                    }
                    if age <= 5 * 60 * 1_000 {
                        obs.prior_count_5m = obs.prior_count_5m.saturating_add(1);
                    }
                    if age <= 60 * 1_000 {
                        obs.prior_count_60s = obs.prior_count_60s.saturating_add(1);
                    }
                }
            }
            // Baseline daily rate over the 7d window EXCLUDING the last 24h.
            let older = ts
                .iter()
                .filter(|&&t| now_ms.saturating_sub(t) > WINDOW_24H_MS)
                .count() as u32;
            let baseline_daily = older / 6;
            if baseline_daily > 0 {
                obs.accel_x100 = obs
                    .prior_count_24h
                    .saturating_mul(100)
                    .saturating_div(baseline_daily);
            } else if obs.prior_count_24h > 0 {
                // No prior-week baseline: any appearance is an acceleration.
                obs.accel_x100 = obs.prior_count_24h.saturating_mul(100);
            }
        }
        // Distinct OTHER aliases of the same family in the trailing hour.
        if let Some(v) = self.family_seen.get(&self.entries[idx].family.ordinal()) {
            let mut seen: Vec<usize> = v
                .iter()
                .filter(|(t, j)| now_ms.saturating_sub(*t) <= WINDOW_1H_MS && *j != idx)
                .map(|(_, j)| *j)
                .collect();
            seen.sort_unstable();
            seen.dedup();
            obs.variant_count_1h = seen.len().min(u32::MAX as usize) as u32;
        }
        obs
    }

    /// Record this mint against its alias and family (after the decision, so the
    /// observation is strictly causal).
    fn record(&mut self, idx: usize, now_ms: u64) {
        let alias = self.entries[idx].alias.clone();
        let v = self.alias_seen.entry(alias).or_default();
        v.push(now_ms);
        v.retain(|&t| now_ms.saturating_sub(t) <= WINDOW_7D_MS);
        let fam = self.entries[idx].family.ordinal();
        let fv = self.family_seen.entry(fam).or_default();
        fv.push((now_ms, idx));
        fv.retain(|(t, _)| now_ms.saturating_sub(*t) <= WINDOW_1H_MS);
    }

    /// Resolve a launch. Returns `(verdict, stage, family, lexicon_version)` as
    /// the event's discriminants. Always returns a verdict: an unmatched name
    /// resolves `Unresolved`, which is a refusal under ENFORCE and a recorded
    /// fact under OBSERVE.
    pub fn resolve(&mut self, name: &str, symbol: &str, now_ms: u64) -> (u8, u8, u8, u32) {
        // The borrowed lexicon view lives only inside this block: `record` below
        // needs `&mut self`, so every borrow of `self.entries` must end first.
        let (out, hit) = {
            let needles: Vec<Vec<DynNeedle<'_>>> = self
                .entries
                .iter()
                .map(|e| {
                    vec![DynNeedle {
                        text: e.alias.as_str(),
                        mode: e.mode,
                    }]
                })
                .collect();
            let entries: Vec<DynFamilyEntry<'_>> = self
                .entries
                .iter()
                .zip(needles.iter())
                .map(|(e, ns)| DynFamilyEntry {
                    family: e.family,
                    needles: ns.as_slice(),
                    first_seen_ms: e.first_seen_ms,
                    as_of_ms: e.as_of_ms,
                    ttl_ms: e.ttl_ms,
                    provenance: e.provenance,
                    confidence_bps: e.confidence_bps,
                })
                .collect();
            let dl = DynamicLexicon {
                version: self.version,
                entries: &entries,
            };
            let resolution = nv_family_resolve(
                name,
                symbol,
                FAMILY_LEXICON_V1,
                Some(&dl),
                now_ms,
                false, // quarantine (model proposals) is measurement-only
            );

            // Which entry fired, if any — needed for the alias stage.
            let hit: Option<usize> = resolution.matched_text.and_then(|t| {
                self.entries
                    .iter()
                    .position(|e| e.alias == t && Some(e.family) == Some(resolution.family))
            });
            let stage = hit.map(|idx| {
                nv_alias_stage(now_ms, &self.observation(idx, now_ms), &STAGE_THRESHOLDS_V1)
            });

            let decision = nv_narrative_verdict(&NarrativeInputs {
                name,
                symbol,
                resolution: &resolution,
                stage,
                // The attention lane and the model lane are not available at launch
                // time; absence stays absent (never fabricated as `false`).
                attention_emerging: None,
                model_no_attach: None,
            });

            (
                (
                    verdict_code(decision.verdict),
                    stage_code(decision.stage),
                    decision.family.ordinal(),
                    resolution.version,
                ),
                hit,
            )
        };

        if let Some(idx) = hit {
            self.record(idx, now_ms);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(body: &str) -> String {
        let p = std::env::temp_dir().join(format!(
            "pq_lex_test_{}_{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&p, body).expect("write fixture");
        p.to_string_lossy().to_string()
    }

    const LIVE: &str = r#"{"schema_version":1,"version":3,"entries":[
      {"family":"celebrity","needles":[{"text":"ansem","mode":"substring"}],
       "first_seen_ms":100,"as_of_ms":500,"ttl_ms":100000000,
       "provenance":"pipeline","confidence_bps":8000}
    ]}"#;

    #[test]
    fn loads_the_artifact_and_resolves_a_live_alias() {
        let p = fixture(LIVE);
        let mut l = NarrativeLexicon::load(&p).expect("loads");
        assert_eq!(l.version(), 3);
        assert_eq!(l.len(), 1);
        // Substring mode: matches a name that CONTAINS the alias, case-insensitively.
        let (v, s, f, lv) = l.resolve("SAYING ANSEM 1 MILL TIMES", "ANSEM", 2000);
        assert_eq!(
            (v, s, f, lv),
            (1, 1, NarrativeFamily::Celebrity.ordinal(), 3)
        );
    }

    #[test]
    fn an_unmatched_name_is_unresolved_never_a_guess() {
        let p = fixture(LIVE);
        let mut l = NarrativeLexicon::load(&p).unwrap();
        let (v, s, f, _) = l.resolve("totally unrelated thing", "XYZ", 2000);
        assert_eq!(v, 5, "unmatched must be Unresolved");
        assert_eq!(s, 0, "no stage was observed");
        assert_eq!(f, 0, "Unclassified, never a guess");
    }

    #[test]
    fn throwaway_is_refused_before_the_lexicon() {
        let p = fixture(LIVE);
        let mut l = NarrativeLexicon::load(&p).unwrap();
        let (v, _, _, _) = l.resolve("Poop Coin", "POOP", 2000);
        assert_eq!(v, 4, "throwaway");
    }

    #[test]
    fn causality_an_entry_not_yet_usable_cannot_fire() {
        let body = r#"{"version":1,"entries":[{"family":"tech",
          "needles":[{"text":"spcx","mode":"substring"}],
          "first_seen_ms":1000,"as_of_ms":9999,"ttl_ms":100000000}]}"#;
        let p = fixture(body);
        let mut l = NarrativeLexicon::load(&p).unwrap();
        let (v, _, f, _) = l.resolve("SPCX", "SPCX", 2000);
        assert_eq!(f, 0, "a not-yet-usable entry must not resolve a family");
        assert_eq!(v, 5);
    }

    #[test]
    fn an_expired_entry_cannot_fire() {
        let body = r#"{"version":1,"entries":[{"family":"tech",
          "needles":[{"text":"spcx","mode":"substring"}],
          "first_seen_ms":1000,"as_of_ms":2000,"ttl_ms":100}]}"#;
        let p = fixture(body);
        let mut l = NarrativeLexicon::load(&p).unwrap();
        let (v, _, f, _) = l.resolve("SPCX", "SPCX", 900000);
        assert_eq!(f, 0, "an expired entry is not evidence");
        assert_eq!(v, 5);
    }

    #[test]
    fn crowding_drives_saturation() {
        let p = fixture(LIVE);
        let mut l = NarrativeLexicon::load(&p).unwrap();
        // Other mints sharing the alias inside 24h = the measured saturated cohort.
        // NB the fixture's as_of_ms is 500: an earlier `now_ms` would be refused by
        // the causality guard and never recorded (that is asserted separately).
        for i in 0..5u64 {
            let _ = l.resolve("ansem thing", "ANSEM", 1000 + i * 1000);
        }
        let (v, s, _, _) = l.resolve("ansem thing again", "ANSEM", 10_000);
        assert_eq!(s, 4, "saturated stage");
        assert_eq!(v, 2, "saturated verdict -> refused under ENFORCE");
    }

    /// The causality guard, end to end through the loader: a mint seen BEFORE an
    /// entry became usable must not be recorded against it.
    #[test]
    fn a_mint_predating_the_entry_is_not_counted_against_it() {
        let p = fixture(LIVE); // as_of_ms = 500
        let mut l = NarrativeLexicon::load(&p).unwrap();
        let (v, _, f, _) = l.resolve("ansem thing", "ANSEM", 100); // before as_of
        assert_eq!((v, f), (5, 0), "not usable yet -> unresolved, no family");
        assert!(
            !l.alias_seen.contains_key("ansem"),
            "a pre-as_of mint must leave no trace in the ring buffer"
        );
    }

    #[test]
    fn a_missing_artifact_is_none_not_a_silent_refusal() {
        assert!(NarrativeLexicon::load("/nonexistent/pq_lex.json").is_none());
    }
}
