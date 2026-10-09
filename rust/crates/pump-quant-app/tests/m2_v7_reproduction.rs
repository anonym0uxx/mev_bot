//! M2 — offline reproduction of the TRAINED `creator_past_launches` for all 19,883 rows of the
//! trained launch table, via the Rust registry and the real engine path, with a restart.
//!
//! Inputs (read-only):
//! * `PQ_M2_V7_TABLE` (default `/training/v2/canonical/renormalized_v7/launches.jsonl`), sha256 ed1fa965…
//! * `PQ_M2_V7_COUNTS` (default `/training/mh_build/proc/m2reg/v7_trained_counts.jsonl`): the trained
//!   counts, produced by the corpus's own rule (`build_c9_enrichment_full.py::prior_launches`,
//!   bisect_left over the creator's launch times) — computed independently in Python.
//!
//! Construction: cutoff = the bF2 replay boundary (first launch of the 09-09 07:49 PT session,
//! 1788965350761). Rows strictly before are the SEED; rows at/after are fed as `LaunchObserved`
//! in receive order through `Engine::tick`, with a process restart (drop + reattach from the log)
//! half-way and an overlap replay of 500 launches. Missing inputs FAIL the test (never a skip).

use std::collections::BTreeMap;

use pump_quant_app::config::Config;
use pump_quant_app::creator_registry::{b58_32, SeedSpec};
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;
use serde_json::Value;

const CUTOFF: i64 = 1_788_965_350_761;
const FILTERED_SHA: &str = "70eb25ecd830073f10e4bd3214429675d798edb7608adfa525e122b995a2ee56";

struct Never;
impl ModelSource for Never {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        Err(InferenceError::Transport("never".into()))
    }
}

fn engine() -> Engine {
    let mut e = Engine::new(Config::dev_portable(), RunMode::Paper);
    e.enable_paper_model(Never);
    e
}

#[test]
fn the_rust_registry_reproduces_all_trained_counts_across_a_restart() {
    let table = std::env::var("PQ_M2_V7_TABLE")
        .unwrap_or_else(|_| "/training/v2/canonical/renormalized_v7/launches.jsonl".into());
    let counts = std::env::var("PQ_M2_V7_COUNTS")
        .unwrap_or_else(|_| "/training/mh_build/proc/m2reg/v7_trained_counts.jsonl".into());
    let body = std::fs::read_to_string(&table).expect("trained launch table must be readable");
    let mut rows: Vec<([u8; 32], [u8; 32], i64)> = Vec::new();
    for l in body.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(l).expect("row");
        rows.push((
            b58_32(v["mint"].as_str().expect("mint")).expect("mint b58"),
            b58_32(v["creator"].as_str().expect("creator")).expect("creator b58"),
            v["recv_unix_ms"].as_i64().expect("recv"),
        ));
    }
    assert_eq!(rows.len(), 19_883);
    let mut want: BTreeMap<[u8; 32], i64> = BTreeMap::new();
    for l in std::fs::read_to_string(&counts)
        .expect("trained counts must be readable")
        .lines()
    {
        let v: Value = serde_json::from_str(l).expect("count row");
        want.insert(
            b58_32(v["mint"].as_str().expect("mint")).expect("b58"),
            v["creator_past_launches"].as_i64().expect("n"),
        );
    }
    assert_eq!(want.len(), 19_883);

    let spec = SeedSpec {
        path: table.clone().into(),
        cutoff_ms: CUTOFF,
        label: "capture_v7".into(),
        expect_sha256: Some(FILTERED_SHA.into()),
    };
    let dir = std::env::temp_dir().join(format!("pq_m2reg_v7_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp");
    let log = dir.join("observed_launches.jsonl");

    // Live = rows at/after the cutoff, receive order (stable on file position).
    let mut live: Vec<_> = rows.iter().copied().filter(|r| r.2 >= CUTOFF).collect();
    live.sort_by_key(|r| r.2);
    let half = live.len() / 2;

    let mut e1 = engine();
    let st = e1
        .model_creator_registry_attach(Some(&spec), &log, CUTOFF, "s1")
        .expect("attach");
    assert_eq!(st.seeded, 11_706);
    assert_eq!(st.seed_rows_at_or_after_cutoff as usize, live.len());
    for r in &live[..half] {
        e1.tick(AppEvent::LaunchObserved {
            mint: DomainMint::from_bytes(r.0),
            creator: r.1,
            launch_unix_ms: r.2,
        });
    }
    drop(e1);
    let mut e2 = engine();
    let st2 = e2
        .model_creator_registry_attach(Some(&spec), &log, CUTOFF, "s2")
        .expect("reattach");
    assert_eq!(st2.restored_from_log as usize, half);
    for r in &live[half.saturating_sub(500)..] {
        e2.tick(AppEvent::LaunchObserved {
            mint: DomainMint::from_bytes(r.0),
            creator: r.1,
            launch_unix_ms: r.2,
        });
    }

    let mut exact = 0usize;
    let mut first_bad = None;
    for (m, n) in &want {
        let d = e2.model_creator_dev_history(m);
        if d.creator_past_launches == Some(*n) && d.creator_known == 1 {
            exact += 1;
        } else if first_bad.is_none() {
            first_bad = Some((*m, *n, d));
        }
    }
    eprintln!("M2 v7 reproduction: exact {exact}/19883");
    assert_eq!(exact, 19_883, "first mismatch: {first_bad:?}");
    assert_eq!(e2.model_creator_registry_len(), 19_883);
    let max = want.values().copied().max().unwrap_or(0);
    assert_eq!(max, 227);
    let lines = std::fs::read_to_string(&log).expect("log").lines().count();
    assert_eq!(lines, 1 + live.len(), "the overlap replay appended nothing");
}
