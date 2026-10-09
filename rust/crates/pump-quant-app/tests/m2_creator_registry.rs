//! M2 — persistent causal observed-launch creator registry, through the REAL engine path
//! (`Engine::tick(AppEvent::LaunchObserved)` -> `DecisionCache` -> `CreatorHistory`).
//!
//! Every guard has a test that fails when the guard is mutated (see proc/SLICE_m2reg_REPORT.md).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::creator_registry::{load_seed, RegistryRefusal, SeedSpec, PROVENANCE_LABEL};
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;

const SKIP: &str = "DECISION: SKIP\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: x";

struct Stub {
    calls: Arc<AtomicUsize>,
}
impl ModelSource for Stub {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(SKIP.to_string())
    }
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pq_m2reg_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("tmp");
    d
}

fn armed() -> (Engine, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    let mut e = Engine::new(c, RunMode::Paper);
    e.enable_paper_model(Stub {
        calls: Arc::clone(&calls),
    });
    (e, calls)
}

fn key(i: u32, tag: u8) -> [u8; 32] {
    let mut k = [0u8; 32];
    k[0] = tag;
    k[1..5].copy_from_slice(&i.to_le_bytes());
    k[31] = 7;
    k
}

const B58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
fn b58(k: &[u8; 32]) -> String {
    let mut digits: Vec<u8> = Vec::new();
    for &b in k {
        let mut carry = u32::from(b);
        for d in digits.iter_mut() {
            carry += u32::from(*d) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut s: String = k.iter().take_while(|&&b| b == 0).map(|_| '1').collect();
    s.extend(digits.iter().rev().map(|&d| B58[d as usize] as char));
    s
}

fn launch(mint: [u8; 32], creator: [u8; 32], t: i64) -> AppEvent {
    AppEvent::LaunchObserved {
        mint: DomainMint::from_bytes(mint),
        creator,
        launch_unix_ms: t,
    }
}

fn write_seed(path: &std::path::Path, rows: &[([u8; 32], [u8; 32], i64)]) {
    let mut s = String::new();
    for (m, c, t) in rows {
        s.push_str(&format!(
            "{{\"mint\":\"{}\",\"creator\":\"{}\",\"slot\":1,\"recv_unix_ms\":{t},\"signature\":\"x\",\"failed\":false}}\n",
            b58(m),
            b58(c)
        ));
    }
    std::fs::write(path, s).expect("seed");
}

/// The corpus's count for a launch table, computed independently here (bisect over the whole
/// table, strict-before) — the oracle every registry count is compared against.
fn oracle(rows: &[([u8; 32], [u8; 32], i64)]) -> std::collections::BTreeMap<[u8; 32], i64> {
    let mut first: std::collections::BTreeMap<[u8; 32], ([u8; 32], i64)> = Default::default();
    for (m, c, t) in rows {
        first.entry(*m).or_insert((*c, *t));
    }
    first
        .iter()
        .map(|(m, (c, t))| {
            let n = first.values().filter(|(c2, t2)| c2 == c && t2 < t).count() as i64;
            (*m, n)
        })
        .collect()
}

/// A deterministic launch population: 6 creators, uneven bursts, same-ms twins.
fn population() -> Vec<([u8; 32], [u8; 32], i64)> {
    let mut v = Vec::new();
    for i in 0..120u32 {
        let creator = key(i % 6 + (i % 5 == 0) as u32 * 0, 0xC0 + (i % 6) as u8);
        let t = 1_000_000 + i64::from(i / 2) * 1_000; // pairs share a millisecond
        v.push((key(i, 0xA0), creator, t));
    }
    v
}

const CUTOFF: i64 = 1_000_000 + 30 * 1_000; // rows 0..60 are before, 60.. are at/after

fn spec(path: &std::path::Path, cutoff: i64) -> SeedSpec {
    SeedSpec {
        path: path.to_path_buf(),
        cutoff_ms: cutoff,
        label: "capture_v7_test".into(),
        expect_sha256: None,
    }
}

#[test]
fn the_seed_keeps_only_rows_strictly_before_the_cutoff() {
    let d = dir("cutoff");
    let pop = population();
    write_seed(&d.join("seed.jsonl"), &pop);
    let s = load_seed(&spec(&d.join("seed.jsonl"), CUTOFF)).expect("seed");
    let before = pop.iter().filter(|r| r.2 < CUTOFF).count();
    assert_eq!(s.rows.len(), before, "only rows strictly before the cutoff");
    assert_eq!(s.rows_at_or_after_cutoff as usize, pop.len() - before);
    assert!(s.rows.iter().all(|r| r.recv_unix_ms < CUTOFF));
    // A row exactly AT the cutoff is excluded (strict).
    assert!(pop.iter().any(|r| r.2 == CUTOFF));
    // The pinned sha256 is over the filtered content: a different cutoff changes it, and a wrong pin refuses.
    let s2 = load_seed(&spec(&d.join("seed.jsonl"), CUTOFF + 1)).expect("seed");
    assert_ne!(s.sha256, s2.sha256);
    let mut bad = spec(&d.join("seed.jsonl"), CUTOFF);
    bad.expect_sha256 = Some(s2.sha256.clone());
    assert!(matches!(
        load_seed(&bad),
        Err(RegistryRefusal::SeedSha256Mismatch { .. })
    ));
    let mut ok = spec(&d.join("seed.jsonl"), CUTOFF);
    ok.expect_sha256 = Some(s.sha256.clone());
    assert!(load_seed(&ok).is_ok());
}

#[test]
fn a_seeded_engine_never_sees_a_launch_from_its_own_future() {
    // The seed table contains the WHOLE population; the run starts at CUTOFF. A creator's launch at
    // or after the cutoff must not be counted for a launch observed live AT the cutoff.
    let d = dir("future");
    let pop = population();
    write_seed(&d.join("seed.jsonl"), &pop);
    let (mut e, _c) = armed();
    let st = e
        .model_creator_registry_attach(
            Some(&spec(&d.join("seed.jsonl"), CUTOFF)),
            &d.join("log.jsonl"),
            CUTOFF,
            "t",
        )
        .expect("attach");
    assert_eq!(
        st.seeded as usize,
        pop.iter().filter(|r| r.2 < CUTOFF).count()
    );
    // A fresh live launch by creator 0 exactly at the cutoff counts ONLY pre-cutoff launches.
    let c0 = pop[0].1;
    let prior = pop.iter().filter(|r| r.1 == c0 && r.2 < CUTOFF).count() as i64;
    let total = pop.iter().filter(|r| r.1 == c0).count() as i64;
    assert!(
        total > prior,
        "the table holds post-cutoff launches of this creator"
    );
    let m = key(9_999, 0xEE);
    e.tick(launch(m, c0, CUTOFF));
    assert_eq!(
        e.model_creator_dev_history(&m).creator_past_launches,
        Some(prior)
    );
}

/// Restart parity: seed + feed through the real engine path, persist, reload; counts identical to
/// the no-restart run AND equal to the oracle (trained counts) for the causal prefix.
#[test]
fn restart_parity_with_the_causal_prefix() {
    let d = dir("parity");
    let pop = population();
    write_seed(&d.join("seed.jsonl"), &pop);
    let sp = spec(&d.join("seed.jsonl"), CUTOFF);
    let live: Vec<_> = pop.iter().filter(|r| r.2 >= CUTOFF).copied().collect();
    let half = live.len() / 2;

    // A: no restart.
    let (mut a, _) = armed();
    a.model_creator_registry_attach(Some(&sp), &d.join("a.jsonl"), CUTOFF, "a")
        .expect("attach a");
    for r in &live {
        a.tick(launch(r.0, r.1, r.2));
    }

    // B: restart half-way, with an overlap replay of the last 10 pre-restart launches.
    let (mut b1, _) = armed();
    b1.model_creator_registry_attach(Some(&sp), &d.join("b.jsonl"), CUTOFF, "b1")
        .expect("attach b1");
    for r in &live[..half] {
        b1.tick(launch(r.0, r.1, r.2));
    }
    drop(b1);
    let (mut b2, _) = armed();
    let st = b2
        .model_creator_registry_attach(Some(&sp), &d.join("b.jsonl"), CUTOFF, "b2")
        .expect("attach b2");
    assert_eq!(
        st.restored_from_log as usize, half,
        "every pre-restart launch restored"
    );
    for r in &live[half - 10..] {
        b2.tick(launch(r.0, r.1, r.2));
    }

    let want = oracle(&pop);
    let mut matched = 0;
    for (m, n) in &want {
        let da = a.model_creator_dev_history(m);
        let db = b2.model_creator_dev_history(m);
        assert_eq!(da, db, "restart changed a count");
        assert_eq!(
            da.creator_past_launches,
            Some(*n),
            "differs from trained count"
        );
        assert_eq!(da.creator_known, 1);
        matched += 1;
    }
    assert_eq!(matched, pop.len());
    assert_eq!(
        a.model_creator_registry_len(),
        b2.model_creator_registry_len()
    );
    // Overlap did not double-append: the log holds exactly the live launches once each.
    let lines = std::fs::read_to_string(d.join("b.jsonl"))
        .expect("log")
        .lines()
        .count();
    assert_eq!(lines, 1 + live.len(), "header + one record per live launch");
    // Feed-observed status survives the restart (cohort freeze predicate).
    assert!(live.iter().all(|r| b2.model_launch_feed_observed(&r.0)));
    assert!(pop
        .iter()
        .filter(|r| r.2 < CUTOFF)
        .all(|r| !b2.model_launch_feed_observed(&r.0)));
}

#[test]
fn a_replayed_launch_never_increments_twice() {
    let d = dir("dedupe");
    let (mut e, _) = armed();
    e.model_creator_registry_attach(None, &d.join("log.jsonl"), 0, "s")
        .expect("attach");
    let c = key(1, 0xC1);
    e.tick(launch(key(1, 0xA1), c, 1_000));
    e.tick(launch(key(1, 0xA1), c, 1_000)); // redelivery
    e.tick(launch(key(2, 0xA1), c, 2_000));
    e.tick(launch(key(1, 0xA1), c, 1_000)); // late overlap replay
    e.tick(launch(key(3, 0xA1), c, 3_000));
    assert_eq!(
        e.model_creator_dev_history(&key(3, 0xA1))
            .creator_past_launches,
        Some(2)
    );
    assert_eq!(e.model_creator_registry_len(), 3);
    let lines = std::fs::read_to_string(d.join("log.jsonl"))
        .expect("log")
        .lines()
        .count();
    assert_eq!(lines, 1 + 3, "a replay appends nothing");
    // Seed/live overlap: a seeded mint re-delivered by the feed does not count twice either.
    let d2 = dir("dedupe_seed");
    write_seed(
        &d2.join("seed.jsonl"),
        &[(key(1, 0xA1), c, 1_000), (key(2, 0xA1), c, 2_000)],
    );
    let (mut e2, _) = armed();
    e2.model_creator_registry_attach(
        Some(&spec(&d2.join("seed.jsonl"), 10_000)),
        &d2.join("log.jsonl"),
        10_000,
        "s",
    )
    .expect("attach");
    e2.tick(launch(key(2, 0xA1), c, 2_000)); // overlap: already seeded
    e2.tick(launch(key(3, 0xA1), c, 3_000));
    assert_eq!(
        e2.model_creator_dev_history(&key(3, 0xA1))
            .creator_past_launches,
        Some(2)
    );
    assert_eq!(e2.model_creator_registry_len(), 3);
}

#[test]
fn a_count_is_frozen_before_its_own_insert() {
    // The target launch never counts itself, and a same-millisecond twin is not prior.
    let d = dir("cbi");
    let (mut e, _) = armed();
    e.model_creator_registry_attach(None, &d.join("log.jsonl"), 0, "s")
        .expect("attach");
    let c = key(5, 0xC5);
    e.tick(launch(key(1, 0xA5), c, 1_000));
    assert_eq!(
        e.model_creator_dev_history(&key(1, 0xA5))
            .creator_past_launches,
        Some(0)
    );
    e.tick(launch(key(2, 0xA5), c, 1_000));
    assert_eq!(
        e.model_creator_dev_history(&key(2, 0xA5))
            .creator_past_launches,
        Some(0)
    );
    e.tick(launch(key(3, 0xA5), c, 1_001));
    assert_eq!(
        e.model_creator_dev_history(&key(3, 0xA5))
            .creator_past_launches,
        Some(2)
    );
}

#[test]
fn an_unreadable_or_incompatible_log_is_a_named_refusal_never_empty() {
    let d = dir("restore");
    let (mut e, _) = armed();
    e.model_creator_registry_attach(None, &d.join("log.jsonl"), 0, "s")
        .expect("attach");
    e.tick(launch(key(1, 0xA7), key(1, 0xC7), 1_000));
    drop(e);
    let p = d.join("log.jsonl");
    let good = std::fs::read_to_string(&p).expect("log");

    let cases: Vec<(String, &str)> = vec![
        (
            good.trim_end().to_string(),
            "creator_registry_log_torn_tail",
        ),
        (
            good.replace("\"schema\":1", "\"schema\":2"),
            "creator_registry_log_schema_incompatible",
        ),
        (
            good.replace("\"seed_cutoff_ms\":0", "\"seed_cutoff_ms\":5"),
            "creator_registry_log_seed_mismatch",
        ),
        (
            format!("{good}{{not json}}\n"),
            "creator_registry_log_record_malformed",
        ),
        (
            "garbage\n".to_string(),
            "creator_registry_log_header_malformed",
        ),
    ];
    for (body, want) in cases {
        std::fs::write(&p, &body).expect("write");
        let (mut e2, _) = armed();
        let r = e2.model_creator_registry_attach(None, &p, 0, "s");
        assert_eq!(
            r.as_ref().err().map(RegistryRefusal::as_str),
            Some(want),
            "{body}"
        );
        assert_eq!(e2.model_creator_registry_refusal(), Some(want));
        // The evidence on disk is left untouched.
        assert_eq!(std::fs::read_to_string(&p).expect("log"), body);
    }
    // A good log restores the launch.
    std::fs::write(&p, &good).expect("write");
    let (mut e3, _) = armed();
    let st = e3
        .model_creator_registry_attach(None, &p, 0, "s")
        .expect("ok");
    assert_eq!(st.restored_from_log, 1);
    assert!(e3.model_launch_feed_observed(&key(1, 0xA7)));
    assert_eq!(e3.model_creator_registry_refusal(), None);
}

#[test]
fn counts_above_the_trained_max_are_rendered_as_is_never_clamped() {
    let d = dir("big");
    let c = key(1, 0xCB);
    let rows: Vec<_> = (0..300u32)
        .map(|i| (key(i, 0xAB), c, 1_000 + i64::from(i)))
        .collect();
    write_seed(&d.join("seed.jsonl"), &rows);
    let (mut e, _) = armed();
    e.model_creator_registry_attach(
        Some(&spec(&d.join("seed.jsonl"), 1_000_000)),
        &d.join("log.jsonl"),
        1_000_000,
        "s",
    )
    .expect("attach");
    let m = key(77_777, 0xAB);
    e.tick(launch(m, c, 2_000_000));
    assert_eq!(
        e.model_creator_dev_history(&m).creator_past_launches,
        Some(300)
    );
    // ... and the RENDERED prompt carries 300 (> trained max 227) verbatim.
    let t = feed_market(&mut e, m, 2_000_000);
    let s = e.model_entry_snapshot(&m, t).expect("snapshot");
    assert!(
        s.user_prompt
            .contains("DEV HISTORY: creator_past_launches=300 creator_known=1\n"),
        "{}",
        s.user_prompt
    );
}

/// Feed `n` priced prints + a curve for `m` after its launch at `t0`; returns a decision clock.
fn feed_market(e: &mut Engine, m: [u8; 32], t0: i64) -> i64 {
    let n = 40u32;
    for i in 0..n {
        let buy = i % 3 != 0;
        let mut w = [0u8; 32];
        w[0] = (i % 200) as u8 + 1;
        w[31] = 1;
        e.tick(AppEvent::MarketTrade {
            mint: DomainMint::from_bytes(m),
            price_fp: 22_000 + i128::from(i),
            quote_lamports: 500_000_000 + u64::from(i),
            liquidity_lamports: 37_900_000_000,
            signed_base: if buy { 30_000_000_000 } else { -30_000_000_000 },
            buyer_entity: 1 + u64::from(i),
            age_slots: 30,
            recv_unix_ms: Some(t0 + 1_000 + i64::from(i) * 2_000),
            trader_pubkey: Some(w),
            slot: Some(1_000 + u64::from(i)),
            fee_lamports: Some(60_000 + u64::from(i) * 100),
            cu_consumed: Some(90_000 + u64::from(i)),
            venue: Some(TradeVenue::PumpFun),
            event_id: None,
            feature: None,
        });
    }
    let t_last = t0 + 1_000 + i64::from(n) * 2_000;
    e.tick(AppEvent::CurveObserved {
        mint: DomainMint::from_bytes(m),
        v_sol_lamports: 37_900_000_000,
        v_tokens: 849_000_000_000_000,
        real_sol_lamports: 7_900_000_000,
        real_tokens: 569_000_000_000_000,
        recv_unix_ms: Some(t_last),
        slot: 2_000,
    });
    t_last + 1_000
}

#[test]
fn decision_records_carry_registry_provenance_and_the_prompt_format_is_unchanged() {
    let d = dir("prov");
    let c = key(1, 0xCD);
    write_seed(
        &d.join("seed.jsonl"),
        &[(key(1, 0xAD), c, 1_000), (key(2, 0xAD), c, 2_000)],
    );
    let (mut e, _) = armed();
    let st = e
        .model_creator_registry_attach(
            Some(&spec(&d.join("seed.jsonl"), 1_000_000)),
            &d.join("log.jsonl"),
            1_000_000,
            "s",
        )
        .expect("attach");
    let m = key(3, 0xAD);
    e.tick(launch(m, c, 2_000_000));
    let t = feed_market(&mut e, m, 2_000_000);
    let s = e.model_entry_snapshot(&m, t).expect("snapshot");
    assert!(s.creator_provenance.contains(PROVENANCE_LABEL));
    assert!(s.creator_provenance.contains(&st.provenance.seed_sha256));
    assert!(s.creator_provenance.contains("capture_v7_test"));
    assert!(s.creator_provenance.contains("cutoff_ms:1000000"));
    // The provenance is a RECORD field only: the prompt is the trained format, byte for byte.
    assert!(!s.user_prompt.contains("observed history"));
    assert!(!s.user_prompt.contains("creator_registry"));
    assert!(s
        .user_prompt
        .contains("DEV HISTORY: creator_past_launches=2 creator_known=1\n"));
    // Same market on an engine WITHOUT the registry attached: identical prompt bytes except the
    // count (the registry changes the number, never the format).
    let (mut plain, _) = armed();
    plain.tick(launch(m, c, 2_000_000));
    let t2 = feed_market(&mut plain, m, 2_000_000);
    let s2 = plain.model_entry_snapshot(&m, t2).expect("snapshot");
    assert_eq!(
        s.user_prompt
            .replace("creator_past_launches=2", "creator_past_launches=0"),
        s2.user_prompt
    );
}

#[test]
fn cohort_freeze_seed_only_markets_refuse_entry_by_name() {
    let d = dir("cohort");
    let c = key(1, 0xCE);
    let seeded = key(1, 0xAE);
    write_seed(&d.join("seed.jsonl"), &[(seeded, c, 1_000)]);
    let (mut e, calls) = armed();
    e.model_creator_registry_attach(
        Some(&spec(&d.join("seed.jsonl"), 1_000_000)),
        &d.join("log.jsonl"),
        1_000_000,
        "s",
    )
    .expect("attach");
    // A market whose launch our feed never saw (only seeded) -> named refusal.
    let t = feed_market(&mut e, seeded, 1_000_000);
    let r = e.model_entry_snapshot(&seeded, t).expect_err("refused");
    assert_eq!(r.as_str(), "join_launch_seed_only_not_feed_observed");
    // An unseen market (no launch anywhere) -> the existing name.
    let other = key(2, 0xAE);
    let t2 = feed_market(&mut e, other, 1_000_000);
    assert_eq!(
        e.model_entry_snapshot(&other, t2)
            .expect_err("refused")
            .as_str(),
        "join_launch_unknown"
    );
    // Per-reason counters exist on the engine path.
    for _ in 0..4 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(10));
    }
    let rep = e.model_lane_report();
    assert!(
        rep.keys()
            .any(|k| k.starts_with("refuse:join_launch_seed_only_not_feed_observed")),
        "{rep:?}"
    );
    assert!(
        rep.keys()
            .any(|k| k.starts_with("refuse:join_launch_unknown")),
        "{rep:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no entry was asked");
}

#[test]
fn an_untrusted_registry_refuses_entry_by_name() {
    let d = dir("untrusted");
    std::fs::write(d.join("log.jsonl"), "garbage\n").expect("w");
    let (mut e, calls) = armed();
    assert!(e
        .model_creator_registry_attach(None, &d.join("log.jsonl"), 0, "s")
        .is_err());
    let m = key(1, 0xAF);
    e.tick(launch(m, key(1, 0xCF), 1_000));
    let t = feed_market(&mut e, m, 1_000);
    assert_eq!(
        e.model_entry_snapshot(&m, t).expect_err("refused").as_str(),
        "join_creator_registry_untrusted"
    );
    for _ in 0..4 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
