//! REGRESSION CLASS 2 — law-presence invariants.
//!
//! For every newly wired law toggle:
//!   (a) its `Config` field / `apply()` key EXISTS and defaults to the pinned
//!       value (a silent default flip is itself a regression), and
//!   (b) flipping it changes ≥ 1 audited output.
//!
//! The journal digest is seeded with `fnv1a_64(format!("{cfg:?}"))` — the whole
//! `Config` participates in the §19 strategy identity — so flipping ANY law
//! toggle must move the golden digest. That proves the law is still part of the
//! strategy identity and cannot be silently dropped from it. A curated subset
//! then proves a DECISION-level effect (net, admitted, reject codes), so a law
//! that is wired only into the seed and nowhere else is also caught.
//!
//! Determinism (§22) makes every A/B exact, not statistical.

use pq_regression::baselines::*;
use pq_regression::golden_tape::{drive, mint};
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::{AppEvent, CreatorActionKind};
use pump_quant_app::journal_log::Decision;

// ---------------------------------------------------------------------------
// (a) Pinned defaults — one place, driven by the manifest table.
// ---------------------------------------------------------------------------

/// The `dev_portable` value of a boolean law key (also proves the key names a
/// real field). `None` for an unknown key.
fn bool_field(cfg: &Config, key: &str) -> Option<bool> {
    Some(match key {
        "creator_dump_veto_enable" => cfg.creator_dump_veto_enable,
        "derived_targets_enable" => cfg.derived_targets_enable,
        "into_strength_exit_enable" => cfg.into_strength_exit_enable,
        "vol_stop_enable" => cfg.vol_stop_enable,
        "setup_classifier_enable" => cfg.setup_classifier_enable,
        "entry_mode_leaves_enable" => cfg.entry_mode_leaves_enable,
        "money_proxy_enable" => cfg.money_proxy_enable,
        "narrative_class_enable" => cfg.narrative_class_enable,
        "platform_lead_enable" => cfg.platform_lead_enable,
        "deployer_screen_enable" => cfg.deployer_screen_enable,
        "fee_floor_enable" => cfg.fee_floor_enable,
        "probe_budget_enable" => cfg.probe_budget_enable,
        "alpha_call_lane_enable" => cfg.alpha_call_lane_enable,
        "designated_caller_enable" => cfg.designated_caller_enable,
        "alpha_exit_pressure_enable" => cfg.alpha_exit_pressure_enable,
        "brain_enable" => cfg.brain_enable,
        "brain_haircut_enable" => cfg.brain_haircut_enable,
        "brain_persist_enable" => cfg.brain_persist_enable,
        "brain_analysis_enable" => cfg.brain_analysis_enable,
        "brain_reflect_enable" => cfg.brain_reflect_enable,
        _ => return None,
    })
}

/// The `dev_portable` value of an integer law key.
fn int_field(cfg: &Config, key: &str) -> Option<i64> {
    Some(match key {
        "universe_age_exempt_slots" => i64::from(cfg.universe_age_exempt_slots),
        "bar_trades_per_bar" => cfg.bar_trades_per_bar as i64,
        "structure_downtrend_haircut_bp" => i64::from(cfg.structure_downtrend_haircut_bp),
        "narrative_decay_bp" => i64::from(cfg.narrative_decay_bp),
        "narrative_decay_floor" => cfg.narrative_decay_floor as i64,
        "promote_corroboration_quota" => cfg.promote_corroboration_quota as i64,
        "meta_taxonomy_version" => i64::from(cfg.meta_taxonomy_version),
        "brain_min_sample" => i64::from(cfg.brain_min_sample),
        "brain_recall_max_distance" => i64::from(cfg.brain_recall_max_distance),
        "brain_haircut_win_rate_bp" => i64::from(cfg.brain_haircut_win_rate_bp),
        "brain_veto_win_rate_bp" => i64::from(cfg.brain_veto_win_rate_bp),
        "brain_haircut_mult_bp" => i64::from(cfg.brain_haircut_mult_bp),
        _ => return None,
    })
}

#[test]
fn every_law_toggle_default_is_pinned() {
    let cfg = Config::dev_portable();
    for &(key, want) in LAW_BOOL_DEFAULTS {
        let got = bool_field(&cfg, key)
            .unwrap_or_else(|| panic!("law key '{key}' no longer names a Config field"));
        assert_eq!(got, want, "law default for '{key}' drifted");
    }
    for &(key, want) in LAW_INT_DEFAULTS {
        let got = int_field(&cfg, key)
            .unwrap_or_else(|| panic!("law key '{key}' no longer names a Config field"));
        assert_eq!(got, want, "law default for '{key}' drifted");
    }
}

// ---------------------------------------------------------------------------
// (b) Config-identity coverage: flipping any law toggle moves the golden digest.
// Each closure flips exactly one toggle (typed, so a renamed/removed field is a
// compile error). Running the golden tape must then produce a different digest.
// ---------------------------------------------------------------------------

/// (name, one-toggle mutation) for every law toggle. A typed mutation keeps this
/// honest: if a field is removed, this array stops compiling.
#[allow(clippy::type_complexity)]
fn law_toggle_flips() -> Vec<(&'static str, fn(&mut Config))> {
    vec![
        ("creator_dump_veto_enable", |c| {
            c.creator_dump_veto_enable = !c.creator_dump_veto_enable
        }),
        ("derived_targets_enable", |c| {
            c.derived_targets_enable = !c.derived_targets_enable
        }),
        ("into_strength_exit_enable", |c| {
            c.into_strength_exit_enable = !c.into_strength_exit_enable
        }),
        ("vol_stop_enable", |c| {
            c.vol_stop_enable = !c.vol_stop_enable
        }),
        ("setup_classifier_enable", |c| {
            c.setup_classifier_enable = !c.setup_classifier_enable
        }),
        ("entry_mode_leaves_enable", |c| {
            c.entry_mode_leaves_enable = !c.entry_mode_leaves_enable
        }),
        ("money_proxy_enable", |c| {
            c.money_proxy_enable = !c.money_proxy_enable
        }),
        ("narrative_class_enable", |c| {
            c.narrative_class_enable = !c.narrative_class_enable
        }),
        ("platform_lead_enable", |c| {
            c.platform_lead_enable = !c.platform_lead_enable
        }),
        ("deployer_screen_enable", |c| {
            c.deployer_screen_enable = !c.deployer_screen_enable
        }),
        ("fee_floor_enable", |c| {
            c.fee_floor_enable = !c.fee_floor_enable
        }),
        ("probe_budget_enable", |c| {
            c.probe_budget_enable = !c.probe_budget_enable
        }),
        ("alpha_call_lane_enable", |c| {
            c.alpha_call_lane_enable = !c.alpha_call_lane_enable
        }),
        ("designated_caller_enable", |c| {
            c.designated_caller_enable = !c.designated_caller_enable
        }),
        ("alpha_exit_pressure_enable", |c| {
            c.alpha_exit_pressure_enable = !c.alpha_exit_pressure_enable
        }),
        ("universe_age_exempt_slots", |c| {
            c.universe_age_exempt_slots = c.universe_age_exempt_slots.wrapping_add(1)
        }),
        ("bar_trades_per_bar", |c| {
            c.bar_trades_per_bar = c.bar_trades_per_bar.wrapping_add(1)
        }),
        ("structure_downtrend_haircut_bp", |c| {
            c.structure_downtrend_haircut_bp -= 1
        }),
        ("narrative_decay_bp", |c| c.narrative_decay_bp -= 1),
        ("narrative_decay_floor", |c| {
            c.narrative_decay_floor = c.narrative_decay_floor.wrapping_add(1)
        }),
        ("promote_corroboration_quota", |c| {
            c.promote_corroboration_quota = 0
        }),
        // LAWs B6/B7 (brain -> strategy analysis). Both are report-plane or
        // default-OFF, so their DECISION effect is nil by design; they must still
        // live inside the §19 config-identity seed, which is exactly what this
        // test proves.
        ("brain_analysis_enable", |c| {
            c.brain_analysis_enable = !c.brain_analysis_enable
        }),
        ("brain_reflect_enable", |c| {
            c.brain_reflect_enable = !c.brain_reflect_enable
        }),
    ]
}

#[test]
fn every_law_toggle_is_in_the_strategy_identity_seed() {
    for (name, flip) in law_toggle_flips() {
        let mut cfg = Config::dev_portable();
        flip(&mut cfg);
        let r = drive(cfg);
        assert_ne!(
            r.journal_digest, GOLDEN_DIGEST,
            "flipping '{name}' did not change the golden digest — the law dropped \
             out of the §19 config-identity seed (dead-coded?)"
        );
    }
}

// ---------------------------------------------------------------------------
// (b') Decision-level wiring for the golden-exercised laws.
// ---------------------------------------------------------------------------

#[test]
fn derived_targets_reversal_moves_the_golden_net_off_the_fixed_ladder() {
    // §24 reversal is DEFAULT ON, so the baseline golden net is cost-derived.
    let derived = drive(Config::dev_portable());
    assert_eq!(derived.net_lamports, GOLDEN_NET_LAMPORTS);

    // Turning the derived ladder OFF falls back to the forbidden fixed ladder —
    // a genuine DECISION change with a pinned net, not just a seed change.
    let mut fixed_cfg = Config::dev_portable();
    fixed_cfg.derived_targets_enable = false;
    let fixed = drive(fixed_cfg);
    assert_eq!(
        fixed.net_lamports, GOLDEN_NET_FIXED_LADDER,
        "the fixed-ladder net drifted — §24 LAW 2 wiring changed"
    );
    assert!(
        derived.net_lamports > fixed.net_lamports,
        "on the representative tape cost-derived must out-earn fixed ({} vs {})",
        derived.net_lamports,
        fixed.net_lamports
    );
}

#[test]
fn corroboration_quota_changes_golden_admissions_and_net() {
    let with_quota = drive(Config::dev_portable());
    let mut no_quota = Config::dev_portable();
    no_quota.promote_corroboration_quota = 0; // §71 union-preservation off
    let without = drive(no_quota);
    assert!(
        with_quota.net_lamports > without.net_lamports,
        "the §71 quota must strictly out-earn its absence ({} vs {})",
        with_quota.net_lamports,
        without.net_lamports
    );
    assert_ne!(
        with_quota.admitted, without.admitted,
        "the §71 quota must change which candidates are admitted"
    );
}

#[test]
fn universe_screen_toggle_changes_the_filtered_count() {
    let armed = drive(Config::dev_portable());
    assert_eq!(armed.universe_filtered, GOLDEN_UNIVERSE_FILTERED);
    let mut exempt = Config::dev_portable();
    exempt.universe_age_exempt_slots = u32::MAX; // every market age-exempt ⇒ screen off
    let neutral = drive(exempt);
    assert_ne!(
        neutral.universe_filtered, armed.universe_filtered,
        "neutralizing the §21.5 age screen must change the filtered count"
    );
}

// ---------------------------------------------------------------------------
// (b'') Decision-level wiring for two protective vetoes NOT exercised by the
// golden tape (it emits no CreatorAction / bundle footprint). Compact hazard
// tapes mirror the audit_wave2 structure and prove the veto still fires.
// ---------------------------------------------------------------------------

const HAZ: u64 = 9_500;
const PRICE_SCALE: i128 = 10_000_000;

fn reject_count(eng: &Engine, tag: u64, reason: u8) -> usize {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB;
    eng.journal()
        .recent()
        .filter(
            |d| matches!(**d, Decision::Rejected { mint, reason: r } if mint == b && r == reason),
        )
        .count()
}

fn pump(eng: &mut Engine, tag: u64, base_mult: i128, n: u64) {
    for i in 0..n {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(tag),
            price_fp: (base_mult + i as i128) * PRICE_SCALE,
            quote_lamports: 800_000,
            liquidity_lamports: 400_000_000,
            signed_base: 900_000 - (i as i64),
            buyer_entity: 40 + i % 7,
            age_slots: 12,
        });
    }
}

/// §26 pre-entry veto: a deployer that has already distributed past the veto
/// threshold before entry must be refused (reject code 13).
fn drive_preentry_dump(cfg: Config) -> (Report, Engine) {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    eng.tick(AppEvent::CreatorAction {
        mint: mint(HAZ),
        kind: CreatorActionKind::Init {
            initial_tokens: 1_000_000_000,
            total_supply: 1_000_000_000,
        },
        slot: 1,
    });
    eng.tick(AppEvent::CreatorAction {
        mint: mint(HAZ),
        kind: CreatorActionKind::Sell {
            tokens: 700_000_000,
            quote_lamports: 500_000_000,
        },
        slot: 2,
    });
    pump(&mut eng, HAZ, 100, 24);
    eng.tick(AppEvent::OnchainConfirm {
        mint: mint(HAZ),
        sellable_depth_lamports: 500_000_000,
    });
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    for i in 0..20u64 {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(HAZ),
            price_fp: (123 - i as i128 * 4) * PRICE_SCALE,
            quote_lamports: 800_000,
            liquidity_lamports: 400_000_000,
            signed_base: -900_000,
            buyer_entity: 40 + i % 7,
            age_slots: 12,
        });
    }
    for _ in 0..6 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    (r, eng)
}

#[test]
fn creator_dump_veto_toggle_gates_a_real_reject() {
    let (armed, aeng) = drive_preentry_dump(Config::dev_portable());
    let mut ncfg = Config::dev_portable();
    ncfg.creator_dump_veto_enable = false;
    let (neut, _neng) = drive_preentry_dump(ncfg);

    assert!(
        reject_count(&aeng, HAZ, 13) > 0,
        "the §26 pre-entry veto must fire (reject 13) on a confirmed dump"
    );
    assert_eq!(armed.admitted, 0, "armed must refuse the dumping market");
    assert!(neut.admitted > 0, "neutral must admit the dumping market");
}

const MF: u64 = 8_400;

/// §70.10 fee-floor veto: a fully-saturated bundle/wash first-slot footprint must
/// be refused pre-entry (reject code 14) when the law is armed.
fn drive_fee_floor(cfg: Config) -> (Report, Engine) {
    use pump_quant_app::event::AppEvent as E;
    use pump_quant_signals::launch_trajectory::FirstSlotTx;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    let mut txs: Vec<FirstSlotTx> = (0..19)
        .map(|_| FirstSlotTx {
            tipper_entity: 0,
            priority_fee_lamports: 0,
            tip_lamports: 0,
            is_bundle: true,
            is_known_sniper: false,
        })
        .collect();
    txs.push(FirstSlotTx {
        tipper_entity: 0,
        priority_fee_lamports: 1,
        tip_lamports: 0,
        is_bundle: true,
        is_known_sniper: false,
    });
    eng.observe_first_slot_fees(mint(MF).as_bytes(), &txs);
    for i in 0..24u64 {
        eng.tick(E::MarketTrade {
            mint: mint(MF),
            price_fp: (100 + i as i128) * PRICE_SCALE,
            quote_lamports: 800_000,
            liquidity_lamports: 400_000_000,
            signed_base: 900_000 - (i as i64),
            buyer_entity: 40 + i % 7,
            age_slots: 12,
        });
    }
    eng.tick(E::OnchainConfirm {
        mint: mint(MF),
        sellable_depth_lamports: 500_000_000,
    });
    for _ in 0..6 {
        eng.tick(E::Tick);
    }
    let r = eng.report();
    (r, eng)
}

#[test]
fn fee_floor_veto_toggle_gates_a_real_reject() {
    let mut acfg = Config::dev_portable();
    acfg.fee_floor_enable = true;
    let (armed, aeng) = drive_fee_floor(acfg);
    let (neut, _neng) = drive_fee_floor(Config::dev_portable()); // default OFF

    assert!(
        reject_count(&aeng, MF, 14) > 0,
        "the §70.10 fee-floor veto must fire (reject 14) on a bundle footprint"
    );
    assert_eq!(armed.admitted, 0, "armed must refuse the bundle launch");
    assert!(neut.admitted > 0, "neutral (law off) must admit the market");
}
