//! TERMINATION CONTRACT through the daemon path: `arm_paper_model` (the code `pq_daemon` runs) ->
//! production `InferenceClient` over real TCP -> worker pool -> `model_poll` -> `resolve_entry`.
//! For each failure shape the stand-in server returns a valid-looking BUY that MUST NOT become an
//! order; the control (same BUY on `finish_reason="stop"`) MUST fill, so the test can fail.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_junction::model_lifecycle::arm_paper_model;

const T0: i64 = 1_800_000_000_000;
const MINT: [u8; 32] = [0xAB; 32];
const CREATOR: [u8; 32] = [0xCD; 32];
const VSOL: u64 = 37_900_000_000;
const VTOK: u64 = 849_000_000_000_000;
const BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained\n";

fn wallet(i: u32) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[0] = (i % 200) as u8 + 1;
    w[1] = (i / 200) as u8;
    w[31] = 1;
    w
}

fn events(n: u32) -> Vec<AppEvent> {
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
            price_fp: 22_000 + i128::from(i),
            quote_lamports: 500_000_000 + u64::from(i),
            liquidity_lamports: VSOL,
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
    // Canonical (non-Mayhem) curve: pq_daemon pushes CurveModeObserved from the decoded account before its
    // reserves and before OnchainConfirm; an unknown mode is a named entry refusal since the offset merge.
    ev.push(AppEvent::CurveModeObserved {
        mint,
        mayhem: false,
        slot: 2_000,
    });
    ev.push(AppEvent::CurveObserved {
        mint,
        v_sol_lamports: VSOL,
        v_tokens: VTOK,
        real_sol_lamports: 7_900_000_000,
        real_tokens: 569_000_000_000_000,
        recv_unix_ms: Some(t_last),
        slot: 2_000,
    });
    ev.push(AppEvent::OnchainConfirm {
        mint,
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
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

/// Serve every request with `content` and `finish_reason` (None = field absent).
fn serve(content: String, reason: Option<&'static str>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut s) = conn else { continue };
            let content = content.clone();
            std::thread::spawn(move || {
                if read_request(&mut s).is_none() {
                    return;
                }
                let mut choice = serde_json::json!({"message": {"content": content}});
                if let Some(r) = reason {
                    choice["finish_reason"] = serde_json::json!(r);
                }
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

/// Drive the entry ask to its verdict; return (position opened, pending orders, lane report).
fn run(name: &str, content: String, reason: Option<&'static str>) -> (bool, usize, String) {
    let url = serve(content, reason);
    let dir = std::env::temp_dir().join(format!("pq_term_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    let mut e = Engine::new(c, RunMode::Paper);
    let armed = arm_paper_model(&mut e, &url, &dir.join("safety.json"));
    assert!(!armed.blocked_at_start, "{armed:?}");
    for ev in events(40) {
        e.tick(ev);
    }
    let t_last = T0 + 1_000 + 40 * 2_000;
    let mut decided = false;
    for i in 0..60_i64 {
        e.tick(AppEvent::CurveObserved {
            mint: DomainMint::from_bytes(MINT),
            v_sol_lamports: VSOL + 200_000_000,
            v_tokens: VTOK - 4_000_000_000_000,
            real_sol_lamports: 8_100_000_000,
            real_tokens: 565_000_000_000_000,
            // Advancing receive time and slot: a fill lands only on a reserve observation strictly
            // after the order (the production landing rule), never on the state the prompt saw.
            recv_unix_ms: Some(t_last + 1_500 + i * 500),
            slot: 2_100 + i as u64,
        });
        for _ in 0..3 {
            e.tick(AppEvent::Tick);
            std::thread::sleep(Duration::from_millis(20));
        }
        let rep = e.model_lane_report();
        // A BUY verdict waits for its simulated fill; a refusal is final once recorded.
        if e.model_position_open(&MINT) || rep.keys().any(|k| k.starts_with("notrade:")) {
            decided = true;
            break;
        }
    }
    let rep = format!("{:?}", e.model_lane_report());
    assert!(decided, "{name}: no verdict reached the engine: {rep}");
    (e.model_position_open(&MINT), e.model_pending_orders(), rep)
}

#[test]
fn control_a_complete_buy_on_stop_fills() {
    let (open, _, rep) = run("ctl", BUY.to_string(), Some("stop"));
    assert!(
        open,
        "the control must trade or the refusals prove nothing: {rep}"
    );
}

#[test]
fn every_unterminated_shape_reaches_the_engine_and_creates_no_order() {
    let repeat = format!("{BUY}</think>\n\n{BUY}");
    let cases: [(&str, String, Option<&'static str>, &str); 5] = [
        (
            "len",
            BUY.to_string(),
            Some("length"),
            "notrade:unterminated:truncated",
        ),
        (
            "none",
            BUY.to_string(),
            None,
            "notrade:unterminated:no_finish_reason",
        ),
        (
            "filt",
            BUY.to_string(),
            Some("content_filter"),
            "notrade:unterminated:unaccepted_finish_reason",
        ),
        (
            "think",
            repeat,
            Some("stop"),
            "notrade:unterminated:template_marker_in_content",
        ),
        (
            "dup",
            format!("{BUY}DECISION: SKIP\nSIZE: NONE\n"),
            Some("stop"),
            "notrade:unterminated:repeated_field",
        ),
    ];
    for (name, content, reason, key) in cases {
        let (open, pending, rep) = run(name, content, reason);
        assert!(
            !open,
            "{name}: an unterminated BUY opened a position: {rep}"
        );
        assert_eq!(
            pending, 0,
            "{name}: an unterminated BUY placed an order: {rep}"
        );
        assert!(
            rep.contains(&format!("\"{key}\"")),
            "{name}: expected {key}: {rep}"
        );
        assert!(
            rep.contains("\"endpoint:"),
            "{name}: must count against endpoint health: {rep}"
        );
    }
}
