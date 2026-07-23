//! Brain-seam sentiment + aggregator-legibility laws: LLM enrichment is a
//! recorded INPUT consumed as corroboration-tier integers — absent = UNKNOWN =
//! byte-inert, bearish = reduce-only, aggregator = earliness-cut, and no
//! sentiment can ever authorize capital.

use pump_quant_app::attention::{AttentionField, AttentionParams, MentionProvenance};
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::social_ingest::{is_bearish, SENTIMENT_BEARISH_MAX_BP, SENTIMENT_MIN_CONF_BP};
use pump_quant_ingest::social_parse::{parse_social_event, SENTIMENT_UNKNOWN};
use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};
use pump_quant_narrative::attention_state::Mention;

const MINT_B58: &str = "9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump";

// ============================================================================
// Parse laws (§6.4): absent/malformed enrichment stays UNKNOWN.
// ============================================================================
#[test]
fn absent_and_malformed_sentiment_stay_unknown() {
    let plain = br#"{"platform":"x","author":"a","text":"gm $WIF"}"#;
    let ev = parse_social_event(plain, 1).unwrap();
    assert_eq!(ev.sentiment_bp, SENTIMENT_UNKNOWN);
    assert_eq!(ev.sentiment_conf_bp, SENTIMENT_UNKNOWN);
    assert!(!ev.aggregator_listed);
    assert!(!is_bearish(&ev), "UNKNOWN is never bearish");

    // Out-of-range annotation = no annotation.
    let bad = br#"{"platform":"x","author":"a","text":"gm","sentiment_bp":99999,"sentiment_conf_bp":10001}"#;
    let ev2 = parse_social_event(bad, 1).unwrap();
    assert_eq!(ev2.sentiment_bp, SENTIMENT_UNKNOWN);
    assert_eq!(ev2.sentiment_conf_bp, SENTIMENT_UNKNOWN);
}

#[test]
fn bearish_requires_confidence_and_threshold() {
    let mk = |s: u32, c: u32| {
        let j = format!(
            "{{\"platform\":\"x\",\"author\":\"a\",\"text\":\"rug\",\"sentiment_bp\":{s},\"sentiment_conf_bp\":{c}}}"
        );
        parse_social_event(j.as_bytes(), 1).unwrap()
    };
    assert!(is_bearish(&mk(
        SENTIMENT_BEARISH_MAX_BP,
        SENTIMENT_MIN_CONF_BP
    )));
    assert!(
        !is_bearish(&mk(SENTIMENT_BEARISH_MAX_BP + 1, 10_000)),
        "above bar"
    );
    assert!(
        !is_bearish(&mk(0, SENTIMENT_MIN_CONF_BP - 1)),
        "low confidence"
    );
}

// ============================================================================
// Attention laws: bearish suppresses the live bonus (reduce-only); an
// aggregator listing cuts the pre-legibility earliness (§783).
// ============================================================================
fn mention(ts: u64, src: u64) -> Mention {
    Mention {
        ts_ns: ts,
        source_id: src,
        community_id: 9,
        weight: 3,
        copycat: false,
    }
}

fn score_with(bearish: bool, aggregator: bool) -> u64 {
    let mut f = AttentionField::new(AttentionParams::standard());
    let m = [5u8; 32];
    // Round 1: plain seed (identical for every arm) + first emit.
    for i in 0..6u64 {
        f.observe(m, mention(1_000_000_000 + i, 40 + i));
    }
    let mut buf = Vec::new();
    f.emit_into(&mut buf, 1, |_| 0, |_| true);
    // Round 2: the live structure arrives — broadcaster + chatters — with the
    // arm's bearish/aggregator flags on the final mention.
    for i in 0..8u64 {
        f.observe_tagged(
            m,
            mention(1_000_000_100 + i, 60 + i),
            &MentionProvenance {
                realtime_chat: true,
                broadcaster: i == 0,
                author_id: 60 + i,
                echo_or_coordinated: false,
                aggregator: aggregator && i == 7,
                bearish: bearish && i == 7,
                mainstream: false,
            },
        );
    }
    let mut out = Vec::new();
    f.emit_into(&mut out, 2, |_| 0, |_| true);
    out.first().map(|c| c.discovery_score).unwrap_or(0)
}

#[test]
fn bearish_suppresses_live_enthusiasm_reduce_only() {
    let clean = score_with(false, false);
    let flagged = score_with(true, false);
    assert!(clean > 0);
    assert!(
        flagged < clean,
        "a fresh high-confidence bearish reading must suppress the live bonus \
         ({flagged} vs {clean})"
    );
}

#[test]
fn aggregator_listing_cuts_earliness() {
    let unlisted = score_with(false, false);
    let listed = score_with(false, true);
    assert!(
        listed <= unlisted,
        "a listed coin can never gain earliness ({listed} vs {unlisted})"
    );
}

// ============================================================================
// Authority law: maximally bullish enrichment authorizes NOTHING.
// ============================================================================
#[test]
fn bullish_sentiment_cannot_authorize_entry() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let mut batch = Vec::new();
    for i in 0..6u64 {
        batch.push(RawSocialPayload::new(
            format!(
                "{{\"platform\":\"pump\",\"author\":\"w{i}\",\"community\":\"{MINT_B58}\",\"text\":\"guaranteed 100x {i}\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false,\"mint\":\"{MINT_B58}\",\"sentiment_bp\":10000,\"sentiment_conf_bp\":10000,\"sentiment_model\":\"local-llm-v0\"}}"
            )
            .into_bytes(),
            1_000_000_000 + i,
        ));
    }
    let mut src = MockSocialSource::new().with_batch(batch);
    assert_eq!(eng.ingest_social(&mut src), 6);
    eng.tick(AppEvent::OnchainConfirm {
        mint: pump_quant_domain::ids::Mint::from_bytes(
            pump_quant_ingest::base58::decode_pubkey(MINT_B58).unwrap(),
        ),
        sellable_depth_lamports: 500_000_000,
    });
    for _ in 0..6 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    assert_eq!(
        r.admitted, 0,
        "no numeric flow: the brain's opinion is never trade authority"
    );
}
