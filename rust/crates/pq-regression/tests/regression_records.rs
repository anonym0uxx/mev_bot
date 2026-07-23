//! REGRESSION CLASS 2 — record / report-plane law presence (engine-driven).
//!
//! Laws whose evidence is a JOURNAL RECORD or an ANALYTICS aggregate, driven over
//! a small, fast, deterministic position tape (§22). Each asserts the record /
//! aggregate the law is responsible for is still populated in its mandated shape:
//!
//!   * §34.4 DecisionRecord completeness — the Admitted record carries the
//!     well-ordered size band (x_min ≤ x_cost ≤ x_max) with the size inside it,
//!     plus the fail-rate and round-trip-cost provenance.
//!   * §49 non-degenerate convexity — a veto/haircut records counterfactual != realized.
//!   * §47/§54 post-exit markouts — markout cells + foregone-upside are present per exit.
//!   * §47a terminal-state labels — a silent mint is labeled dead at the versioned δT.
//!   * §25 setup-archetype classifier — ON tags ≥1 non-stub archetype; OFF is all-0 stub.
//!   * §71.2 discovery-lane attribution — independent lanes carry distinct realized net.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::journal_log::Decision;
use pump_quant_domain::ids::Mint;

const PRICE_SCALE: i128 = 10_000_000;

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB;
    Mint::from_bytes(b)
}

fn pump(eng: &mut Engine, tag: u64, base_mult: i128, n: u64, liq: u64) {
    for i in 0..n {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(tag),
            price_fp: (base_mult + i as i128) * PRICE_SCALE,
            quote_lamports: 800_000,
            liquidity_lamports: liq,
            signed_base: 900_000 - (i as i64),
            buyer_entity: 40 + i % 7,
            age_slots: 12,
        });
    }
}

/// A small multi-mint tape that opens positions, then craters them so held
/// positions actually exit (populating exits / markouts / convexity).
fn drive_positions(cfg: Config) -> Engine {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    for round in 0..3u64 {
        for m in 0..6u64 {
            pump(&mut eng, m, 100 + round as i128 * 20, 24, 400_000_000);
            eng.tick(AppEvent::OnchainConfirm {
                mint: mint(m),
                sellable_depth_lamports: 500_000_000,
            });
        }
        for _ in 0..40 {
            eng.tick(AppEvent::Tick);
        }
        for m in 0..6u64 {
            for i in 0..12u64 {
                eng.tick(AppEvent::MarketTrade {
                    mint: mint(m),
                    price_fp: (150 - i as i128 * 6) * PRICE_SCALE,
                    quote_lamports: 800_000,
                    liquidity_lamports: 400_000_000,
                    signed_base: -900_000,
                    buyer_entity: 40 + i % 7,
                    age_slots: 12,
                });
            }
        }
        for _ in 0..40 {
            eng.tick(AppEvent::Tick);
        }
    }
    eng
}

#[test]
fn admitted_record_carries_well_ordered_band_and_provenance() {
    let cfg = Config::dev_portable();
    let fail_rate = cfg.gate_fail_rate_bps;
    let mut eng = drive_positions(cfg);
    let _ = eng.report();
    let admits: Vec<_> = eng
        .journal()
        .recent()
        .filter_map(|d| match *d {
            Decision::Admitted {
                size_lamports,
                x_min,
                x_cost,
                x_max,
                fail_rate_bps,
                rt_cost_bps,
                ..
            } => Some((
                size_lamports,
                x_min,
                x_cost,
                x_max,
                fail_rate_bps,
                rt_cost_bps,
            )),
            _ => None,
        })
        .collect();
    assert!(
        !admits.is_empty(),
        "the tape must open at least one position"
    );
    for (size, x_min, x_cost, x_max, fr, rt) in admits {
        assert!(
            x_min <= x_cost && x_cost <= x_max,
            "size band must be ordered"
        );
        assert!(
            size >= x_min && size <= x_max,
            "admitted size must lie within the band"
        );
        assert_eq!(fr, fail_rate, "fail-rate provenance must be recorded");
        assert!(
            rt > 0,
            "a real round-trip cost must be recorded at the admitted size"
        );
    }
}

#[test]
fn vetoes_and_haircuts_record_nondegenerate_convexity() {
    let mut eng = drive_positions(Config::dev_portable());
    let _ = eng.report();
    let rules = eng.analytics_report().convexity_rules;
    assert!(!rules.is_empty(), "the convexity ledger must be populated");
    assert!(
        rules
            .iter()
            .any(|r| r.suppressed_n > 0 && r.net_convexity_bps() != 0),
        "a veto/haircut must record a non-degenerate (counterfactual != realized) event"
    );
}

#[test]
fn post_exit_markouts_and_foregone_upside_present() {
    let mut eng = drive_positions(Config::dev_portable());
    let _ = eng.report();
    let a = eng.analytics_report();
    assert!(
        !a.markout_cells.is_empty(),
        "post-exit markout cells must be present"
    );
    assert!(
        !a.foregone_upside.is_empty(),
        "foregone-upside aggregates must be present"
    );
    let horizons: std::collections::BTreeSet<u64> =
        a.markout_cells.iter().map(|c| c.horizon_ns).collect();
    assert!(
        !horizons.is_empty(),
        "markout cells must carry mandated ns horizons"
    );
}

#[test]
fn dead_mint_gets_terminal_label_at_versioned_delta_t() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    pump(&mut eng, 7, 100, 6, 400_000_000);
    for _ in 0..300 {
        eng.tick(AppEvent::Tick);
    }
    let refs = eng.terminal_reflections();
    assert!(
        !refs.is_empty(),
        "the reflection cadence must produce terminal labels"
    );
    assert!(
        refs.iter().all(|r| r.criterion_version == 1),
        "labels must be stamped with the δT criterion version"
    );
    assert!(
        refs.iter().any(|r| r.is_dead()),
        "a mint silent past δT must be labeled terminal"
    );
}

// ---------------------------------------------------------------------------
// §25 setup-archetype classifier (config-gated).
// ---------------------------------------------------------------------------

fn bar8(eng: &mut Engine, tag: u64, prices: [i128; 8], entity0: u64) {
    for (i, &p) in prices.iter().enumerate() {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(tag),
            price_fp: p * PRICE_SCALE,
            quote_lamports: 800_000,
            liquidity_lamports: 400_000_000,
            signed_base: 900_000,
            buyer_entity: entity0 + i as u64 % 7,
            age_slots: 12,
        });
    }
}

fn drive_classifier(cfg: Config) -> (Report, Vec<u16>) {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    let (a, b) = (8_100u64, 8_200u64);
    bar8(&mut eng, a, [100, 104, 108, 112, 110, 108, 113, 115], 40);
    bar8(&mut eng, a, [116, 118, 121, 124, 120, 122, 124, 125], 41);
    bar8(&mut eng, a, [125, 118, 112, 110, 115, 122, 128, 130], 42);
    bar8(&mut eng, b, [100, 104, 108, 112, 110, 108, 113, 115], 50);
    bar8(&mut eng, b, [116, 118, 121, 124, 120, 122, 124, 125], 51);
    bar8(&mut eng, b, [125, 110, 95, 90, 100, 115, 125, 130], 52);
    for tag in [a, b] {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(tag),
            price_fp: 131 * PRICE_SCALE,
            quote_lamports: 800_000,
            liquidity_lamports: 400_000_000,
            signed_base: 900_000,
            buyer_entity: 44,
            age_slots: 12,
        });
        eng.tick(AppEvent::OnchainConfirm {
            mint: mint(tag),
            sellable_depth_lamports: 500_000_000,
        });
    }
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    let arch = eng.analytics_report().archetypes;
    (r, arch)
}

#[test]
fn setup_classifier_tags_nonzero_archetype_vs_all_zero_stub() {
    let (armed, armed_arch) = drive_classifier(Config::dev_portable());
    let mut ncfg = Config::dev_portable();
    ncfg.setup_classifier_enable = false;
    let (neut, neut_arch) = drive_classifier(ncfg);

    assert!(
        armed.admitted >= 2 && neut.admitted >= 2,
        "both markets must open"
    );
    // OFF: every realized row is the all-0 stub.
    assert_eq!(
        neut_arch,
        vec![0],
        "classifier OFF ⇒ only the all-0 stub archetype"
    );
    // ON: at least one real, non-stub archetype id is tagged.
    assert!(
        armed_arch.iter().any(|&x| x != 0),
        "classifier ON ⇒ at least one non-stub archetype ({armed_arch:?})"
    );
}

// ---------------------------------------------------------------------------
// §71.2 discovery-lane attribution — independent lanes carry distinct net.
// ---------------------------------------------------------------------------

fn sell_flow(eng: &mut Engine, tag: u64, base: i128, n: u64) {
    for i in 0..n {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(tag),
            price_fp: (base - i as i128) * PRICE_SCALE,
            quote_lamports: 800_000,
            liquidity_lamports: 400_000_000,
            signed_base: -500_000,
            buyer_entity: 60 + i % 7,
            age_slots: 12,
        });
    }
}

#[test]
fn discovery_lane_attribution_keeps_lanes_distinct() {
    use pump_quant_watchlist::candidate::{DiscoveryLane, Lane as WlLane};

    let ma = 7_100u64; // on-chain creation sighting
    let mb = 7_200u64; // social caller
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    eng.tick(AppEvent::TokenMetadata {
        mint: mint(ma),
        category_id: 1,
        taxonomy_version: 1,
        creator: 11,
        slot: 1,
    });
    for _ in 0..4 {
        eng.tick(AppEvent::SocialCall {
            mint: mint(mb),
            source_quality_bp: 3_000,
        });
    }
    sell_flow(&mut eng, ma, 130, 12);
    sell_flow(&mut eng, mb, 130, 12);
    for tag in [ma, mb] {
        eng.tick(AppEvent::OnchainConfirm {
            mint: mint(tag),
            sellable_depth_lamports: 500_000_000,
        });
    }
    eng.tick(AppEvent::TokenMetadata {
        mint: mint(ma),
        category_id: 1,
        taxonomy_version: 1,
        creator: 11,
        slot: 2,
    });
    for _ in 0..4 {
        eng.tick(AppEvent::SocialCall {
            mint: mint(mb),
            source_quality_bp: 3_000,
        });
    }
    for _ in 0..4 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    assert!(r.admitted >= 2, "both corroboration-lane markets must open");

    let disc_net = |lane: DiscoveryLane| -> i64 {
        r.per_discovery_lane_net
            .iter()
            .find(|(l, _)| *l == lane)
            .map(|(_, n)| *n)
            .expect("discovery lane must be present")
    };
    let onchain = disc_net(DiscoveryLane::OnchainCreation);
    let social = disc_net(DiscoveryLane::SocialCaller);
    assert!(
        onchain != 0,
        "the on-chain-creation lane must carry its own realized net"
    );
    assert!(
        social != 0,
        "the social-caller lane must carry its own realized net"
    );

    // The legacy setup-archetype ledger lumps BOTH into one CreationSniper slot —
    // §71.2 splits them: the setup total is the SUM of the two distinct lanes.
    let lumped = r
        .per_lane_net
        .iter()
        .find(|(l, _)| *l == WlLane::CreationSniper)
        .map(|(_, n)| *n)
        .expect("CreationSniper setup lane must be present");
    assert_eq!(
        lumped,
        onchain + social,
        "the two discovery lanes must partition the slot"
    );
    assert_ne!(
        onchain, lumped,
        "the split is real — neither lane equals the whole slot"
    );
    assert_ne!(
        social, lumped,
        "the split is real — neither lane equals the whole slot"
    );
}
