//! Pump-native intelligence laws: venue-native replies (the tier-3 capture
//! lane's exact NDJSON, explicit mint-grade thread reference) flow through the
//! EXISTING social architecture — corroboration-tier, deduplicated, fail-closed
//! — and PumpPortal create/migration events (previously dropped) reach the
//! canonical creation/migration paths through the existing token-ingest owner.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::token_ingest::to_token_metadata_v1;
use pump_quant_ingest::pumpportal_parse::{parse_pumpportal_create, parse_pumpportal_migration};
use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};

/// **DEPTH REALISM (re-pin #26).** The gate's price-impact model is now DERIVED from
/// the market's own SOL-side reserve (`cost_model::impact_den_for`), so a fixture's
/// declared depth is a decision input rather than decoration. Real pump.fun virtual
/// reserves START at 30 SOL; the sub-SOL depths these fixtures used to declare put the
/// operator's 0.1 SOL floor clip at 20-125% of the pool — a market in which no
/// strategy result means anything (Amendment A-13(1)).
const REAL_CURVE_VSOL: u64 = 30_000_000_000;

const MINT_B58: &str = "9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump";

fn pump_line(author: &str, text: &str) -> String {
    format!(
        "{{\"platform\":\"pump\",\"author\":\"{author}\",\"community\":\"{MINT_B58}\",\"text\":\"{text}\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false,\"mint\":\"{MINT_B58}\"}}"
    )
}

/// The thread-context mint resolves the observation at MINT GRADE even when
/// the reply text names no address at all — and pump replies alone surface the
/// coin for WATCHING (promoted) without ever authorizing entry.
#[test]
fn pump_replies_feed_candidates_but_never_authorize() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let mut batch = Vec::new();
    for i in 0..6u64 {
        batch.push(RawSocialPayload::new(
            pump_line(&format!("wallet{i}"), &format!("dev is based lfg {i}")).into_bytes(),
            1_000_000_000 + i * 1_000_000,
        ));
    }
    let mut src = MockSocialSource::new().with_batch(batch);
    let applied = eng.ingest_social(&mut src);
    assert_eq!(applied, 6, "every reply must resolve via the thread mint");
    // Give the coin an on-chain confirm but NO trade flow at all.
    eng.tick(AppEvent::OnchainConfirm {
        mint: pump_quant_domain::ids::Mint::from_hex(&hex_of(MINT_B58)).unwrap(),
        sellable_depth_lamports: REAL_CURVE_VSOL,
    });
    for _ in 0..6 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    assert!(
        r.promoted > 0,
        "venue-native attention must surface the coin"
    );
    assert_eq!(
        r.admitted, 0,
        "no numeric flow evidence — social observation can never substitute \
         for canonical on-chain truth"
    );
}

/// Duplicate deliveries of the SAME post (cross-provider) and verbatim
/// same-author reposts count ONCE; distinct authors sharing content still
/// both land (that is the §29.7c coordination signature, not a duplicate).
#[test]
fn duplicates_never_inflate_corroboration() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    // Same author+text arriving twice (two capture lanes) + a verbatim repost.
    let batch = vec![
        RawSocialPayload::new(pump_line("walleta", "send it").into_bytes(), 1_000_000_000),
        RawSocialPayload::new(pump_line("walleta", "send it").into_bytes(), 1_000_000_500),
        RawSocialPayload::new(pump_line("walleta", "send it").into_bytes(), 2_000_000_000),
        // A DIFFERENT author with the same text: passes dedup (coordination
        // handles it), so it must still be applied.
        RawSocialPayload::new(pump_line("walletb", "send it").into_bytes(), 1_000_001_000),
    ];
    let mut src = MockSocialSource::new().with_batch(batch);
    let applied = eng.ingest_social(&mut src);
    assert_eq!(
        applied, 2,
        "one per distinct (author, content): duplicates and reposts drop"
    );
}

/// PumpPortal `create` events — previously received and DROPPED — parse into
/// the SAME RawTokenMetadata → TokenMetadata path the on-chain decoder owns,
/// and surface a CreationSniper sighting (earliest-launch discovery, §23).
#[test]
fn pumpportal_create_reaches_creation_discovery() {
    let j = format!(
        "{{\"signature\":\"x\",\"mint\":\"{MINT_B58}\",\"txType\":\"create\",\"name\":\"Based Coin\",\"symbol\":\"BASED\",\"traderPublicKey\":\"{MINT_B58}\"}}"
    );
    let raw = parse_pumpportal_create(j.as_bytes()).expect("create parses");
    let ev = to_token_metadata_v1(&raw);
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    eng.tick(ev);
    for _ in 0..2 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    assert!(
        r.promoted > 0,
        "a decoded launch must be discoverable immediately (CreationSniper)"
    );
    assert_eq!(r.admitted, 0, "discovery is never entry");
}

/// PumpPortal migration events map onto the existing Migration event.
#[test]
fn pumpportal_migration_maps_to_app_event() {
    let j = format!("{{\"mint\":\"{MINT_B58}\",\"txType\":\"migrate\"}}");
    let mint = parse_pumpportal_migration(j.as_bytes()).expect("migration parses");
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    eng.tick(AppEvent::Migration {
        mint: pump_quant_domain::ids::Mint::from_bytes(mint),
        slot: 0,
    });
    eng.tick(AppEvent::Tick);
    // No panic, no fabricated candidate: a migration alone discovers nothing.
    assert_eq!(eng.report().promoted, 0);
}

/// Helper: hex of a base58 mint (test-side only).
fn hex_of(b58: &str) -> String {
    let key = pump_quant_ingest::base58::decode_pubkey(b58).unwrap();
    let mut s = String::with_capacity(64);
    for b in key {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
