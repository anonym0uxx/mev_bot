//! Wave-3 Discord paid-alpha law attribution (LAWs D1–D5): A/B proof that each
//! newly wired law changes the audited outcome in the mandated direction on a tape
//! built to contain exactly the hazard it targets. Mirrors `audit_wave2_laws.rs`
//! exactly — isolate the law with a config toggle, drive the SAME event tape twice
//! (armed vs neutralized), and assert the armed arm strictly wins in the law's own
//! axis. Determinism (§22) makes every comparison exact, not statistical.
//!
//! Operator context: the operator subscribes to PAID Discord alpha rooms; alpha
//! calls arrive via the Discord capture lane and must be reviewed, attributed,
//! cached, and taken in as ACTIONABLE ALPHA — for ENTRIES (raise rank / attention /
//! earliness, never self-authorize) AND EXITS (a bearish sell call accelerates a
//! held exit, reduce-only). These tests pin that behaviour law-by-law:
//!   * D2 designated-caller attention weight — a known paid-alpha caller ranks a
//!     genuine winner higher than its absence (surfaces it).
//!   * D3 bearish alpha sell call → reduce-only held-exit pressure that avoids loss.
//!   * D4 alpha-alone-cannot-admit — the on-chain + economic gate still fires; only
//!     WITH a confirm does the same alpha-called market admit (a pinned invariant).
//!   * D5 per-room net-SOL attribution — two rooms, one leading winners and one
//!     leading losers, accrue distinct realized net in the §29.8 outcome ledger.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::journal_log::Decision;
use pump_quant_domain::ids::Mint;
use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};
use pump_quant_social::types::{SourceKind, SourceRef};

/// **DEPTH REALISM (re-pin #26).** The gate's price-impact model is now DERIVED from
/// the market's own SOL-side reserve (`cost_model::impact_den_for`), so a fixture's
/// declared depth is a decision input rather than decoration. Real pump.fun virtual
/// reserves START at 30 SOL; the sub-SOL depths these fixtures used to declare put the
/// operator's 0.1 SOL floor clip at 20-125% of the pool — a market in which no
/// strategy result means anything (Amendment A-13(1)).
/// **A REAL BONDING CURVE THAT HAS BEEN BOUGHT INTO (corrected 2026-07-28).**
///
/// pump.fun seeds a curve with **30 SOL of VIRTUAL reserve and ZERO real SOL**, and
/// escrows `real_sol = virtual_sol - 30 SOL` thereafter. This constant used to be the
/// bare seed reserve (30 SOL) paired with a "sellable depth" of 29-30 SOL — a market
/// that cannot exist, since a curve nobody has bought into can pay out nothing at all.
/// It is now a curve with 0.3 SOL genuinely raised: the price reserve is close enough
/// to the seed that own-impact on a 0.1 SOL floor clip is unchanged at 33 bps a leg,
/// and the payout reserve is the 0.3 SOL that was actually paid in.
/// See `curve_state::real_sol_for`.
const REAL_CURVE_VSOL: u64 = 30_300_000_000;
/// The SOL this curve actually escrows — `REAL_CURVE_VSOL - LAUNCH_VSOL_LAMPORTS`,
/// the identity, not a choice. This is what caps `size_band`'s `x_max`.
const REAL_CURVE_REAL_SOL: u64 = 300_000_000;

const PRICE_SCALE: i128 = 10_000_000;

/// Decode a base58 pubkey to a `Mint` (valid by construction for these tests).
fn b58(s: &str) -> Mint {
    Mint::from_bytes(pump_quant_ingest::base58::decode_pubkey(s).expect("valid pubkey"))
}

/// Advance the logical clock by `n` ticks (each runs a full evaluate()).
fn ticks(eng: &mut Engine, n: u64) {
    for _ in 0..n {
        eng.tick(AppEvent::Tick);
    }
}

/// One trade for `m` at `price_mult × PRICE_SCALE` with the given signed base flow
/// and a rotating buyer entity (so the §21.5 wash guard never filters the tape).
fn one(eng: &mut Engine, m: Mint, price_mult: i128, signed_base: i64, entity: u64) {
    eng.tick(AppEvent::MarketTrade {
        mint: m,
        price_fp: price_mult * PRICE_SCALE,
        quote_lamports: 800_000,
        liquidity_lamports: REAL_CURVE_VSOL,
        signed_base,
        buyer_entity: entity,
        age_slots: 12,
    });
}

/// Feed ONE Discord alpha-room NDJSON post (designated caller) naming `addr`, into
/// the engine via the real capture seam. `sentiment` is `Some((bp, conf))` for a
/// sentiment-annotated (e.g. bearish sell) call, `None` for a plain bullish call.
fn discord_call(
    eng: &mut Engine,
    room: &str,
    author: &str,
    addr: &str,
    ts_ns: u64,
    sentiment: Option<(u32, u32)>,
) {
    let sent = match sentiment {
        Some((bp, conf)) => format!(",\"sentiment_bp\":{bp},\"sentiment_conf_bp\":{conf}"),
        None => String::new(),
    };
    let json = format!(
        "{{\"platform\":\"discord\",\"author\":\"{author}\",\"community\":\"{room}\",\
         \"text\":\"call {addr} r send\",\"likes\":0,\"is_designated_caller\":true{sent}}}"
    );
    let mut src =
        MockSocialSource::new().with_batch(vec![RawSocialPayload::new(json.into_bytes(), ts_ns)]);
    eng.ingest_social(&mut src);
}

/// Count journalled rejections carrying `reason` for one mint.
fn reject_count(eng: &Engine, m: Mint, reason: u8) -> usize {
    let bytes = *m.as_bytes();
    eng.journal()
        .recent()
        .filter(
            |d| matches!(**d, Decision::Rejected { mint, reason: r } if mint == bytes && r == reason),
        )
        .count()
}

// ============================================================================
// LAW D2 — designated-caller attention weight (§29).
//
// A known paid-alpha caller (or curated followed key account) is high signal, so a
// fresh designated call adds elevated attention — BREADTH-GATED (each distinct
// caller half-forms; echoes add zero), never a blank multiplier. On an otherwise
// identical low-attention mention stream, the armed weight lifts a designated-led
// mint's discovery score STRICTLY above its absence (it surfaces the call); the
// class-unconditioned path treats the two identically. Mirrors the LAW 9
// (platform-lead) attention-field A/B. Determinism (§22) makes it exact.
// ============================================================================

#[test]
fn designated_caller_weight_surfaces_a_paid_call_over_its_absence() {
    use pump_quant_app::attention::{AttentionField, AttentionParams, MentionProvenance};
    use pump_quant_narrative::attention_state::Mention;

    fn men(ts_ns: u64, source_id: u64, weight: u64) -> Mention {
        Mention {
            ts_ns,
            source_id,
            community_id: source_id,
            weight,
            copycat: false,
        }
    }

    // Identical attention dynamics in every arm — SAME mentions, SAME two extra
    // "caller" posts as the attention BUILDS in round 2. `designated` flips ONLY the
    // provenance flag on those two caller posts (designated vs plain), so the sole
    // difference between the armed arms is the D2 weight itself (never an extra
    // mention). The two distinct designated callers complete formation as the coin
    // takes off; a lone caller would be half-formation. Money confirmed so the §29
    // fade cap (which binds identically in both arms) does not mask the comparison.
    fn run(enable: bool, designated: bool) -> u64 {
        let params = AttentionParams {
            designated_caller_enable: enable,
            ..AttentionParams::standard()
        };
        let mut f = AttentionField::new(params);
        let m = [3u8; 32];
        let plain = MentionProvenance::default();
        let caller = MentionProvenance {
            designated_caller: true,
            ..MentionProvenance::default()
        };
        let caller_prov = if designated { &caller } else { &plain };
        // Round 1: 4 thin plain mentions establish a low baseline (below formation).
        for s in 0..4u64 {
            f.observe_tagged(m, men(1_000 + s * 10, s, 4), &plain);
        }
        let mut buf = Vec::new();
        f.emit_into(&mut buf, 1, |_| 1_000, |_| true);
        // Round 2: the SAME 4 plain mentions, PLUS two distinct caller posts as the
        // coin takes off. Their designated-caller weight (armed) lifts the level
        // across formation, so the growth ratio (branching factor) jumps.
        for s in 0..4u64 {
            f.observe_tagged(m, men(2_000 + s * 10, s, 6), &plain);
        }
        f.observe_tagged(m, men(2_100, 100, 6), caller_prov);
        f.observe_tagged(m, men(2_110, 101, 6), caller_prov);
        buf.clear();
        f.emit_into(&mut buf, 2, |_| 1_000, |_| true);
        buf.first().map(|c| c.discovery_score).unwrap_or(0)
    }

    let armed_designated = run(true, true);
    let armed_plain = run(true, false);
    let neut_designated = run(false, true);
    let neut_plain = run(false, false);

    eprintln!(
        "D2 armed_designated={armed_designated} armed_plain={armed_plain} \
         neut_designated={neut_designated} neut_plain={neut_plain}"
    );
    // Unconditioned: designated-caller provenance is inert — the two mints score
    // identically (no per-source trust in the class-off path, §29.8).
    assert_eq!(
        neut_designated, neut_plain,
        "without the law the designated caller earns no elevated weight"
    );
    // Armed: the designated-led mint out-scores the identical non-designated one —
    // the paid caller is surfaced. Breadth-gated: two distinct callers complete the
    // formation the plain stream never reaches at this thin weight.
    assert!(
        armed_designated > armed_plain,
        "the §29 designated-caller weight must lift a paid-call-led mint above its \
         absence ({armed_designated} vs {armed_plain})"
    );
}

/// Breadth gate: a LONE designated caller is half-formation, and a coordinated
/// ECHO flood adds zero designated breadth (fade-first, §29) — only genuine
/// DISTINCT corroboration completes it. Pins that the weight is not a blank
/// multiplier a raid can farm.
#[test]
fn designated_caller_weight_is_breadth_gated_not_a_blank_multiplier() {
    use pump_quant_app::attention::{AttentionField, AttentionParams, MentionProvenance};
    use pump_quant_narrative::attention_state::Mention;

    fn men(ts_ns: u64, source_id: u64) -> Mention {
        Mention {
            ts_ns,
            source_id,
            community_id: source_id,
            weight: 4,
            copycat: false,
        }
    }
    // `distinct` ⇒ N genuine distinct designated callers; otherwise ONE caller
    // repeated as coordinated echoes (echo_or_coordinated set) — reach, not breadth.
    fn run(distinct: bool, n: u64) -> u64 {
        let params = AttentionParams::standard();
        let params = AttentionParams {
            designated_caller_enable: true,
            ..params
        };
        let mut f = AttentionField::new(params);
        let m = [4u8; 32];
        for i in 0..n {
            let prov = MentionProvenance {
                designated_caller: true,
                echo_or_coordinated: !distinct,
                author_id: if distinct { i } else { 999 },
                ..MentionProvenance::default()
            };
            f.observe_tagged(
                m,
                men(1_000 + i * 10, if distinct { i } else { 999 }),
                &prov,
            );
        }
        let mut buf = Vec::new();
        f.emit_into(&mut buf, 1, |_| 1_000, |_| true);
        buf.clear();
        for i in 0..n {
            let prov = MentionProvenance {
                designated_caller: true,
                echo_or_coordinated: !distinct,
                author_id: if distinct { i } else { 999 },
                ..MentionProvenance::default()
            };
            f.observe_tagged(
                m,
                men(2_000 + i * 10, if distinct { i } else { 999 }),
                &prov,
            );
        }
        buf.clear();
        f.emit_into(&mut buf, 2, |_| 1_000, |_| true);
        buf.first().map(|c| c.discovery_score).unwrap_or(0)
    }

    // Genuine distinct breadth must never rank below a single-caller echo flood of
    // the same size (echoes add zero designated breadth — fade-first, §29).
    let distinct = run(true, 6);
    let echo_flood = run(false, 6);
    eprintln!("D2-breadth distinct={distinct} echo_flood={echo_flood}");
    assert!(
        distinct >= echo_flood,
        "distinct designated breadth must not rank below an echo flood \
         ({distinct} vs {echo_flood})"
    );
}

// ============================================================================
// LAW D4 — alpha-alone-cannot-admit (§29.8/§6.6 corroboration discipline).
//
// A designated caller in a paid room calls a mint. With NO on-chain confirmation
// and NO numeric microstructure, the mint is PROMOTED (the AlphaCall lane surfaces
// it) but the gate REFUSES it (NeedsOnchainConfirmation, code 1) — alpha raises
// rank/attention/earliness but can NEVER authorize capital alone. Add a real
// on-chain confirm + net-buy microstructure and the SAME alpha-called market
// admits — proving alpha never substitutes for the gate. A pinned invariant.
// ============================================================================

const D4_B58: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";

fn drive_alpha_admission(with_onchain: bool) -> (Report, Engine) {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let m = b58(D4_B58);
    for round in 0..4u64 {
        // The paid room repeatedly calls the mint (designated callers).
        let ts = 1_000_000_000u64 + round * 10_000_000;
        discord_call(&mut eng, "room-x", "lead", D4_B58, ts, None);
        discord_call(&mut eng, "room-x", "second", D4_B58, ts + 1_000, None);
        if with_onchain {
            // Real on-chain support: net-buy microstructure + a confirm. Only NOW
            // may the market admit — the alpha calls did not change.
            for i in 0..10u64 {
                one(&mut eng, m, 100 + i as i128, 900_000 - i as i64, 40 + i % 7);
            }
            eng.tick(AppEvent::OnchainConfirm {
                mint: m,
                virtual_sol_lamports: REAL_CURVE_VSOL,
                real_sol_lamports: REAL_CURVE_REAL_SOL,
            });
        }
        ticks(&mut eng, 3);
    }
    let r = eng.report();
    (r, eng)
}

#[test]
fn alpha_alone_cannot_admit_but_admits_with_onchain_confirm() {
    // Alpha alone: no confirm, no numeric — the market is promoted but refused.
    let (alpha_only, aeng) = drive_alpha_admission(false);
    assert!(
        alpha_only.promoted > 0,
        "the AlphaCall lane must SURFACE the alpha-called mint (promoted {})",
        alpha_only.promoted
    );
    assert_eq!(
        alpha_only.admitted, 0,
        "alpha evidence alone can NEVER admit an entry (§29.8/§6.6) — got {}",
        alpha_only.admitted
    );
    // The refusal is specifically the missing on-chain confirmation (gate code 1).
    assert!(
        reject_count(&aeng, b58(D4_B58), 1) > 0,
        "alpha alone must be refused for want of on-chain confirmation (code 1)"
    );

    // Same alpha calls + a real on-chain confirm and microstructure: it admits —
    // alpha never SUBSTITUTES for the gate, it only accelerates a real setup.
    let (confirmed, _ceng) = drive_alpha_admission(true);
    assert!(
        confirmed.admitted > 0,
        "with an on-chain confirm the same alpha-called market must admit (got {})",
        confirmed.admitted
    );
}

// ============================================================================
// LAW D5 — per-Discord-room realized-net-SOL attribution (§29.8/§71/§74).
//
// Two PAID rooms: room-WIN leads a genuine winner, room-LOSE leads a market that
// craters. Both admit and close. The per-source outcome ledger accrues each room's
// realized net DISTINCTLY — room-WIN positive, room-LOSE negative — the seam
// reflection uses to up/down-weight or retire a paid room on its own outcome.
// ============================================================================

const D5_WIN_B58: &str = "6VNKYELM4Wt6z8xgkbYXVd4X9M8j1J8v8k6d3Qb2n7pQ";
const D5_LOSE_B58: &str = "8HrHTZjMFRZDdFTUVJXTgNzZWx1n5cV6yHmTbmVjPkN9";

fn drive_two_rooms() -> (Report, Engine) {
    let mut cfg = Config::dev_portable();
    cfg.watchlist_ttl_ticks = 6; // discovery goes stale after the first admit (no re-admit churn)
    let mut eng = Engine::new(cfg, RunMode::Replay);
    let win = b58(D5_WIN_B58);
    let lose = b58(D5_LOSE_B58);
    // Both rooms call their mint early (AlphaCall). A net-SELL numeric snapshot +
    // confirm admits each ONCE — net-SELL keeps the §32 momentum thesis quiet AND
    // the numeric lane silent (no self-authorizing re-discovery churn), so each
    // room's ONE position plays out cleanly. The room bound the mint as its alpha
    // source either way — the D5 ledger records the room's realized outcome.
    for r in 0..3u64 {
        let ts = 1_000_000_000u64 + r * 10_000;
        discord_call(&mut eng, "room-win", "lead", D5_WIN_B58, ts, None);
        discord_call(&mut eng, "room-lose", "lead", D5_LOSE_B58, ts + 500, None);
    }
    for i in 0..10u64 {
        one(&mut eng, win, 100, -500_000, 40 + i % 7);
        one(&mut eng, lose, 100, -500_000, 50 + i % 7);
    }
    eng.tick(AppEvent::OnchainConfirm {
        mint: win,
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    eng.tick(AppEvent::OnchainConfirm {
        mint: lose,
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    discord_call(
        &mut eng,
        "room-win",
        "lead",
        D5_WIN_B58,
        1_000_100_000,
        None,
    );
    discord_call(
        &mut eng,
        "room-lose",
        "lead",
        D5_LOSE_B58,
        1_000_100_500,
        None,
    );
    ticks(&mut eng, 2); // admit both
                        // Winner rides UP on net-SELL flow (rising price, §32 quiet); Loser CRATERS on
                        // a −40% single-swap collapse (rug precursor) → a booked loss.
    for i in 0..14u64 {
        one(&mut eng, win, 101 + i as i128, -400_000, 40 + i % 7);
        ticks(&mut eng, 1);
    }
    one(&mut eng, lose, 60, -1_500_000, 55); // −40% precursor
    ticks(&mut eng, 6);
    let r = eng.report();
    (r, eng)
}

#[test]
fn two_paid_rooms_accrue_distinct_per_source_net() {
    let (r, eng) = drive_two_rooms();
    let win_room = SourceRef::new(
        SourceKind::Discord,
        pump_quant_ingest::social_parse::fnv1a_64(b"room-win"),
    );
    let lose_room = SourceRef::new(
        SourceKind::Discord,
        pump_quant_ingest::social_parse::fnv1a_64(b"room-lose"),
    );
    assert_ne!(win_room, lose_room, "the two rooms are distinct sources");

    let ledger = eng.source_outcome_ledger();
    let win_net = ledger.net_sol(win_room);
    let lose_net = ledger.net_sol(lose_room);
    eprintln!(
        "D5 admitted={} win_net={win_net} lose_net={lose_net} report={:?}",
        r.admitted, r.per_alpha_source_net
    );
    // Both rooms' positions opened and closed (the ledger recorded an outcome each).
    assert!(
        ledger.trade_count(win_room) > 0 && ledger.trade_count(lose_room) > 0,
        "both rooms must have a reconciled realized outcome"
    );
    // The winner-leading room earns positive net; the loser-leading room is negative
    // — distinct realized attribution per room (§29.8), the grading seam.
    assert!(
        win_net > 0,
        "the room that led the winner must accrue positive net ({win_net})"
    );
    assert!(
        lose_net < 0,
        "the room that led the loser must accrue negative net ({lose_net})"
    );
    // And the Report surfaces the same split (sorted, report-plane readout).
    let win_reported = r
        .per_alpha_source_net
        .iter()
        .find(|(s, _)| *s == win_room)
        .map(|(_, n)| *n);
    let lose_reported = r
        .per_alpha_source_net
        .iter()
        .find(|(s, _)| *s == lose_room)
        .map(|(_, n)| *n);
    assert_eq!(
        win_reported,
        Some(win_net),
        "report matches the ledger (win)"
    );
    assert_eq!(
        lose_reported,
        Some(lose_net),
        "report matches the ledger (lose)"
    );
}

// ============================================================================
// LAW D3 — bearish alpha SELL call → reduce-only held-exit pressure (§29.5).
//
// A market opens a position and pumps into a small profit, then plateaus (no new
// highs) and fades below entry. A DESIGNATED caller posts a high-confidence BEARISH
// sell call while the position is HELD and still in profit: armed, it raises
// reduce-only exit pressure (halves the stall window), so the position exits on the
// tightened stall NEAR THE TOP; neutral rides the plateau the full stall window and
// fades into a loss. The armed arm keeps strictly more lamports (loss AVOIDED, §52
// spirit). The law NEVER opens, sizes, or authorizes — only accelerates the exit.
// ============================================================================

const D3_B58: &str = "4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R";

/// Total realized net across journalled exits carrying `reason` (ExitReason code).
fn fill_net(eng: &Engine, reason: u8) -> i128 {
    eng.journal()
        .recent()
        .filter_map(|d| match *d {
            Decision::Filled {
                reason: r,
                net_pnl_lamports,
                ..
            } if r == reason => Some(net_pnl_lamports),
            _ => None,
        })
        .sum()
}

/// A narrative blast for `held` (drives corroboration-lane discovery — the entry
/// lane is irrelevant to the D3 EXIT law, so we reuse the proven audit scaffolding).
fn narrate(eng: &mut Engine, held: Mint) {
    eng.tick(AppEvent::NarrativeSample {
        mint: held,
        prior_active: 5,
        new_mentions: 9_000,
    });
}

/// Discover `held` through the narrative lane and open a scalp on a net-SELL numeric
/// snapshot (so the §32 net-buy momentum thesis stays quiet and CVD-rollover never
/// arms — cvd_peak stays ≤ 0). Leaves the position OPEN at entry ≈ 100. Mirrors the
/// audit `seed_open` that reliably holds a position.
fn seed_open_held(eng: &mut Engine, held: Mint) {
    for _ in 0..4 {
        narrate(eng, held);
    }
    for i in 0..10u64 {
        one(eng, held, 100, -500_000, 60 + i % 7);
    }
    eng.tick(AppEvent::OnchainConfirm {
        mint: held,
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    narrate(eng, held);
    ticks(eng, 2); // admit
}

/// The shared hazard tape. The position pumps to a small profit (peak ≈ +8%, held
/// BELOW the +10% derived-ladder floor so NO tranche banks and the whole position
/// rides on the stall/trail — isolating the exit MECHANIC), the bearish alpha SELL
/// call lands at the top, then the market PLATEAUS just under the peak (no new highs,
/// give-back < the 22% trail) before cratering below entry. Only
/// `alpha_exit_pressure_enable` differs between arms:
///   * armed ⇒ the halved stall window fires an early ThesisInvalidation exit while
///     STILL IN PROFIT on the plateau;
///   * neutral ⇒ the full stall window is never reached before the crater drags the
///     position out below entry — a realized loss.
fn drive_bearish_alpha(cfg: Config) -> (Report, Engine) {
    let held = b58(D3_B58);
    let mut eng = Engine::new(cfg, RunMode::Replay);
    seed_open_held(&mut eng, held);
    // Pump UP to peak ≈ +8% on net-SELL flow (new highs each step ⇒ no stall yet;
    // last_high_tick advances to the peak tick). Spaced so the peak is well-defined.
    for m in [102i128, 104, 106, 108] {
        one(&mut eng, held, m, -400_000, 55);
        ticks(&mut eng, 2);
    }
    // The bearish designated-caller SELL call lands while HELD and in profit (+8%).
    // Armed ⇒ reduce-only exit pressure (halved stall window + trail cap).
    discord_call(
        &mut eng,
        "room-d3",
        "lead",
        D3_B58,
        2_000_000_000,
        Some((800, 9_000)), // bearish sentiment, high confidence
    );
    // Plateau at +7% for 16 ticks (no new highs, give-back 1% < the 22% trail so the
    // trail stays quiet): the armed halved stall window (≈12) fires HERE, in profit;
    // the neutral full window (≈25) does not.
    for _ in 0..16u64 {
        one(&mut eng, held, 107, -500_000, 70);
        ticks(&mut eng, 1);
    }
    // Then the market CRATERS below entry on continued net-SELL: the still-holding
    // (neutral) position trails/stops out at a loss; the armed one already exited.
    for m in [100i128, 94, 88, 82, 78, 74] {
        one(&mut eng, held, m, -600_000, 70);
        ticks(&mut eng, 1);
    }
    ticks(&mut eng, 6);
    let r = eng.report();
    (r, eng)
}

#[test]
fn bearish_alpha_sell_call_accelerates_a_reduce_only_exit_that_avoids_loss() {
    // Isolate the EXIT decision (both arms): the seed discovery goes stale quickly,
    // so an early exit frees the slot WITHOUT re-admitting into the crater (the
    // re-deploy of freed capital is a separate concern, not the D3 axis — the audit
    // LAW-5 precedent). lane_evidence_ttl bounds the narrative lane's re-emission.
    let mut acfg = Config::dev_portable();
    acfg.watchlist_ttl_ticks = 6;
    acfg.lane_evidence_ttl_ticks = 6;
    acfg.alpha_exit_pressure_enable = true; // D3 armed
    let (armed, aeng) = drive_bearish_alpha(acfg);

    let mut ncfg = Config::dev_portable();
    ncfg.watchlist_ttl_ticks = 6;
    ncfg.lane_evidence_ttl_ticks = 6;
    ncfg.alpha_exit_pressure_enable = false; // D3 neutralized
    let (neut, neng) = drive_bearish_alpha(ncfg);

    eprintln!(
        "D3 armed_admitted={} armed_net={} neut_admitted={} neut_net={}",
        armed.admitted, armed.net_lamports, neut.admitted, neut.net_lamports
    );
    for reason in 1u8..=9 {
        let a = fill_net(&aeng, reason);
        let n = fill_net(&neng, reason);
        if a != 0 || n != 0 {
            eprintln!("  reason {reason}: armed={a} neut={n}");
        }
    }
    assert!(
        armed.admitted > 0 && neut.admitted > 0,
        "both arms must open the position"
    );
    // Neutral: no exit pressure — the position rides the plateau into the crater and
    // realizes a loss.
    assert!(
        neut.net_lamports < 0,
        "riding the fade without exit pressure must lose (neutral net {})",
        neut.net_lamports
    );
    // Armed: the bearish alpha call's reduce-only pressure exits near the top —
    // strictly more lamports kept (loss avoided, §52 spirit).
    assert!(
        armed.net_lamports > neut.net_lamports,
        "the §29.5 bearish-alpha exit pressure must strictly out-earn ignoring the \
         sell call ({} vs {})",
        armed.net_lamports,
        neut.net_lamports
    );
}
