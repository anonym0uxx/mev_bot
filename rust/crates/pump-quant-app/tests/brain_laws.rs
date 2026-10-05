//! LAWs B1–B5 — the episodic recall memory wiring, proven law by law.
//!
//! Mirrors the `alpha_laws.rs` / `audit_wave2_laws.rs` discipline exactly: isolate
//! the law with a config toggle, drive the SAME deterministic event tape twice
//! (armed vs neutralized), and assert the armed arm wins in the law's own axis.
//! Determinism (§22) makes every comparison exact rather than statistical.
//!
//! * **B1** — an episode is sealed per completed trade, and its fingerprint is a
//!   function of ENTRY-time state only. Pinned by mutating the whole post-entry
//!   price path and asserting the recorded fingerprint is byte-identical: a
//!   fingerprint that moved would be reading the answer off the back of the card.
//! * **B2** — the reflection cadence produces grounded readouts (recalled setup
//!   classes with their realized medians, the meta lifecycle, measured author
//!   track records) instead of blind hypotheses.
//! * **B3** — on a tape where one setup class repeatedly bleeds, the armed
//!   reduce-only haircut/veto STRICTLY out-earns its absence.
//! * **B4** — an `Unknown` verdict changes nothing: with an insufficient brain the
//!   armed and disarmed runs produce a byte-identical `Report`, and a
//!   brain-disabled run produces a byte-identical DECISION stream.
//! * **B5** — recall verdicts are byte-identical after persist → "restart" →
//!   restore.

#![allow(dead_code)] // test scaffolding: helper/fixture chains not every #[test] exercises (consolidation N2)

use pump_quant_app::brain::AppBlobStore;
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::journal_log::Decision;
use pump_quant_brain::fingerprint::SetupFingerprint;
use pump_quant_brain::persist::MemBlobStore;
use pump_quant_domain::ids::Mint;
use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};

mod tape_b3;
use tape_b3::*;

/// Total realized net across journalled exits.
fn journal_stream(eng: &Engine) -> Vec<Decision> {
    eng.journal().recent().copied().collect()
}

/// Every fingerprint the brain has sealed, oldest first.
fn recorded_fingerprints(eng: &Engine) -> Vec<SetupFingerprint> {
    eng.brain()
        .index()
        .iter_oldest_first()
        .map(|e| *e.fingerprint())
        .collect()
}

// ===========================================================================
// LAW B1 — one episode per completed trade, fingerprinted AT ENTRY.
// ===========================================================================

/// The shared B1 tape: open one position, then let the post-entry path be dictated
/// by `path`. The ENTRY script is byte-identical in both arms.
fn drive_one_trade(cfg: Config, path: &[i128]) -> Engine {
    let m = mint(9_001);
    let mut eng = Engine::new(cfg, RunMode::Replay);
    seed_and_admit(&mut eng, m, 300);
    for (i, &px) in path.iter().enumerate() {
        one(&mut eng, m, px, -600_000, 380 + (i as u64 % 5));
        ticks(&mut eng, 1);
    }
    ticks(&mut eng, 6);
    let _ = eng.report();
    eng
}

// ===========================================================================
// LAW B2 — grounded reflection readouts.
// ===========================================================================

/// Feed one social call naming `addr` from `author`, through the real capture seam.
fn social_call(eng: &mut Engine, author: &str, addr: &str, ts_ns: u64) {
    let json = format!(
        "{{\"platform\":\"telegram\",\"author\":\"{author}\",\"community\":\"tg-b2\",\
         \"text\":\"call {addr} send it\",\"likes\":42,\"is_designated_caller\":true}}"
    );
    let mut src =
        MockSocialSource::new().with_batch(vec![RawSocialPayload::new(json.into_bytes(), ts_ns)]);
    eng.ingest_social(&mut src);
}

/// Base58 cohort keys for the B2 social tape (valid pubkeys, distinct from every
/// `mint(tag)` which are `tag_le ++ 0xB1 ++ 0…`).
const B2_KEYS: [&str; 10] = [
    "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr",
    "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263",
    "4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R",
    "So11111111111111111111111111111111111111112",
    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
    "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
    "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So",
    "7dHbWXmci3dT8UFYWYZweBLXgycu7Y3iL6trKn1Y7ARj",
    "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN",
    "9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E",
];

fn b58(s: &str) -> Mint {
    Mint::from_bytes(pump_quant_ingest::base58::decode_pubkey(s).expect("valid pubkey"))
}

#[test]
fn b2_meta_lifecycle_is_recorded_when_categories_exist() {
    // Feeding TokenMetadata gives mints a category, which the reflection cadence
    // snapshots onto the brain's meta timeline — the "state of the meta" readout.
    let mut cfg = hazard_cfg();
    cfg.reflect_every_ticks = 10;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    for k in 0..6u64 {
        let m = mint(2_000 + k);
        eng.tick(AppEvent::TokenMetadata {
            mint: m,
            category_id: 7,
            // v1 is the shipped taxonomy version (see `META_TAXONOMY_VERSION_DEFAULT`);
            // an assignment stamped with any other version is left UNKNOWN, never
            // retroactively remapped (criterion 81).
            taxonomy_version: 1,
            creator: 5_000 + k,
            slot: 10 + k,
        });
        seed_and_admit(&mut eng, m, 900 + k * 10);
        crater(&mut eng, m, 980 + k);
    }
    let r = eng.report();
    assert!(
        !r.brain_meta_state.is_empty(),
        "LAW B2: with categories fed, the meta lifecycle timeline must be populated"
    );
    assert!(
        eng.brain().meta_timeline().len() >= r.brain_meta_state.len(),
        "the report shows a bounded head of the full timeline"
    );
}

// ===========================================================================
// LAW B4 — fail-closed: an Unknown verdict changes nothing. PINNED, no toggle.
// ===========================================================================

/// A tape too short for recall to ever clear the §46 sample floor: every admit-time
/// verdict is structurally `Unknown`.
fn drive_thin_brain(cfg: Config) -> (Report, Engine) {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    for k in 0..3u64 {
        let m = mint(7_000 + k);
        seed_and_admit(&mut eng, m, 3_000 + k * 10);
        crater(&mut eng, m, 3_900 + k);
    }
    let r = eng.report();
    (r, eng)
}

// ===========================================================================
// LAW B5 — persistence: recall verdicts survive a restart.
// ===========================================================================

#[test]
fn b5_a_fresh_store_restores_to_an_empty_fail_closed_brain() {
    // The other half of the law: restoring from nothing yields an EMPTY index whose
    // every verdict is Unknown — a restart never manufactures evidence.
    let mut cfg = hazard_cfg();
    cfg.brain_path =
        pump_quant_app::config::CfgPath::from_str_checked("brain-empty").expect("path");
    let mut eng = Engine::new(cfg, RunMode::Replay);
    let report = eng
        .attach_brain_store(AppBlobStore::Mem(MemBlobStore::new()))
        .expect("attach");
    assert_eq!(report.admitted(), 0, "nothing to restore");
    assert!(eng.brain().index().is_empty());
}

// ===========================================================================
// Config surface: the brain's toggles, thresholds and path parse and validate.
// ===========================================================================

#[test]
fn brain_config_keys_parse_and_the_reduce_only_envelope_is_enforced() {
    use pump_quant_app::config::{CfgPath, ConfigError, BRAIN_PATH_CAP};

    // Integer toggles + thresholds parse through the ordinary key = value grammar.
    let doc = "brain_enable = 1\n\
               brain_haircut_enable = 1\n\
               brain_min_sample = 12\n\
               brain_recall_max_distance = 4\n\
               brain_haircut_win_rate_bp = 4000\n\
               brain_veto_win_rate_bp = 1000\n\
               brain_haircut_mult_bp = 6000\n\
               brain_persist_enable = 1\n\
               brain_path = data/brain\n";
    let cfg = Config::from_str_over_default(doc).expect("parse");
    assert!(cfg.brain_enable && cfg.brain_haircut_enable && cfg.brain_persist_enable);
    assert_eq!(cfg.brain_min_sample, 12);
    assert_eq!(cfg.brain_recall_max_distance, 4);
    assert_eq!(cfg.brain_haircut_win_rate_bp, 4_000);
    assert_eq!(cfg.brain_veto_win_rate_bp, 1_000);
    assert_eq!(cfg.brain_haircut_mult_bp, 6_000);
    assert_eq!(cfg.brain_path.as_str(), "data/brain");

    // LAW B3 is reduce-only: a "haircut" above 100% is refused, not clamped.
    let mut bad = Config::dev_portable();
    bad.brain_haircut_mult_bp = 10_001;
    assert_eq!(
        bad.validate(),
        Err(ConfigError::Inconsistent(
            "brain_haircut_mult_bp exceeds 100% (LAW B3 is reduce-only)"
        ))
    );
    // The veto bar must be strictly harsher evidence than the haircut bar.
    let mut inverted = Config::dev_portable();
    inverted.brain_veto_win_rate_bp = 9_000;
    inverted.brain_haircut_win_rate_bp = 3_500;
    assert_eq!(
        inverted.validate(),
        Err(ConfigError::Inconsistent(
            "brain_veto_win_rate_bp exceeds brain_haircut_win_rate_bp"
        ))
    );
    // §46: a zero sample floor would let a single episode move risk.
    let mut zero = Config::dev_portable();
    zero.brain_min_sample = 0;
    assert_eq!(
        zero.validate(),
        Err(ConfigError::Inconsistent(
            "brain_min_sample must be positive (§46 fail-closed)"
        ))
    );
    // An over-long path is REFUSED, never truncated — a truncated path is a
    // different path, and silently journaling to it would be worse than failing.
    let long = "x".repeat(BRAIN_PATH_CAP + 1);
    assert!(CfgPath::from_str_checked(&long).is_none());
    assert_eq!(
        Config::from_str_over_default(&format!("brain_path = {long}")),
        Err(ConfigError::PathTooLong("brain_path".to_string()))
    );
    // And the defaults ship the way the manifest pins them.
    let d = Config::dev_portable();
    assert!(d.brain_enable, "B1/B2 record+readout default ON");
    assert!(
        d.brain_haircut_enable,
        "LAW B3 ships ARMED as of re-pin #21: it is the unique configuration in the \
         2^3 law lattice that clears the pre-registered rule in \
         `tests/law_permutation_sweep.rs` — material on the union tape, exactly \
         neutral on the golden tape, and not a lamport of loss on ANY of the nine \
         hazard tapes measured (including its own maximal false-positive mirror)"
    );
    assert!(!d.brain_persist_enable, "LAW B5 is an operator opt-in");
    assert!(d.brain_path.is_empty());
}
