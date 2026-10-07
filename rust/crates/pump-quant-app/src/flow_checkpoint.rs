//! Durable checkpoint of the trained flow-history state (wallet extraction totals, distinct-mint samples,
//! first/last-seen, the unpruned co-entry graph, per-mint windows) with provenance, a per-source resume cursor,
//! and OBSERVED-coverage accounting that is kept separate from elapsed age.
//!
//! Contract
//! * One file, written atomically (temp, fsync, rename, fsync dir). The body is `header JSON line` + `\n` + payload;
//!   the header carries schema, reducer-params fingerprint, payload length and sha256, provenance, cursors, coverage.
//! * LOAD never returns a partially-trusted state: wrong magic/schema/params, bad length/hash, or a payload the
//!   reducer refuses => `Untrusted(reason)`. A missing file is `NeverWritten`.
//! * A checkpoint that loads is NOT a claim that the feed was continuous: `restore` compares the cursor to the
//!   resume time and records the unobserved interval as a named gap (`unavailable_ms`); `complete` is false then.
//! * Elapsed age (`flow_lookback_d` basis) and observed coverage are different numbers and reported separately.
//! * Nothing here prunes the co-entry graph or lifetime wallet state; `resources()` reports their size.
//!
//! Source cursors. An event is ordered only within its own source, so each source has its own cursor:
//! `(high_water_recv_ms, ids seen AT that millisecond)`. Replay rule per source: `recv < high_water` => already
//! applied or late (dropped, counted `late_or_replayed`, never applied: a late event never rewrites earlier decisions);
//! `recv == high_water` => applied only if its id is not in the boundary set; `recv > high_water` => applied.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;

use pump_quant_market_state::flow_reducer::{FlowEvent, FlowParams, FlowReducer, STATE_SCHEMA};
use pump_quant_protocol::sha256::sha256;
use serde_json::{json, Value};

pub const MAGIC: &str = "pq-flow-checkpoint";
pub const FILE_SCHEMA: u64 = 1;
/// Two events further apart than this are NOT bridged into one observed segment.
pub const MAX_BRIDGE_MS: i64 = 60_000;

/// Per-source resume cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub high_water_ms: i64,
    pub boundary_ids: BTreeSet<u128>,
}

/// Where the history came from (provenance), carried verbatim.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Provenance {
    /// Free-form but stable label of the seed source, e.g. `frozen_tape:renormalized_v7`.
    pub seed_source: String,
    /// sha256 (hex) of the seed artifact when known.
    pub seed_sha256: String,
    /// The seed contains events with recv strictly below this (unix ms).
    pub seed_before_ms: i64,
    /// Producer git SHA / build id that wrote the in-session rows.
    pub producer: String,
}

/// Observed-coverage segments: intervals during which events were actually being received.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    pub segments: Vec<(i64, i64)>,
    /// Intervals named unavailable (feed down / checkpoint restored across a hole).
    pub gaps: Vec<(i64, i64)>,
}

impl Coverage {
    pub fn observe(&mut self, recv_ms: i64) {
        match self.segments.last_mut() {
            Some(s) if recv_ms >= s.1 && recv_ms - s.1 <= MAX_BRIDGE_MS => s.1 = recv_ms,
            Some(s) if recv_ms < s.1 => {} // late: coverage never moves backwards
            _ => self.segments.push((recv_ms, recv_ms)),
        }
    }
    #[must_use]
    pub fn observed_ms(&self) -> i64 {
        self.segments.iter().map(|s| s.1 - s.0).sum()
    }
}

/// Counters for one ingest pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IngestStats {
    pub applied: u64,
    pub duplicate: u64,
    pub late_or_replayed: u64,
}

/// The live flow-history state plus its provenance and cursors.
pub struct FlowHistory {
    pub reducer: FlowReducer,
    pub provenance: Provenance,
    pub cursors: BTreeMap<String, Cursor>,
    pub coverage: Coverage,
}

impl FlowHistory {
    #[must_use]
    pub fn new(params: FlowParams, provenance: Provenance) -> Self {
        Self {
            reducer: FlowReducer::with_params(params),
            provenance,
            cursors: BTreeMap::new(),
            coverage: Coverage::default(),
        }
    }

    /// Apply one event from `source` under the per-source resume rule. `event_id` is the stable identity.
    pub fn ingest(
        &mut self,
        source: &str,
        event_id: u128,
        e: &FlowEvent,
        st: &mut IngestStats,
    ) -> bool {
        let c = self.cursors.entry(source.to_string()).or_default();
        let t = e.recv_unix_ms;
        if t < c.high_water_ms {
            st.late_or_replayed += 1;
            return false;
        }
        if t == c.high_water_ms {
            if !c.boundary_ids.insert(event_id) {
                st.duplicate += 1;
                return false;
            }
        } else {
            c.high_water_ms = t;
            c.boundary_ids.clear();
            c.boundary_ids.insert(event_id);
        }
        self.reducer.on_event(e);
        self.coverage.observe(t);
        st.applied += 1;
        true
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let payload = self.reducer.encode_state();
        let digest = sha256(&payload);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let cursors: Vec<Value> = self
            .cursors
            .iter()
            .map(|(k, c)| {
                json!({"source": k, "high_water_ms": c.high_water_ms,
                       "boundary_ids": c.boundary_ids.iter().map(|i| format!("{i:032x}")).collect::<Vec<_>>()})
            })
            .collect();
        let hdr = json!({
            "magic": MAGIC, "file_schema": FILE_SCHEMA, "state_schema": STATE_SCHEMA,
            "params": self.reducer.params_fingerprint().iter().map(ToString::to_string).collect::<Vec<_>>(),
            "payload_len": payload.len(), "payload_sha256": hex,
            "provenance": {"seed_source": self.provenance.seed_source, "seed_sha256": self.provenance.seed_sha256,
                           "seed_before_ms": self.provenance.seed_before_ms, "producer": self.provenance.producer},
            "cursors": cursors,
            "coverage": {"segments": self.coverage.segments, "gaps": self.coverage.gaps},
        });
        let mut out = serde_json::to_vec(&hdr).unwrap_or_default();
        out.push(b'\n');
        out.extend_from_slice(&payload);
        out
    }

    /// Persist atomically. A crash before the rename leaves the previous checkpoint intact.
    pub fn persist(&self, path: &Path) -> std::io::Result<usize> {
        let body = self.encode();
        let tmp = path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&body)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        if let Some(dir) = path.parent() {
            if let Ok(d) = fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(body.len())
    }
}

/// Outcome of loading a checkpoint.
pub enum Load {
    NeverWritten,
    Untrusted(&'static str),
    Loaded(Box<FlowHistory>),
}

fn parse_i64(v: &Value) -> Option<i64> {
    v.as_i64()
}

/// Decode a checkpoint body under `params`.
pub fn decode(params: FlowParams, body: &[u8]) -> Load {
    let Some(nl) = body.iter().position(|b| *b == b'\n') else {
        return Load::Untrusted("checkpoint_no_header");
    };
    let Ok(h) = serde_json::from_slice::<Value>(&body[..nl]) else {
        return Load::Untrusted("checkpoint_bad_header");
    };
    if h["magic"] != MAGIC {
        return Load::Untrusted("checkpoint_bad_magic");
    }
    if h["file_schema"].as_u64() != Some(FILE_SCHEMA)
        || h["state_schema"].as_u64() != Some(u64::from(STATE_SCHEMA))
    {
        return Load::Untrusted("checkpoint_schema_mismatch");
    }
    let probe = FlowReducer::with_params(params);
    let want: Vec<String> = probe
        .params_fingerprint()
        .iter()
        .map(ToString::to_string)
        .collect();
    let have: Vec<String> = h["params"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if want != have {
        return Load::Untrusted("checkpoint_params_mismatch");
    }
    let payload = &body[nl + 1..];
    if h["payload_len"].as_u64() != Some(payload.len() as u64) {
        return Load::Untrusted("checkpoint_length_mismatch");
    }
    let hex: String = sha256(payload).iter().map(|b| format!("{b:02x}")).collect();
    if h["payload_sha256"].as_str() != Some(hex.as_str()) {
        return Load::Untrusted("checkpoint_hash_mismatch");
    }
    let Ok(reducer) = FlowReducer::decode_state(params, payload) else {
        return Load::Untrusted("checkpoint_payload_refused");
    };
    let p = &h["provenance"];
    let provenance = Provenance {
        seed_source: p["seed_source"].as_str().unwrap_or_default().to_string(),
        seed_sha256: p["seed_sha256"].as_str().unwrap_or_default().to_string(),
        seed_before_ms: p["seed_before_ms"].as_i64().unwrap_or(0),
        producer: p["producer"].as_str().unwrap_or_default().to_string(),
    };
    let mut cursors = BTreeMap::new();
    for c in h["cursors"].as_array().cloned().unwrap_or_default() {
        let (Some(src), Some(hw)) = (c["source"].as_str(), parse_i64(&c["high_water_ms"])) else {
            return Load::Untrusted("checkpoint_bad_cursor");
        };
        let mut ids = BTreeSet::new();
        for i in c["boundary_ids"].as_array().cloned().unwrap_or_default() {
            let Some(id) = i.as_str().and_then(|s| u128::from_str_radix(s, 16).ok()) else {
                return Load::Untrusted("checkpoint_bad_cursor");
            };
            ids.insert(id);
        }
        cursors.insert(
            src.to_string(),
            Cursor {
                high_water_ms: hw,
                boundary_ids: ids,
            },
        );
    }
    let pairs = |v: &Value| -> Option<Vec<(i64, i64)>> {
        v.as_array()?
            .iter()
            .map(|x| Some((x.get(0)?.as_i64()?, x.get(1)?.as_i64()?)))
            .collect()
    };
    let (Some(segments), Some(gaps)) = (
        pairs(&h["coverage"]["segments"]),
        pairs(&h["coverage"]["gaps"]),
    ) else {
        return Load::Untrusted("checkpoint_bad_coverage");
    };
    Load::Loaded(Box::new(FlowHistory {
        reducer,
        provenance,
        cursors,
        coverage: Coverage { segments, gaps },
    }))
}

/// Read and decode `path`.
pub fn load(params: FlowParams, path: &Path) -> Load {
    match fs::read(path) {
        Ok(b) => decode(params, &b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Load::NeverWritten,
        Err(_) => Load::Untrusted("checkpoint_unreadable"),
    }
}

/// What a restore says about continuity. `complete` is true ONLY when the cursor reaches the resume time
/// within `MAX_BRIDGE_MS`; a successful load alone never makes it true.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestoreReport {
    pub unavailable_ms: i64,
    pub complete: bool,
}

impl FlowHistory {
    /// Declare the live feed resumes at `resume_ms`. Any hole between the newest cursor and `resume_ms` is recorded
    /// as a named gap in coverage and reported; the state stays usable but is not described as complete.
    pub fn restore(&mut self, resume_ms: i64) -> RestoreReport {
        let newest = self
            .cursors
            .values()
            .map(|c| c.high_water_ms)
            .max()
            .unwrap_or(i64::MIN);
        if newest == i64::MIN {
            return RestoreReport {
                unavailable_ms: 0,
                complete: false,
            };
        }
        let hole = resume_ms.saturating_sub(newest);
        if hole > MAX_BRIDGE_MS {
            self.coverage.gaps.push((newest, resume_ms));
            RestoreReport {
                unavailable_ms: hole,
                complete: false,
            }
        } else {
            RestoreReport {
                unavailable_ms: 0,
                complete: true,
            }
        }
    }

    /// Elapsed age since the tape origin (what `flow_lookback_d` is computed from) and ACTUAL observed coverage, ms.
    /// The seed's own coverage is not known to this struct; only segments recorded here are counted.
    #[must_use]
    pub fn age_and_coverage(&self, now_ms: i64) -> (i64, i64) {
        let first = self.coverage.segments.first().map_or(now_ms, |s| s.0);
        (now_ms.saturating_sub(first), self.coverage.observed_ms())
    }

    /// (wallets, co-entry links) held: resource accounting for the unpruned state.
    #[must_use]
    pub fn resources(&self) -> (usize, usize) {
        self.reducer.sizes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_market_state::flow_reducer::Side;

    fn ev(t: i64, w: u8, mint: u8, side: Side, lamports: i64) -> FlowEvent {
        FlowEvent {
            mint: [mint; 32],
            trader: [w; 32],
            side,
            slot: t as u64,
            recv_unix_ms: t,
            sol_lamports: lamports,
            fee_lamports: 5000,
            cu_consumed: Some(1),
        }
    }
    fn hist() -> FlowHistory {
        FlowHistory::new(
            FlowParams::default(),
            Provenance {
                seed_source: "t".into(),
                seed_sha256: "00".into(),
                seed_before_ms: 1,
                producer: "x".into(),
            },
        )
    }
    fn fill(h: &mut FlowHistory) {
        let mut st = IngestStats::default();
        for i in 0..40i64 {
            h.ingest(
                "live",
                i as u128 + 1,
                &ev(
                    1_000 + i * 1_000,
                    (i % 7) as u8,
                    9,
                    if i % 3 == 0 { Side::Sell } else { Side::Buy },
                    -1_000_000_000 * (i % 4 + 1),
                ),
                &mut st,
            );
        }
        h.reducer.track_mint([9; 32]);
    }
    fn tmpdir(n: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pq_ckpt_{n}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn roundtrip_is_byte_identical_and_serves_identically() {
        let mut h = hist();
        fill(&mut h);
        let d = tmpdir("rt");
        let p = d.join("flow.ckpt");
        h.persist(&p).unwrap();
        let Load::Loaded(r) = load(FlowParams::default(), &p) else {
            panic!("must load")
        };
        assert_eq!(
            r.encode(),
            h.encode(),
            "re-encoding the restored state reproduces the bytes"
        );
        assert_eq!(
            r.reducer.serve(&[9; 32], 60_000),
            h.reducer.serve(&[9; 32], 60_000)
        );
        assert_eq!(r.cursors, h.cursors);
        assert_eq!(r.coverage, h.coverage);
        assert_eq!(r.provenance, h.provenance);
    }

    #[test]
    fn crash_before_persist_keeps_previous_and_after_persist_has_new() {
        let d = tmpdir("crash");
        let p = d.join("flow.ckpt");
        let mut h = hist();
        fill(&mut h);
        h.persist(&p).unwrap();
        let old = h.encode();
        // more events, then "crash" after writing the temp file but before the rename
        let mut st = IngestStats::default();
        h.ingest(
            "live",
            999,
            &ev(900_000, 3, 9, Side::Buy, -2_000_000_000),
            &mut st,
        );
        fs::write(p.with_extension("tmp"), h.encode()[..50].to_vec()).unwrap(); // torn temp
        let Load::Loaded(r) = load(FlowParams::default(), &p) else {
            panic!()
        };
        assert_eq!(r.encode(), old, "torn temp never replaces the checkpoint");
        h.persist(&p).unwrap(); // the completed persist
        let Load::Loaded(r2) = load(FlowParams::default(), &p) else {
            panic!()
        };
        assert_eq!(
            r2.encode(),
            h.encode(),
            "after the persist the new state is what loads"
        );
    }

    #[test]
    fn duplicate_replay_changes_nothing_and_late_events_are_not_applied() {
        let mut h = hist();
        fill(&mut h);
        let bytes = h.encode();
        let mut st = IngestStats::default();
        // replay the whole stream again from the start (as a restart overlap would)
        for i in 0..40i64 {
            h.ingest(
                "live",
                i as u128 + 1,
                &ev(
                    1_000 + i * 1_000,
                    (i % 7) as u8,
                    9,
                    if i % 3 == 0 { Side::Sell } else { Side::Buy },
                    -1_000_000_000 * (i % 4 + 1),
                ),
                &mut st,
            );
        }
        assert_eq!(st.applied, 0);
        assert_eq!(st.late_or_replayed + st.duplicate, 40);
        assert_eq!(
            h.reducer.encode_state(),
            FlowReducer::decode_state(
                FlowParams::default(),
                &FlowReducer::decode_state(FlowParams::default(), &h.reducer.encode_state())
                    .unwrap()
                    .encode_state()
            )
            .unwrap()
            .encode_state()
        );
        // coverage is the only other thing that could move; it must not
        assert_eq!(
            h.encode(),
            bytes,
            "overlap replay is a no-op on the persisted state"
        );
        // same-millisecond DISTINCT id at the boundary is applied, same id is not
        let mut st2 = IngestStats::default();
        let hw = h.cursors["live"].high_water_ms;
        assert!(h.ingest("live", 7777, &ev(hw, 1, 9, Side::Buy, -1), &mut st2));
        assert!(!h.ingest("live", 7777, &ev(hw, 1, 9, Side::Buy, -1), &mut st2));
        assert_eq!(st2.duplicate, 1);
    }

    #[test]
    fn sources_have_independent_cursors() {
        let mut h = hist();
        let mut st = IngestStats::default();
        assert!(h.ingest("a", 1, &ev(5_000, 1, 9, Side::Buy, -1), &mut st));
        // a second source at an earlier time is NOT late relative to the first source
        assert!(h.ingest("b", 2, &ev(1_000, 2, 9, Side::Buy, -1), &mut st));
        assert_eq!(st.late_or_replayed, 0);
    }

    #[test]
    fn corrupt_truncated_and_incompatible_files_are_untrusted_never_partial() {
        let mut h = hist();
        fill(&mut h);
        let good = h.encode();
        let p = FlowParams::default();
        let reason = |b: &[u8]| match decode(p, b) {
            Load::Untrusted(r) => r,
            Load::Loaded(_) => "LOADED",
            Load::NeverWritten => "never",
        };
        let mut flip = good.clone();
        let n = flip.len();
        flip[n - 10] ^= 0xFF;
        assert_eq!(reason(&flip), "checkpoint_hash_mismatch");
        assert_eq!(
            reason(&good[..good.len() - 5]),
            "checkpoint_length_mismatch"
        );
        assert_eq!(reason(b"garbage"), "checkpoint_no_header");
        assert_eq!(reason(b"{not json}\nxx"), "checkpoint_bad_header");
        let other = FlowParams {
            smart_mints: 4,
            ..FlowParams::default()
        };
        assert!(matches!(
            decode(other, &good),
            Load::Untrusted("checkpoint_params_mismatch")
        ));
        let s = String::from_utf8_lossy(&good[..good.iter().position(|b| *b == b'\n').unwrap()])
            .replace("\"file_schema\":1", "\"file_schema\":2");
        let mut bad_schema = s.into_bytes();
        bad_schema.push(b'\n');
        bad_schema.extend_from_slice(&good[good.iter().position(|b| *b == b'\n').unwrap() + 1..]);
        assert_eq!(reason(&bad_schema), "checkpoint_schema_mismatch");
        // a payload that hashes right but is structurally wrong is refused by the reducer
        let hdr_end = good.iter().position(|b| *b == b'\n').unwrap();
        let payload = vec![9u8; 40];
        let hex: String = sha256(&payload)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut hv: Value = serde_json::from_slice(&good[..hdr_end]).unwrap();
        hv["payload_len"] = json!(payload.len());
        hv["payload_sha256"] = json!(hex);
        let mut forged = serde_json::to_vec(&hv).unwrap();
        forged.push(b'\n');
        forged.extend_from_slice(&payload);
        assert_eq!(reason(&forged), "checkpoint_payload_refused");
        assert!(matches!(
            load(p, Path::new("/nonexistent/zzz.ckpt")),
            Load::NeverWritten
        ));
    }

    #[test]
    fn a_loaded_checkpoint_across_a_hole_reports_the_unavailable_interval_and_is_not_complete() {
        let mut h = hist();
        fill(&mut h);
        let newest = h.cursors["live"].high_water_ms;
        let mut ok = FlowHistory::new(FlowParams::default(), h.provenance.clone());
        ok.cursors = h.cursors.clone();
        assert_eq!(
            ok.restore(newest + 10_000),
            RestoreReport {
                unavailable_ms: 0,
                complete: true
            }
        );
        let rep = h.restore(newest + 3_600_000);
        assert_eq!(
            rep,
            RestoreReport {
                unavailable_ms: 3_600_000,
                complete: false
            }
        );
        assert_eq!(h.coverage.gaps, vec![(newest, newest + 3_600_000)]);
        // the gap survives a persist/load
        let Load::Loaded(r) = decode(FlowParams::default(), &h.encode()) else {
            panic!()
        };
        assert_eq!(r.coverage.gaps, h.coverage.gaps);
        // a never-fed history is never "complete"
        assert!(!hist().restore(5).complete);
    }

    #[test]
    fn elapsed_age_and_observed_coverage_are_different_numbers() {
        let mut h = hist();
        let mut st = IngestStats::default();
        h.ingest("live", 1, &ev(0, 1, 9, Side::Buy, -1), &mut st);
        h.ingest("live", 2, &ev(30_000, 1, 9, Side::Buy, -1), &mut st);
        // a 6 h hole, then 30 s more
        h.ingest("live", 3, &ev(21_600_000, 1, 9, Side::Buy, -1), &mut st);
        h.ingest("live", 4, &ev(21_630_000, 1, 9, Side::Buy, -1), &mut st);
        let (age, observed) = h.age_and_coverage(21_630_000);
        assert_eq!(age, 21_630_000);
        assert_eq!(
            observed, 60_000,
            "only the two 30 s stretches were observed"
        );
        assert_eq!(h.coverage.segments.len(), 2);
    }

    #[test]
    fn resources_report_the_unpruned_graph() {
        let mut h = hist();
        let mut st = IngestStats::default();
        for i in 0..6u8 {
            h.ingest(
                "live",
                u128::from(i) + 1,
                &ev(1_000 + i64::from(i), i, 9, Side::Buy, -1),
                &mut st,
            );
        }
        let (wallets, links) = h.resources();
        assert_eq!(wallets, 6);
        assert_eq!(
            links,
            6 * 5,
            "every early-buyer pair is linked both ways; nothing pruned"
        );
    }
}
