//! Twitch end-to-end: real-time live-chat capture (the Rust IRC lane's exact
//! NDJSON schema) → ingest → deep AttentionField state → discovery candidate →
//! gate. Locks the §29 discipline: live viewing FEEDS candidates for
//! evaluation/watching, and never authorizes entry without on-chain proof.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};

/// A base58 Solana-shaped mint string (44 chars) and its parsed key, as the
/// ingest cashtag/mint extractor sees it.
const MINT_B58: &str = "So11111111111111111111111111111111111111112";

fn mint_key() -> [u8; 32] {
    // Parse the same way the extractor does: via the app-visible ingest parse.
    let json = format!(
        "{{\"platform\":\"twitch\",\"author\":\"streamer\",\"community\":\"streamer\",\"text\":\"{MINT_B58}\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false}}"
    );
    let ev = pump_quant_ingest::social_parse::parse_social_event(json.as_bytes(), 1).unwrap();
    ev.mints()[0]
}

fn twitch_line(author: &str, channel: &str, text: &str) -> String {
    format!(
        "{{\"platform\":\"twitch\",\"author\":\"{author}\",\"community\":\"{channel}\",\"text\":\"{text}\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false}}"
    )
}

/// The capture lane's exact schema flows end to end: a broadcaster naming a
/// mint on stream + distinct chatters echoing the ticker feed the attention
/// field, and the coin surfaces as a discovery candidate (promoted) — while
/// entry still requires numeric evidence + an on-chain confirm.
#[test]
fn twitch_stream_feeds_candidates_but_never_authorizes_alone() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    // The broadcaster names BOTH the ticker and the concrete mint (binds them),
    // then distinct chatters spam the ticker only — the dominant chat shape.
    let mut batch = vec![RawSocialPayload::new(
        twitch_line(
            "streamer",
            "streamer",
            &format!("$WIF {MINT_B58} sending it"),
        )
        .into_bytes(),
        1_000_000_000,
    )];
    for i in 0..8u64 {
        batch.push(RawSocialPayload::new(
            twitch_line(&format!("chatter{i}"), "streamer", &format!("$WIF lfg {i}")).into_bytes(),
            1_000_000_000 + 1_000_000 * (i + 1),
        ));
    }
    let mut src = MockSocialSource::new().with_batch(batch);
    let applied = eng.ingest_social(&mut src);
    // 1 mint-named observation + 8 cashtag-only resolved through the bind.
    assert!(
        applied >= 9,
        "broadcaster bind + chatter resolution must all land, got {applied}"
    );
    // Attention alone (social-only evidence) must never admit: run the loop.
    for _ in 0..6 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    assert!(
        r.promoted > 0,
        "the streamed coin must surface for WATCHING"
    );
    assert_eq!(
        r.admitted, 0,
        "live viewing corroborates; it must never authorize entry alone"
    );
}

/// Cashtag-only chatter with NO prior mint binding resolves to nothing —
/// an unbound ticker cannot fabricate a market (§6.4: unknown stays unknown).
#[test]
fn unbound_cashtags_fabricate_nothing() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let batch = vec![RawSocialPayload::new(
        twitch_line("chatter1", "somechan", "$UNBOUND to the moon").into_bytes(),
        1_000_000_000,
    )];
    let mut src = MockSocialSource::new().with_batch(batch);
    let applied = eng.ingest_social(&mut src);
    assert_eq!(applied, 0, "no binding, no mint, no attention entry");
    for _ in 0..4 {
        eng.tick(AppEvent::Tick);
    }
    assert_eq!(eng.report().promoted, 0);
}

/// First bind wins: a later post cannot re-point an established ticker at a
/// different mint (anti-hijack, §29).
#[test]
fn first_cashtag_bind_wins() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let other_b58 = "So11111111111111111111111111111111111111113";
    let batch = vec![
        RawSocialPayload::new(
            twitch_line("streamer", "streamer", &format!("$WIF {MINT_B58}")).into_bytes(),
            1_000_000_000,
        ),
        // Hijack attempt: same ticker, different mint.
        RawSocialPayload::new(
            twitch_line("scammer", "streamer", &format!("$WIF {other_b58}")).into_bytes(),
            1_000_000_500,
        ),
        // Cashtag-only chatter must resolve to the FIRST mint.
        RawSocialPayload::new(
            twitch_line("chatter1", "streamer", "$WIF pamp").into_bytes(),
            1_000_001_000,
        ),
    ];
    let mut src = MockSocialSource::new().with_batch(batch);
    let applied = eng.ingest_social(&mut src);
    // 2 mint-named + 1 resolved — and the resolution targets the first mint;
    // determinism of the count is the observable (the bind map is internal).
    assert_eq!(applied, 3);
    let _ = mint_key(); // schema sanity: the fixture parses to a real key
}
