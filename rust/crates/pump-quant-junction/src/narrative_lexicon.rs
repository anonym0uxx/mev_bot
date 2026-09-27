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
    nv_family_resolve_with_inference, DynFamilyEntry, DynNeedle, DynamicLexicon, LexiconSource,
    Provenance,
};
use pump_quant_narrative::entry_narrative::{
    nv_narrative_verdict, NarrativeInputs, NarrativeVerdict,
};
use pump_quant_narrative::narrative_family::{
    MatchMode, NarrativeFamily, FAMILY_LEXICON_V1, NAME_INFERENCE_V1,
};
// The serve-side bridge: live inputs -> the exact block the model is shown.
use pump_quant_proposal::TokenIdentity;

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

/// One bucket's span in the flat, `as_of`-ascending `entries` vector.
///
/// Buckets exist because of CAUSALITY: an entry is usable only when
/// `first_seen_ms <= as_of_ms <= t_ms`. A single artifact cut "as of now" can
/// therefore label only rows that come AFTER it — measured, that is 0 of 200,000
/// historical rows, which reads as a thin vocabulary rather than as the rule
/// working. Each bucket is a frozen view of what was knowable at its boundary.
#[derive(Debug, Clone, Copy)]
struct BucketSpan {
    /// The boundary this bucket was computed as of.
    as_of_ms: u64,
    /// The artifact version in force at that boundary.
    version: u32,
    /// Half-open span of this bucket's entries in the flat vector.
    start: usize,
    end: usize,
}

/// The loaded lexicon plus the per-alias observation state it needs.
pub struct NarrativeLexicon {
    version: u32,
    /// All buckets' entries, ascending by their bucket's `as_of_ms`.
    entries: Vec<OwnedEntry>,
    /// The bucket boundaries, ascending. Never empty for a successfully loaded file.
    buckets: Vec<BucketSpan>,
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
        // Parse ONE bucket's entries into the flat, as_of-ascending vector.
        fn parse_into(arr: &[json::JsonValue], out: &mut Vec<OwnedEntry>) {
            for e in arr {
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
                    out.push(OwnedEntry {
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
        }

        let mut entries: Vec<OwnedEntry> = Vec::new();
        let mut buckets: Vec<BucketSpan> = Vec::new();

        if let Some(bs) = root.get("buckets").and_then(|v| v.as_array()) {
            let mut spans: Vec<BucketSpan> = Vec::with_capacity(bs.len());
            for b in bs {
                let as_of_ms = b
                    .get("as_of_ms")
                    .and_then(|v| v.as_number_str())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let bver = b
                    .get("version")
                    .and_then(|v| v.as_number_str())
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(version);
                let start = entries.len();
                if let Some(arr) = b.get("entries").and_then(|v| v.as_array()) {
                    parse_into(arr, &mut entries);
                }
                spans.push(BucketSpan {
                    as_of_ms,
                    version: bver,
                    start,
                    end: entries.len(),
                });
            }
            // Ascending `as_of` is the contract: "the latest bucket at or before t"
            // means nothing over an unordered list, so order it here rather than
            // trusting the producer's emission order.
            spans.sort_by_key(|s| s.as_of_ms);
            buckets = spans;
        } else {
            // Legacy single-cut artifact: one bucket, stamped with the document's
            // own `as_of_ms`. It can only ever label rows at or after that instant.
            let raw_entries = root.get("entries")?.as_array()?;
            parse_into(raw_entries, &mut entries);
            let as_of_ms = root
                .get("as_of_ms")
                .and_then(|v| v.as_number_str())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            buckets.push(BucketSpan {
                as_of_ms,
                version,
                start: 0,
                end: entries.len(),
            });
        }
        Some(NarrativeLexicon {
            version,
            entries,
            buckets,
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

    /// How many time buckets the artifact carries. `1` for a legacy single-cut file.
    #[must_use]
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    /// Index of the latest bucket whose boundary is at or before `now_ms`.
    ///
    /// `None` when `now_ms` precedes every boundary: nothing was knowable yet, so
    /// the launch resolves `Unresolved` — the CAUSAL answer, not a failure. The
    /// scan is linear because there are a handful of buckets, and it exits early
    /// on the first boundary that is still in the future.
    fn bucket_for(&self, now_ms: u64) -> Option<usize> {
        let mut found = None;
        for (i, b) in self.buckets.iter().enumerate() {
            if b.as_of_ms <= now_ms {
                found = Some(i);
            } else {
                break;
            }
        }
        found
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
        // The bucket in force at `now_ms`. `None` means this instant precedes every
        // boundary we hold: nothing was knowable yet, so it resolves `Unresolved`
        // with the sentinel stage/family. Refusing to guess is the entire point of
        // bucketing — the alternative is labelling history with a lexicon that had
        // not been built at that instant.
        let Some(bi) = self.bucket_for(now_ms) else {
            return (
                verdict_code(NarrativeVerdict::Unresolved),
                0,
                0,
                self.version,
            );
        };
        let (bstart, bend) = (self.buckets[bi].start, self.buckets[bi].end);
        let bversion = self.buckets[bi].version;

        // The borrowed lexicon view lives only inside this block: `record` below
        // needs `&mut self`, so every borrow of `self.entries` must end first.
        let (out, hit) = {
            let slice = &self.entries[bstart..bend];
            let needles: Vec<Vec<DynNeedle<'_>>> = slice
                .iter()
                .map(|e| {
                    vec![DynNeedle {
                        text: e.alias.as_str(),
                        mode: e.mode,
                    }]
                })
                .collect();
            let entries: Vec<DynFamilyEntry<'_>> = slice
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
                version: bversion,
                entries: &entries,
            };
            let resolution = nv_family_resolve_with_inference(
                name,
                symbol,
                FAMILY_LEXICON_V1,
                Some(&dl),
                // Tried LAST, and only ever reported as `NameInference`. The operator's
                // instruction was to infer from the name: a name is creation metadata, so
                // this reading cannot look ahead and needs no time window.
                NAME_INFERENCE_V1,
                now_ms,
                false, // quarantine (model proposals) is measurement-only
            );

            // Which entry fired, if any — needed for the alias stage.
            //
            // Only an OBSERVED hit may be attributed to a bucket entry. An inference
            // hit is a reading of the name, not evidence that this entry fired; if it
            // were allowed through, a name whose text happens to equal an observed
            // alias (say `ansem`) would be recorded against that alias and the ring
            // buffer would stop being strictly causal — the precise failure the
            // causality guard exists to prevent.
            let hit: Option<usize> = match resolution.source {
                Some(LexiconSource::NameInference) | None => None,
                Some(_) => resolution.matched_text.and_then(|t| {
                    slice
                        .iter()
                        .position(|e| e.alias == t && Some(e.family) == Some(resolution.family))
                        // GLOBAL index: `observation`/`record` key on the flat vector.
                        .map(|i| bstart + i)
                }),
            };
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

    /// The model-facing identity for a launch — **the serve-side call.**
    ///
    /// THIS IS THE GAP, CLOSED. The engine carries the narrative codes as integers
    /// (§22 keeps strings off the outcome path) and the bundle has an `identity` slot
    /// that nothing ever filled, so the live model was being shown no name at all. A
    /// serve path holding a mint's creation metadata calls this once and attaches the
    /// result to `BundleInputs::identity`, which the shipped `BundlePolicy::live()`
    /// then emits.
    ///
    /// `query_evidence` stays `None` deliberately: no j7/social evidence is wired at
    /// this point, and a slot that renders `none` is honest where a placeholder string
    /// would be fabricated.
    pub fn resolve_identity(&mut self, name: &str, symbol: &str, now_ms: u64) -> TokenIdentity {
        let (verdict, stage, family, version) = self.resolve(name, symbol, now_ms);
        TokenIdentity::from_codes(name, symbol, verdict, stage, family, version, None)
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
    ///
    /// NB the family still resolves — from the NAME-INFERENCE table, which reads
    /// creation metadata and needs no time window. That is not the observed entry
    /// firing early, and the ring-buffer assertion below is what proves it.
    #[test]
    fn a_mint_predating_the_entry_is_not_counted_against_it() {
        let p = fixture(LIVE); // as_of_ms = 500
        let mut l = NarrativeLexicon::load(&p).unwrap();
        let (_, _, f, _) = l.resolve("ansem thing", "ANSEM", 100); // before as_of
        assert!(
            !l.alias_seen.contains_key("ansem"),
            "a pre-as_of mint must leave no trace in the ring buffer"
        );
        assert_eq!(
            f,
            NarrativeFamily::Celebrity.ordinal(),
            "the family comes from inference, not from the not-yet-usable entry"
        );
    }

    #[test]
    fn a_missing_artifact_is_none_not_a_silent_refusal() {
        assert!(NarrativeLexicon::load("/nonexistent/pq_lex.json").is_none());
    }

    // ---- TIME BUCKETS -------------------------------------------------------
    //
    // These pin the reason bucketing exists: a single artifact cut "as of now"
    // cannot label history, because causality requires `as_of_ms <= t_ms`.

    /// Two boundaries: as of 1000 (ansem only) and as of 5000 (ansem + spcx).
    const BUCKETS: &str = r#"{"version":7,"schema_version":2,"as_of_ms":9000,"bucket_ms":4000,
      "buckets":[
        {"as_of_ms":1000,"version":7,"entries":[
          {"family":"celebrity","needles":[{"text":"ansem","mode":"substring"}],
           "first_seen_ms":500,"as_of_ms":1000,"ttl_ms":1000000000}]},
        {"as_of_ms":5000,"version":7,"entries":[
          {"family":"celebrity","needles":[{"text":"ansem","mode":"substring"}],
           "first_seen_ms":500,"as_of_ms":1000,"ttl_ms":1000000000},
          {"family":"tech","needles":[{"text":"spcx","mode":"substring"}],
           "first_seen_ms":4000,"as_of_ms":5000,"ttl_ms":1000000000}]}]}"#;

    #[test]
    fn a_row_before_every_boundary_is_unresolved() {
        let p = fixture(BUCKETS);
        let mut l = NarrativeLexicon::load(&p).unwrap();
        assert_eq!(l.bucket_count(), 2);
        let (v, s, f, _) = l.resolve("ansem thing", "ANSEM", 500);
        assert_eq!(
            (v, s, f),
            (5, 0, 0),
            "nothing was knowable before the first boundary - and that is the answer"
        );
    }

    #[test]
    fn a_row_resolves_against_the_bucket_in_force_then() {
        let p = fixture(BUCKETS);
        let mut l = NarrativeLexicon::load(&p).unwrap();
        let (v, _, f, lv) = l.resolve("ansem thing", "ANSEM", 1500);
        assert_eq!((v, f), (1, NarrativeFamily::Celebrity.ordinal()));
        assert_eq!(lv, 7);
    }

    #[test]
    fn a_bucket_cannot_see_its_successors_entries() {
        // `spcx` exists ONLY in the bucket as of 5000. At t=1500 it must not match:
        // matching would label a launch with knowledge from the future, which is the
        // look-ahead this whole structure exists to prevent.
        let p = fixture(BUCKETS);
        let mut l = NarrativeLexicon::load(&p).unwrap();
        let (v, _, f, _) = l.resolve("SPCX", "SPCX", 1500);
        assert_eq!((v, f), (5, 0), "a later bucket must not leak backwards");
        // ...and the same name DOES resolve once its bucket is in force.
        let (v2, _, f2, _) = l.resolve("SPCX", "SPCX", 6000);
        assert_eq!((v2, f2), (1, NarrativeFamily::Tech.ordinal()));
    }

    #[test]
    fn a_legacy_single_cut_loads_as_one_bucket() {
        let p = fixture(LIVE);
        let l = NarrativeLexicon::load(&p).unwrap();
        assert_eq!(
            l.bucket_count(),
            1,
            "no `buckets` key -> one bucket stamped from the document's as_of_ms"
        );
    }

    // ---- SERVE PATH, END TO END ---------------------------------------------
    //
    // Live inputs (creation metadata + clock) must produce the model-facing block.
    // Before `resolve_identity` existed, nothing filled `BundleInputs::identity` and
    // the live prompt carried no name at all.

    #[test]
    fn the_serve_path_renders_the_token_name_into_the_model_prompt() {
        let p = fixture(LIVE);
        let mut l = NarrativeLexicon::load(&p).unwrap();
        let id = l.resolve_identity("ansem thing", "ANSEM", 2000);
        assert_eq!(id.name, "ansem thing");
        assert_eq!(id.symbol, "ANSEM");
        let rendered = pump_quant_proposal::render_identity(&Some(id));
        assert!(rendered.contains("token_name=ansem thing"), "{rendered}");
        assert!(rendered.contains("token_symbol=ANSEM"), "{rendered}");
        assert!(
            rendered.contains("narrative_family=celebrity"),
            "{rendered}"
        );
        assert!(
            rendered.contains("narrative_verdict=Eligible"),
            "{rendered}"
        );
    }

    /// The live case that matters most: a brand-new meta with NO resolved narrative.
    /// The name must still reach the model, with the unresolved state stated plainly
    /// rather than the block being suppressed.
    #[test]
    fn the_serve_path_still_carries_an_unresolved_name() {
        let p = fixture(LIVE);
        let mut l = NarrativeLexicon::load(&p).unwrap();
        let id = l.resolve_identity("totally novel thing", "NOVEL", 2000);
        let rendered = pump_quant_proposal::render_identity(&Some(id));
        assert!(
            rendered.contains("token_name=totally novel thing"),
            "{rendered}"
        );
        assert!(
            rendered.contains("narrative_verdict=Unresolved"),
            "{rendered}"
        );
        assert!(rendered.contains("query_evidence=none"), "{rendered}");
    }
}
