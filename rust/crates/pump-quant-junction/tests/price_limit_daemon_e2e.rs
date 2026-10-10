//! W3 — PRICE LIMIT execution correctness through the DAEMON path: `arm_paper_model` (the code `pq_daemon`
//! runs) -> production `InferenceClient` over real TCP -> worker pool -> `model_poll` -> `resolve_entry` ->
//! pending order -> paper fill (`model_try_fills`, v1 observed unless a case says v2).
//!
//! The contract is NOT changed here: `PRICE LIMIT` stays OPTIONAL (seam.rs), the stand-in server plays the
//! model, and no prompt, threshold or model output is altered. Two different questions are kept apart:
//!
//! * PARSER ACCEPTANCE — does the completion become a BUY verdict at all? (`notrade:off_contract:*` if not.)
//! * EXECUTABLE FEASIBILITY — given an accepted limit, does the order fill at the ACTUAL chosen clip, priced
//!   impact- and fee-inclusive on the landing reserves? (`fill_none:limit_not_met` / `limit_unusable` if not.)
//!
//! Every reserve here is SYNTHETIC: this proves engineering behaviour, never profitability.
//!
//! UNITS. `PRICE LIMIT` is lamports per RAW token — the unit of the state line's
//! `price_lamports_per_raw_token=` that the corpus echoes on 7,961/7,961 limit-bearing BUYs
//! (`price_anchor.rs`). The prompt also renders SOL/raw (x1e-9) and SOL/whole token (x1e-3 for 6 dp).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_junction::model_lifecycle::arm_paper_model;

const T0: i64 = 1_800_000_000_000;
const MINT: [u8; 32] = [0xAB; 32];
const CREATOR: [u8; 32] = [0xCD; 32];
/// pump.fun constant product `vsol * vtok` (30 SOL x 1.073e15 raw tokens).
const K: u128 = 30_000_000_000u128 * 1_073_000_000_000_000u128;
/// Initial virtual token reserve minus real tokens (`vtok - real_tok` is constant on a canonical curve).
const VTOK_OFFSET: u64 = 279_900_000_000_000;
const VSOL_OFFSET: u64 = 30_000_000_000;

/// A canonical curve state at virtual SOL `vsol` (vtok = K / vsol; real legs from the offsets).
#[derive(Clone, Copy, Debug)]
struct Pool {
    vsol: u64,
    vtok: u64,
    real_sol: u64,
    real_tok: u64,
}

fn pool(vsol: u64) -> Pool {
    let vtok = (K / u128::from(vsol)) as u64;
    Pool {
        vsol,
        vtok,
        real_sol: vsol - VSOL_OFFSET,
        real_tok: vtok - VTOK_OFFSET,
    }
}

/// Marginal price in the engine's 1e9 fixed point (lamports per raw token x 1e9).
fn spot_fp(p: Pool) -> i128 {
    (u128::from(p.vsol) * 1_000_000_000 / u128::from(p.vtok)) as i128
}

fn wallet(i: u32) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[0] = (i % 200) as u8 + 1;
    w[1] = (i / 200) as u8;
    w[31] = 1;
    w
}

/// Launch + `n` prints whose price IS the decision pool's spot (so the prompt's supplied price is the
/// marginal price at decision, as a reserve-delta producer reports it), mode, reserves, confirm.
fn events(n: u32, p: Pool) -> Vec<AppEvent> {
    let mint = DomainMint::from_bytes(MINT);
    let mut ev = vec![AppEvent::LaunchObserved {
        mint,
        creator: CREATOR,
        launch_unix_ms: T0,
    }];
    for i in 0..n {
        let buy = i % 3 != 0;
        ev.push(AppEvent::MarketTrade {
            mint,
            price_fp: spot_fp(p),
            quote_lamports: 500_000_000 + u64::from(i),
            liquidity_lamports: p.vsol,
            signed_base: if buy { 30_000_000_000 } else { -30_000_000_000 },
            buyer_entity: 1 + u64::from(i),
            age_slots: 30,
            recv_unix_ms: Some(T0 + 1_000 + i64::from(i) * 2_000),
            trader_pubkey: Some(wallet(i)),
            slot: Some(1_000 + u64::from(i)),
            fee_lamports: Some(60_000 + u64::from(i) * 100),
            cu_consumed: Some(90_000 + u64::from(i)),
            venue: Some(TradeVenue::PumpFun),
            event_id: None,
            feature: None,
        });
    }
    let t_last = T0 + 1_000 + i64::from(n) * 2_000;
    ev.push(AppEvent::CurveModeObserved {
        mint,
        mayhem: false,
        slot: 2_000,
    });
    ev.push(AppEvent::CurveObserved {
        mint,
        v_sol_lamports: p.vsol,
        v_tokens: p.vtok,
        real_sol_lamports: p.real_sol,
        real_tokens: p.real_tok,
        recv_unix_ms: Some(t_last),
        slot: 2_000,
    });
    ev.push(AppEvent::OnchainConfirm {
        mint,
        virtual_sol_lamports: p.vsol,
        real_sol_lamports: p.real_sol,
    });
    ev
}

fn read_request(s: &mut TcpStream) -> Option<String> {
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some(h) = text.find("\r\n\r\n") {
            let len = text[..h]
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if buf.len() >= h + 4 + len {
                return Some(text[h + 4..h + 4 + len].to_string());
            }
        }
    }
}

/// What the stand-in model answers, as a function of the user prompt the daemon actually sent.
type Answer = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// The stand-in llama-server: every request is answered by `answer(user_prompt)` on `finish_reason=stop`.
fn serve(answer: Answer) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut s) = conn else { continue };
            let answer = Arc::clone(&answer);
            std::thread::spawn(move || {
                let Some(body) = read_request(&mut s) else {
                    return;
                };
                let req: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let user = req["messages"][1]["content"].as_str().unwrap_or("").to_string();
                let content = answer(&user);
                let choice = serde_json::json!({"message": {"content": content}, "finish_reason": "stop"});
                let out = serde_json::json!({ "choices": [choice] }).to_string();
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    out.len(),
                    out
                );
            });
        }
    });
    url
}

/// The price the prompt SUPPLIED: the state line's `price_lamports_per_raw_token=` token, verbatim digits.
fn supplied_token(user: &str) -> String {
    user.lines()
        .find_map(|l| {
            let (_, rest) = l.split_once("price_lamports_per_raw_token=")?;
            rest.split_whitespace().next().map(str::to_string)
        })
        .expect("the rendered prompt carries the supplied price")
}

/// A BUY completion at `size` whose `PRICE LIMIT` line is `limit` (None = no line).
fn buy(size: &str, limit: Option<&str>) -> String {
    let mut s = format!("DECISION: BUY\nSIZE: {size}\n");
    if let Some(l) = limit {
        s.push_str(&format!("PRICE LIMIT: {l}\n"));
    }
    s.push_str("INVALIDATION: none\nEVIDENCE: flow sustained\n");
    s
}

fn fixed(text: String) -> Answer {
    Arc::new(move |_u: &str| text.clone())
}

/// What one daemon-path run produced.
struct Run {
    open: bool,
    pending: usize,
    rep: BTreeMap<String, u64>,
    /// The clip the authority sized (from the durable order record), when an order was created.
    clip: Option<u64>,
    /// Raw tokens the paper fill delivered, when a position opened.
    tokens: Option<u64>,
}

impl Run {
    fn has(&self, key: &str) -> bool {
        self.rep.contains_key(key)
    }
    fn verdict_buy(&self) -> bool {
        self.rep.keys().any(|k| k.starts_with("verdict:buy"))
    }
}

/// Drive: decision on `dec`, verdict delivered before any landing state, then landing observations on
/// `land` until the order fills or is refused by name (or the BUY itself was refused).
fn run(name: &str, answer: Answer, dec: Pool, land: Pool, v2: bool) -> Run {
    let url = serve(answer);
    let dir = std::env::temp_dir().join(format!("pq_plim_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    let mut e = Engine::new(c, RunMode::Paper);
    let armed = arm_paper_model(&mut e, &url, &dir.join("safety.json"));
    assert!(!armed.blocked_at_start, "{armed:?}");
    if v2 {
        e.model_set_paper_fill(pump_quant_app::shadow_pool::PaperFillVersion::V2Shadow);
    }
    for ev in events(40, dec) {
        e.tick(ev);
    }
    // The verdict arrives while the decision state is still the latest reserve observation.
    let mut order_id = None;
    for _ in 0..40 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
        if let Some((id, _, _)) = e.model_pending_order(&MINT) {
            order_id = Some(id);
            break;
        }
        if e.model_lane_report().keys().any(|k| k.starts_with("notrade:")) {
            break;
        }
    }
    let t_last = T0 + 1_000 + 40 * 2_000;
    let mint = DomainMint::from_bytes(MINT);
    if order_id.is_some() {
        for i in 0..20_i64 {
            e.tick(AppEvent::CurveObserved {
                mint,
                v_sol_lamports: land.vsol,
                v_tokens: land.vtok,
                real_sol_lamports: land.real_sol,
                real_tokens: land.real_tok,
                recv_unix_ms: Some(t_last + 1_500 + i * 500),
                slot: 2_100 + i as u64,
            });
            for _ in 0..3 {
                e.tick(AppEvent::Tick);
            }
            if e.model_position_open(&MINT)
                || e.model_lane_report().keys().any(|k| k.starts_with("fill_none:"))
            {
                break;
            }
        }
    }
    let rep = e.model_lane_report().clone();
    let clip = order_id.and_then(|id| e.model_order_rec(id)).map(|r| r.clip_lamports);
    let open = e.model_position_open(&MINT);
    let _ = std::fs::remove_dir_all(&dir);
    Run {
        open,
        pending: e.model_pending_orders(),
        tokens: if open { e.model_inventory_tokens(&MINT) } else { None },
        rep,
        clip,
    }
}

/// The paper fill's own quote for `clip` on `p` (same module the engine prices with).
fn quote_tokens(p: Pool, clip: u64) -> u64 {
    pump_quant_app::exec_quote::curve_buy(p.vsol, p.vtok, p.real_tok, clip)
        .expect("synthetic canonical curve is quotable")
        .tokens
}

/// All-in average price (fees + impact inside), lamports per raw token.
fn avg(clip: u64, tokens: u64) -> f64 {
    clip as f64 / tokens as f64
}

fn bp(a: f64, b: f64) -> f64 {
    (a / b - 1.0) * 10_000.0
}

const DEC: u64 = 80_000_000_000;

// ───────────────────────── PARSER ACCEPTANCE ─────────────────────────

/// Control + absent: a BUY with no `PRICE LIMIT` line is accepted and fills (68% of trained BUYs).
#[test]
fn absent_limit_is_accepted_and_fills_at_full_size() {
    let p = pool(DEC);
    let r = run("absent", fixed(buy("FULL", None)), p, p, false);
    assert!(r.verdict_buy(), "{:?}", r.rep);
    assert!(r.open, "an absent limit must not block the fill: {:?}", r.rep);
    let (clip, tok) = (r.clip.unwrap(), r.tokens.unwrap());
    assert_eq!(tok, quote_tokens(p, clip), "filled at the actual clip's quote");
    eprintln!("absent FULL: clip={clip} tokens={tok} avg={:.12}", avg(clip, tok));
}

/// Literal "unavailable" text and every unusable number: refused AT THE PARSER, by name, before any
/// order exists. The BUY is not silently treated as limit-less (that would change Qwen's decision).
#[test]
fn literal_and_invalid_limits_are_named_parser_refusals_with_no_order() {
    let p = pool(DEC);
    let cases = [
        "unavailable", "n/a", "absent", "NONE", "", // literal / empty text
        "-0.0002", "0", "0.0", "-0", "NaN", "nan", "inf", "-inf", "1e400", // non-positive / non-finite
        "0x1A", "1,5", "0.0002 lamports", "abc", "0.0002.1", // garbage / unit-suffixed
    ];
    for tok in cases {
        let name = format!("inv{}", tok.len());
        let r = run(&name, fixed(buy("FULL", Some(tok))), p, p, false);
        assert!(!r.open, "`{tok}` opened a position: {:?}", r.rep);
        assert_eq!(r.pending, 0, "`{tok}` left an order: {:?}", r.rep);
        assert!(r.clip.is_none(), "`{tok}` created an order record");
        assert!(
            r.has("notrade:off_contract:BuyWithUnusablePriceLimit"),
            "`{tok}`: expected the named parser refusal, got {:?}",
            r.rep
        );
        assert!(!r.verdict_buy(), "`{tok}` reached a BUY verdict");
    }
}

// ───────────────────────── EXECUTABLE FEASIBILITY ─────────────────────────

/// Clip the authority sizes for `size` on `p` (taken from a limit-less run: the clip never depends on the
/// limit — Rust does not re-size around it).
fn clip_for(size: &str, p: Pool) -> u64 {
    let r = run(&format!("clip{size}"), fixed(buy(size, None)), p, p, false);
    assert!(r.open, "{size}: control must fill: {:?}", r.rep);
    r.clip.unwrap()
}

/// THE CORPUS SHAPE: the candidate copies the supplied price digit-for-digit (7,961/7,961 trained limit
/// rows do). Admission ACCEPTS it (parser + authority: a BUY verdict and an order at FULL size). The paper
/// fill at the ACTUAL clip then prices the order impact- and fee-inclusive, which is strictly above the
/// marginal price the prompt supplied, so the order is refused by name with no position. A prompt price is
/// not a validated buy quote. Same result under the v2 shadow fill.
#[test]
fn a_limit_copied_from_the_prompt_is_accepted_by_admission_but_is_not_executable_at_full_size() {
    let p = pool(DEC);
    let copy: Answer = Arc::new(|u: &str| buy("FULL", Some(&supplied_token(u))));
    for v2 in [false, true] {
        let r = run(&format!("copy{v2}"), Arc::clone(&copy), p, p, v2);
        assert!(r.verdict_buy(), "v2={v2}: admission must accept the copied limit: {:?}", r.rep);
        let clip = r.clip.expect("an order was created at the authority's clip");
        assert!(!r.open, "v2={v2}: a copied prompt price filled: {:?}", r.rep);
        assert_eq!(r.pending, 0);
        assert!(r.has("fill_none:limit_not_met"), "v2={v2}: {:?}", r.rep);
        let supplied = spot_fp(p) as f64 / 1e9;
        let a = avg(clip, quote_tokens(p, clip));
        assert!(a > supplied);
        eprintln!(
            "copy v2={v2}: clip={clip} supplied={supplied:.12} full-size avg={a:.12} (+{:.1} bp over the limit)",
            bp(a, supplied)
        );
    }
}

/// Enforcement is at the ACTUAL chosen clip. One limit between the SMALL and FULL all-in averages: SMALL
/// fills, FULL is refused — the limit binds on the size Qwen chose, never on a reference size. And the
/// rounding is the protective direction: a limit whose `clip/limit` is half a raw token above the exact
/// fill count (achieved average a hair ABOVE the limit) is refused; the exact average itself fills.
#[test]
fn the_limit_is_enforced_at_the_actual_clip_impact_and_fees_inclusive() {
    let p = pool(DEC);
    let (small, full) = (clip_for("SMALL", p), clip_for("FULL", p));
    assert!(full > small, "FULL {full} vs SMALL {small}");
    let (a_s, a_f) = (avg(small, quote_tokens(p, small)), avg(full, quote_tokens(p, full)));
    assert!(a_f > a_s, "impact: the bigger clip pays a worse average");
    let mid = format!("{}", (a_s + a_f) / 2.0);
    let rs = run("midS", fixed(buy("SMALL", Some(&mid))), p, p, false);
    assert!(rs.open, "SMALL under the limit must fill: {:?}", rs.rep);
    let rf = run("midF", fixed(buy("FULL", Some(&mid))), p, p, false);
    assert!(rf.verdict_buy() && !rf.open, "FULL over the limit filled: {:?}", rf.rep);
    assert!(rf.has("fill_none:limit_not_met"), "{:?}", rf.rep);
    // Boundary at FULL.
    let tok = quote_tokens(p, full);
    let exact = format!("{}", full as f64 / tok as f64);
    let r = run("edgeEq", fixed(buy("FULL", Some(&exact))), p, p, false);
    assert!(r.open, "limit == achieved average must fill: {:?}", r.rep);
    assert_eq!(r.tokens, Some(tok));
    let hair = format!("{}", full as f64 / (tok as f64 + 0.5));
    let r = run("edgeLo", fixed(buy("FULL", Some(&hair))), p, p, false);
    assert!(!r.open && r.has("fill_none:limit_not_met"), "{:?}", r.rep);
    eprintln!("size: small={small} avg={a_s:.12} full={full} avg={a_f:.12} mid-limit={mid}");
}

/// The quote moves between decision and landing. The limit is set from the SUPPLIED price with headroom
/// (supplied x 1.05). Landing on the decision state: within the limit, fills. Landing after a 5% move up
/// in vsol (marginal price +~10%): over the limit, refused by name; no position, no pending order.
#[test]
fn a_quote_that_moves_above_the_limit_before_landing_is_refused_by_name() {
    let dec = pool(DEC);
    let headroom: Answer = Arc::new(|u: &str| {
        let s: f64 = supplied_token(u).parse().unwrap();
        buy("FULL", Some(&format!("{}", s * 1.05)))
    });
    let within = run("mvIn", Arc::clone(&headroom), dec, dec, false);
    assert!(within.open, "within the limit must fill: {:?}", within.rep);
    let moved = pool(DEC + DEC / 20);
    let above = run("mvUp", Arc::clone(&headroom), dec, moved, false);
    assert!(above.verdict_buy(), "{:?}", above.rep);
    assert!(!above.open && above.pending == 0, "{:?}", above.rep);
    assert!(above.has("fill_none:limit_not_met"), "{:?}", above.rep);
    // A favourable move (vsol down 5%) only helps.
    let down = run("mvDn", headroom, dec, pool(DEC - DEC / 20), false);
    assert!(down.open, "{:?}", down.rep);
    let clip = above.clip.unwrap();
    eprintln!(
        "moves: limit=supplied*1.05; avg@decision={:.12} avg@moved={:.12} supplied={:.12}",
        avg(clip, quote_tokens(dec, clip)),
        avg(clip, quote_tokens(moved, clip)),
        spot_fp(dec) as f64 / 1e9
    );
}

/// UNITS. The limit is lamports per RAW token. Each rendered mis-reading of the same headroom limit
/// (supplied x 1.05) is driven through the daemon and its outcome pinned, because they fail DIFFERENTLY:
///
/// * lamports/raw (correct): fills.
/// * SOL/raw (x1e-9): parser ACCEPTS (positive, finite); `clip/limit` exceeds u64 -> `fill_none:limit_unusable`.
/// * SOL/whole token (x1e-3): parser accepts; `min_tokens` 1000x the fill -> `fill_none:limit_not_met`.
/// * lamports/whole token (x1e6): parser accepts AND THE ORDER FILLS — a 1e6-too-loose bound never binds.
///   Neither parser nor fill can tell it from a deliberately loose limit; only the F5c grounding counter
///   (capture-only by contract) records it as not grounded on the supplied price.
///
/// Scientific notation (45% of corpus limits, e.g. `7.937045570605475e-05`) is accepted and enforced
/// identically; exercised on a pool whose supplied price renders below 1e-4.
#[test]
fn unit_mistakes_are_accepted_by_the_parser_and_fail_or_pass_only_at_execution() {
    let p = pool(DEC);
    let scaled = |k: f64| -> Answer {
        Arc::new(move |u: &str| {
            let s: f64 = supplied_token(u).parse().unwrap();
            buy("FULL", Some(&format!("{:e}", s * 1.05 * k)))
        })
    };
    let ok = run("uLam", scaled(1.0), p, p, false);
    assert!(ok.open, "{:?}", ok.rep);
    let sol_raw = run("uSolRaw", scaled(1e-9), p, p, false);
    assert!(sol_raw.verdict_buy() && !sol_raw.open, "{:?}", sol_raw.rep);
    assert!(sol_raw.has("fill_none:limit_unusable"), "{:?}", sol_raw.rep);
    let sol_whole = run("uSolWhole", scaled(1e-3), p, p, false);
    assert!(sol_whole.verdict_buy() && !sol_whole.open, "{:?}", sol_whole.rep);
    assert!(sol_whole.has("fill_none:limit_not_met"), "{:?}", sol_whole.rep);
    let lam_whole = run("uLamWhole", scaled(1e6), p, p, false);
    assert!(lam_whole.open, "a 1e6-loose limit is non-binding and fills: {:?}", lam_whole.rep);
    // Scientific notation as rendered: a cheaper pool whose supplied price is < 1e-4 (MID keeps own impact
    // under the 90 bp cap on the thinner pool).
    let cheap = pool(50_000_000_000);
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let seen2 = Arc::clone(&seen);
    let sci: Answer = Arc::new(move |u: &str| {
        let t = supplied_token(u);
        *seen2.lock().unwrap() = t.clone();
        let s: f64 = t.parse().unwrap();
        buy("MID", Some(&format!("{}", s * 1.05)))
    });
    let r = run("uSci", sci, cheap, cheap, false);
    let tok = seen.lock().unwrap().clone();
    assert!(tok.contains("e-"), "the supplied price renders in scientific form: {tok}");
    assert!(r.open, "{:?}", r.rep);
    let r = run("uSciCopy", fixed(buy("MID", Some(&tok))), cheap, cheap, false);
    assert!(r.verdict_buy() && !r.open && r.has("fill_none:limit_not_met"), "{:?}", r.rep);
}
