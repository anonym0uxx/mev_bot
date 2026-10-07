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
//! applied or late (recorded durably as LATE (not a duplicate), never applied: a late event never rewrites earlier decisions);
//! `recv == high_water` => applied only if its id is not in the boundary set; `recv > high_water` => applied.

use std::collections::BTreeMap;
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

/// Ids seen are remembered this far behind the high-water mark. An id older than that is no longer provable as a
/// duplicate, so an unseen-looking event older than the window is treated as LATE (a gap), never silently dropped.
pub const OVERLAP_MS: i64 = 120_000;
/// Late-event records kept durably; beyond this the overflow counter makes the whole history entry-refusing.
pub const LATE_CAP: usize = 4096;

/// Per-source resume cursor. Source guarantee assumed: events of ONE source are delivered in nondecreasing receipt
/// time except for redelivery (at-least-once). An event that violates this and is not a proven duplicate is LATE.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub high_water_ms: i64,
    /// id -> recv_ms for ids at/after `high_water_ms - OVERLAP_MS` (applied or recorded-late).
    pub recent: BTreeMap<u128, i64>,
}

/// An event that arrived older than its source's high-water mark and was not a proven duplicate. It was NOT applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LateRecord {
    pub source: String,
    pub id: u128,
    pub recv_ms: i64,
    pub high_water_ms: i64,
    pub mint: [u8; 32],
}

/// Result of offering one event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    Applied,
    /// Proven duplicate: the same id is in the overlap window.
    Duplicate,
    /// Not applied; a durable late record was written (dependency-scoped refusal follows).
    LateUnseen,
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
    pub late_unseen: u64,
}

/// Everything about the history EXCEPT the reducer: provenance, per-source cursors, coverage, late records, waivers.
/// Small (late records are capped); cloned together with the reducer to make one consistent snapshot.
#[derive(Clone, Debug)]
pub struct FlowMeta {
    pub provenance: Provenance,
    pub cursors: BTreeMap<String, Cursor>,
    pub coverage: Coverage,
    /// Durable late-event records (not applied).
    pub late: Vec<LateRecord>,
    /// Late events beyond [`LATE_CAP`] (counted, mint unknown): entry is refused everywhere while nonzero.
    pub late_overflow: u64,
    /// Operator waivers `(from, to, reason)` of interval gaps: REPORTED, never described as complete coverage.
    pub waived: Vec<(i64, i64, String)>,
}

/// The live flow-history state: the reducer plus its metadata.
pub struct FlowHistory {
    pub reducer: FlowReducer,
    pub meta: FlowMeta,
}

impl std::ops::Deref for FlowHistory {
    type Target = FlowMeta;
    fn deref(&self) -> &FlowMeta {
        &self.meta
    }
}
impl std::ops::DerefMut for FlowHistory {
    fn deref_mut(&mut self) -> &mut FlowMeta {
        &mut self.meta
    }
}

impl FlowHistory {
    #[must_use]
    pub fn new(params: FlowParams, provenance: Provenance) -> Self {
        Self {
            reducer: FlowReducer::with_params(params),
            meta: FlowMeta::new(provenance),
        }
    }

    /// Offer one event; applies it to the reducer only when admitted.
    pub fn offer(
        &mut self,
        source: &str,
        event_id: u128,
        e: &FlowEvent,
        st: &mut IngestStats,
    ) -> Offer {
        let o = self
            .meta
            .admit(source, event_id, e.recv_unix_ms, e.mint, st);
        if o == Offer::Applied {
            self.reducer.on_event(e);
        }
        o
    }

    /// Back-compat boolean form: `true` only when applied.
    pub fn ingest(
        &mut self,
        source: &str,
        event_id: u128,
        e: &FlowEvent,
        st: &mut IngestStats,
    ) -> bool {
        self.offer(source, event_id, e, st) == Offer::Applied
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.meta.encode_with(&self.reducer)
    }

    /// Persist atomically. A crash before the rename leaves the previous checkpoint intact.
    pub fn persist(&self, path: &Path) -> std::io::Result<usize> {
        write_atomic(path, &self.encode())
    }

    /// (wallets, co-entry links) held: resource accounting for the unpruned state.
    #[must_use]
    pub fn resources(&self) -> (usize, usize) {
        self.reducer.sizes()
    }
}

/// Atomic durable write: temp file, fsync, rename, fsync directory.
pub fn write_atomic(path: &Path, body: &[u8]) -> std::io::Result<usize> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body)?;
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

impl FlowMeta {
    #[must_use]
    pub fn new(provenance: Provenance) -> Self {
        Self {
            provenance,
            cursors: BTreeMap::new(),
            coverage: Coverage::default(),
            late: Vec::new(),
            late_overflow: 0,
            waived: Vec::new(),
        }
    }

    /// Admission decision for one event of `source` (no reducer access). Proven duplicates add nothing. An unseen
    /// event older than the source's high-water mark is NEVER discarded as if it were a duplicate: it is recorded
    /// durably as a late event and NOT applied (the reducer is order-dependent, so an in-place repair is not exact),
    /// and the affected prompts refuse by dependency scope.
    pub fn admit(
        &mut self,
        source: &str,
        event_id: u128,
        t: i64,
        mint: [u8; 32],
        st: &mut IngestStats,
    ) -> Offer {
        let c = self.cursors.entry(source.to_string()).or_default();
        if c.recent.contains_key(&event_id) {
            st.duplicate += 1;
            return Offer::Duplicate;
        }
        if t < c.high_water_ms {
            c.recent.insert(event_id, t);
            st.late_unseen += 1;
            if self.late.len() < LATE_CAP {
                self.late.push(LateRecord {
                    source: source.to_string(),
                    id: event_id,
                    recv_ms: t,
                    high_water_ms: c.high_water_ms,
                    mint,
                });
            } else {
                self.late_overflow += 1;
            }
            return Offer::LateUnseen;
        }
        c.high_water_ms = t;
        c.recent.insert(event_id, t);
        if c.recent.len() > 1024 {
            let floor = t.saturating_sub(OVERLAP_MS);
            c.recent.retain(|_, r| *r >= floor);
        }
        self.coverage.observe(t);
        st.applied += 1;
        Offer::Applied
    }

    /// Record an event the caller could not apply (e.g. older than its mint's newest print) as a durable late record,
    /// unless its id is already known. Never marks the event applied.
    pub fn record_late(
        &mut self,
        source: &str,
        event_id: u128,
        t: i64,
        mint: [u8; 32],
        st: &mut IngestStats,
    ) -> Offer {
        let c = self.cursors.entry(source.to_string()).or_default();
        if c.recent.contains_key(&event_id) {
            st.duplicate += 1;
            return Offer::Duplicate;
        }
        c.recent.insert(event_id, t);
        st.late_unseen += 1;
        if self.late.len() < LATE_CAP {
            self.late.push(LateRecord {
                source: source.to_string(),
                id: event_id,
                recv_ms: t,
                high_water_ms: c.high_water_ms.max(t),
                mint,
            });
        } else {
            self.late_overflow += 1;
        }
        Offer::LateUnseen
    }

    /// Dependency-scoped readiness: why (if at all) a decision at `t_dec_ms` for `mint` cannot rely on this history.
    /// * a late event of THIS mint inside the decision's 300 s window (both audiences);
    /// * late-record overflow (entry);
    /// * an unwaived feed gap: entry refuses once any such gap lies before the decision (cumulative wallet state is
    ///   short by an unknown amount); management refuses only if the gap overlaps its 300 s window.
    #[must_use]
    pub fn scope_refusal(
        &self,
        mint: &[u8; 32],
        t_dec_ms: i64,
        entry: bool,
    ) -> Option<(&'static str, i64, i64)> {
        let lo = t_dec_ms.saturating_sub(pump_quant_market_state::flow_reducer::WINDOW_300_MS);
        if let Some(l) = self
            .late
            .iter()
            .find(|l| l.mint == *mint && l.recv_ms >= lo && l.recv_ms < t_dec_ms)
        {
            return Some(("late_event_in_window", l.recv_ms, l.high_water_ms));
        }
        if entry && self.late_overflow > 0 {
            return Some(("late_overflow", 0, 0));
        }
        for &(a, b) in &self.coverage.gaps {
            if self.waived.iter().any(|w| w.0 <= a && b <= w.1) {
                continue;
            }
            if b > t_dec_ms {
                continue;
            }
            if entry || b > lo {
                return Some(("feed_gap", a, b));
            }
        }
        None
    }

    #[must_use]
    pub fn encode_with(&self, reducer: &FlowReducer) -> Vec<u8> {
        let payload = reducer.encode_state();
        let digest = sha256(&payload);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let cursors: Vec<Value> = self
            .cursors
            .iter()
            .map(|(k, c)| {
                json!({"source": k, "high_water_ms": c.high_water_ms,
                       "recent": c.recent.iter().map(|(i, r)| format!("{i:032x}:{r}")).collect::<Vec<_>>()})
            })
            .collect();
        let hdr = json!({
            "magic": MAGIC, "file_schema": FILE_SCHEMA, "state_schema": STATE_SCHEMA,
            "params": reducer.params_fingerprint().iter().map(ToString::to_string).collect::<Vec<_>>(),
            "payload_len": payload.len(), "payload_sha256": hex,
            "provenance": {"seed_source": self.provenance.seed_source, "seed_sha256": self.provenance.seed_sha256,
                           "seed_before_ms": self.provenance.seed_before_ms, "producer": self.provenance.producer},
            "cursors": cursors,
            "coverage": {"segments": self.coverage.segments, "gaps": self.coverage.gaps},
            "late": self.late.iter().map(|l| json!({"source": l.source, "id": format!("{:032x}", l.id), "recv_ms": l.recv_ms,
                       "high_water_ms": l.high_water_ms, "mint": l.mint.iter().map(|b| format!("{b:02x}")).collect::<String>()})).collect::<Vec<_>>(),
            "late_overflow": self.late_overflow,
            "waived": self.waived.iter().map(|w| json!([w.0, w.1, w.2])).collect::<Vec<_>>(),
        });
        let mut out = serde_json::to_vec(&hdr).unwrap_or_default();
        out.push(b'\n');
        out.extend_from_slice(&payload);
        out
    }
}

/// Background checkpoint writer. The engine thread hands over ONE consistent clone (reducer + matching meta); encoding and
/// disk IO happen on the writer thread, latest-wins. A failed write never advances `durable_seq` (the durable cursor).
pub struct CkptWriter {
    slot: std::sync::Arc<(std::sync::Mutex<CkptSlot>, std::sync::Condvar)>,
    submitted: std::sync::atomic::AtomicU64,
    pub durable_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub consecutive_failures: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub total_failures: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Last write: bytes and microseconds spent encoding / writing (observability).
    pub last_bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub last_encode_us: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub last_write_us: std::sync::Arc<std::sync::atomic::AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

struct CkptSlot {
    pending: Option<(u64, FlowReducer, FlowMeta)>,
    shutdown: bool,
}

impl CkptWriter {
    /// `fail_hook` (tests) forces writes to fail.
    #[must_use]
    pub fn start(
        path: std::path::PathBuf,
        fail_hook: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::{Arc, Condvar, Mutex};
        let slot = Arc::new((
            Mutex::new(CkptSlot {
                pending: None,
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let durable = Arc::new(AtomicU64::new(0));
        let cf = Arc::new(AtomicU64::new(0));
        let tf = Arc::new(AtomicU64::new(0));
        let lb = Arc::new(AtomicU64::new(0));
        let le = Arc::new(AtomicU64::new(0));
        let lw = Arc::new(AtomicU64::new(0));
        let (s2, d2, c2, t2, lb2, le2, lw2) = (
            slot.clone(),
            durable.clone(),
            cf.clone(),
            tf.clone(),
            lb.clone(),
            le.clone(),
            lw.clone(),
        );
        let handle = std::thread::Builder::new()
            .name("flow-ckpt-writer".into())
            .spawn(move || loop {
                let job = {
                    let (m, cv) = &*s2;
                    let Ok(mut g) = m.lock() else { return };
                    while g.pending.is_none() && !g.shutdown {
                        g = match cv.wait(g) {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                    }
                    match g.pending.take() {
                        Some(j) => j,
                        None => return,
                    }
                };
                let t0 = std::time::Instant::now();
                let body = job.2.encode_with(&job.1);
                le2.store(t0.elapsed().as_micros() as u64, Ordering::SeqCst);
                let t1 = std::time::Instant::now();
                let forced = fail_hook.as_ref().is_some_and(|f| f.load(Ordering::SeqCst));
                let r = if forced {
                    Err(std::io::Error::other("forced failure"))
                } else {
                    write_atomic(&path, &body)
                };
                lw2.store(t1.elapsed().as_micros() as u64, Ordering::SeqCst);
                match r {
                    Ok(n) => {
                        lb2.store(n as u64, Ordering::SeqCst);
                        d2.store(job.0, Ordering::SeqCst); // the durable cursor advances ONLY here
                        c2.store(0, Ordering::SeqCst);
                    }
                    Err(_) => {
                        c2.fetch_add(1, Ordering::SeqCst);
                        t2.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
            .ok();
        Self {
            slot,
            submitted: AtomicU64::new(0),
            durable_seq: durable,
            consecutive_failures: cf,
            total_failures: tf,
            last_bytes: lb,
            last_encode_us: le,
            last_write_us: lw,
            handle,
        }
    }

    /// Hand over one consistent snapshot; never blocks on disk. Returns its sequence.
    pub fn submit(&self, reducer: FlowReducer, meta: FlowMeta) -> u64 {
        let seq = self
            .submitted
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let (m, cv) = &*self.slot;
        if let Ok(mut g) = m.lock() {
            g.pending = Some((seq, reducer, meta));
            cv.notify_one();
        }
        seq
    }

    #[must_use]
    pub fn durable(&self) -> u64 {
        self.durable_seq.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Wait until `seq` is durable (shutdown / tests only; never the tick path).
    pub fn wait_durable(&self, seq: u64, timeout: std::time::Duration) -> bool {
        let t0 = std::time::Instant::now();
        while t0.elapsed() < timeout {
            if self.durable() >= seq {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        self.durable() >= seq
    }
}

impl Drop for CkptWriter {
    fn drop(&mut self) {
        {
            let (m, cv) = &*self.slot;
            if let Ok(mut g) = m.lock() {
                g.shutdown = true;
                cv.notify_one();
            }
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
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
        let mut ids = BTreeMap::new();
        for i in c["recent"].as_array().cloned().unwrap_or_default() {
            let Some((id, r)) = i
                .as_str()
                .and_then(|x| x.split_once(':'))
                .and_then(|(a, b)| {
                    Some((u128::from_str_radix(a, 16).ok()?, b.parse::<i64>().ok()?))
                })
            else {
                return Load::Untrusted("checkpoint_bad_cursor");
            };
            ids.insert(id, r);
        }
        cursors.insert(
            src.to_string(),
            Cursor {
                high_water_ms: hw,
                recent: ids,
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
    let mut late = Vec::new();
    for l in h["late"].as_array().cloned().unwrap_or_default() {
        let hex32 = |v: &Value| -> Option<[u8; 32]> {
            let s = v.as_str()?;
            if s.len() != 64 || !s.is_ascii() {
                return None;
            }
            let mut o = [0u8; 32];
            for (i, b) in o.iter_mut().enumerate() {
                *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
            }
            Some(o)
        };
        let (Some(src), Some(id), Some(rm), Some(hw), Some(mint)) = (
            l["source"].as_str(),
            l["id"]
                .as_str()
                .and_then(|x| u128::from_str_radix(x, 16).ok()),
            l["recv_ms"].as_i64(),
            l["high_water_ms"].as_i64(),
            hex32(&l["mint"]),
        ) else {
            return Load::Untrusted("checkpoint_bad_late");
        };
        late.push(LateRecord {
            source: src.to_string(),
            id,
            recv_ms: rm,
            high_water_ms: hw,
            mint,
        });
    }
    let mut waived = Vec::new();
    for w in h["waived"].as_array().cloned().unwrap_or_default() {
        let (Some(a), Some(b), Some(r)) = (
            w.get(0).and_then(Value::as_i64),
            w.get(1).and_then(Value::as_i64),
            w.get(2).and_then(Value::as_str),
        ) else {
            return Load::Untrusted("checkpoint_bad_waiver");
        };
        waived.push((a, b, r.to_string()));
    }
    Load::Loaded(Box::new(FlowHistory {
        reducer,
        meta: FlowMeta {
            provenance,
            cursors,
            coverage: Coverage { segments, gaps },
            late,
            late_overflow: h["late_overflow"].as_u64().unwrap_or(0),
            waived,
        },
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

impl FlowMeta {
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
        assert_eq!(st.late_unseen + st.duplicate, 40);
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
        assert_eq!(st.late_unseen, 0);
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

    #[test]
    fn an_unseen_earlier_event_is_late_not_duplicate_and_survives_restart() {
        let d = tmpdir("late");
        let p = d.join("flow.ckpt");
        let mut h = hist();
        fill(&mut h); // ids 1..=40 up to t=40_000 on mint 9
        let mint = [9u8; 32];
        let t_dec = 41_000i64;
        let before = h.reducer.serve(&mint, 20_000);
        let mut st = IngestStats::default();
        // a NEW id with a receipt time older than the high-water mark
        assert_eq!(
            h.offer(
                "live",
                5000,
                &ev(15_500, 3, 9, Side::Buy, -9_000_000_000),
                &mut st
            ),
            Offer::LateUnseen
        );
        assert_eq!((st.late_unseen, st.duplicate, st.applied), (1, 0, 0));
        // an EARLIER decision is byte-identical: the late event is never folded in
        assert_eq!(h.reducer.serve(&mint, 20_000), before);
        // dependency-scoped: this mint's window containing it refuses; another mint and a later window do not
        assert_eq!(
            h.scope_refusal(&mint, 16_000, true).map(|x| x.0),
            Some("late_event_in_window")
        );
        assert_eq!(h.scope_refusal(&[1u8; 32], 16_000, true), None);
        assert_eq!(
            h.scope_refusal(&mint, 16_000 + 400_000, true),
            None,
            "leaves the 300 s window"
        );
        let _ = t_dec;
        // restart: the durable late record and cursor come back; the same late id redelivered is a DUPLICATE
        h.persist(&p).unwrap();
        let Load::Loaded(mut r) = load(FlowParams::default(), &p) else {
            panic!()
        };
        assert_eq!(r.late.len(), 1);
        assert_eq!(
            r.scope_refusal(&mint, 16_000, true).map(|x| x.0),
            Some("late_event_in_window")
        );
        let mut st2 = IngestStats::default();
        assert_eq!(
            r.offer(
                "live",
                5000,
                &ev(15_500, 3, 9, Side::Buy, -9_000_000_000),
                &mut st2
            ),
            Offer::Duplicate
        );
        assert_eq!(r.late.len(), 1, "redelivery adds no second late record");
        // and a different unseen older id after the restart is late again, not applied
        assert_eq!(
            r.offer("live", 5001, &ev(15_600, 4, 9, Side::Buy, -1), &mut st2),
            Offer::LateUnseen
        );
        assert_eq!(r.late.len(), 2);
    }

    #[test]
    fn an_id_older_than_the_overlap_window_is_late_not_a_proven_duplicate() {
        let mut h = hist();
        let mut st = IngestStats::default();
        for i in 0..1300i64 {
            h.ingest(
                "live",
                i as u128 + 1,
                &ev(1_000 + i * 1_000, 1, 9, Side::Buy, -1),
                &mut st,
            );
        }
        // id 1 was applied long ago and has aged out of the id window: no longer PROVABLE as a duplicate
        assert_eq!(
            h.offer("live", 1, &ev(1_000, 1, 9, Side::Buy, -1), &mut st),
            Offer::LateUnseen
        );
        assert_eq!(st.late_unseen, 1);
        assert_eq!(h.late.len(), 1);
    }

    #[test]
    fn late_record_overflow_refuses_entry_everywhere_and_is_never_silent() {
        let mut h = hist();
        let mut st = IngestStats::default();
        h.ingest("live", 1, &ev(1_000_000, 1, 9, Side::Buy, -1), &mut st);
        for i in 0..(LATE_CAP as u128 + 5) {
            h.offer(
                "live",
                10 + i,
                &ev(10 + i as i64, 1, 9, Side::Buy, -1),
                &mut st,
            );
        }
        assert_eq!(h.late.len(), LATE_CAP);
        assert_eq!(h.late_overflow, 5);
        assert_eq!(
            h.scope_refusal(&[77u8; 32], 2_000_000, true).map(|x| x.0),
            Some("late_overflow")
        );
    }

    #[test]
    fn a_feed_gap_blocks_entry_until_waived_and_a_waiver_is_reported_not_complete() {
        let mut h = hist();
        let mut st = IngestStats::default();
        h.ingest("live", 1, &ev(1_000, 1, 9, Side::Buy, -1), &mut st);
        h.coverage.gaps.push((2_000, 900_000));
        let m = [9u8; 32];
        assert_eq!(
            h.scope_refusal(&m, 2_000_000, true).map(|x| x.0),
            Some("feed_gap")
        );
        assert_eq!(
            h.scope_refusal(&m, 2_000_000, false),
            None,
            "management window no longer overlaps the gap"
        );
        assert_eq!(
            h.scope_refusal(&m, 1_000_000, false).map(|x| x.0),
            Some("feed_gap")
        );
        h.waived.push((2_000, 900_000, "operator".into()));
        assert_eq!(h.scope_refusal(&m, 2_000_000, true), None);
        assert!(
            !h.coverage.gaps.is_empty(),
            "the gap itself is still on record"
        );
    }
}
