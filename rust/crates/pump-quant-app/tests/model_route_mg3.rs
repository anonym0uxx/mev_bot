//! probe_mg_v3 routing over the REAL wire path: `InferencePool` worker -> `ModelSource::complete_meta_for` -> `InferenceClient` ->
//! TCP. A stub server records every request body. Proves:
//!   * Base (production default): request bodies are byte-identical to `request_body` (no `"model"` field) for BOTH lanes.
//!   * Candidate: each lane carries ITS adapter id, selected by `JobKind` only; alternating entry/mgmt jobs never cross.
//!   * Candidate with an unserved id refuses at `verify_route` (no fallback).
use pump_quant_app::model_authority::ModelSource;
use pump_quant_inference::{request_body, InferenceClient, ModelRoute, RequestFamily};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn stub(models: &'static str) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = Arc::clone(&seen);
    std::thread::spawn(move || {
        for c in l.incoming() {
            let mut c = c.unwrap();
            let s3 = Arc::clone(&s2);
            std::thread::spawn(move || {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut n = 0usize;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h == "\r\n" || h.is_empty() {
                        break;
                    }
                    let lh = h.to_ascii_lowercase();
                    if let Some(v) = lh.strip_prefix("content-length:") {
                        n = v.trim().parse().unwrap();
                    }
                }
                let mut b = vec![0u8; n];
                r.read_exact(&mut b).unwrap();
                let out = if line.starts_with("GET /v1/models") {
                    models.to_string()
                } else {
                    let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
                    let tag = v
                        .get("model")
                        .and_then(|m| m.as_str())
                        .unwrap_or("BASE")
                        .to_string();
                    s3.lock().unwrap().push(v);
                    format!(
                        r#"{{"choices":[{{"message":{{"content":"DECISION: SKIP\nREASON: {tag}\nRISK: x"}},"finish_reason":"stop"}}]}}"#
                    )
                };
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", out.len(), out);
            });
        }
    });
    (url, seen)
}

const MODELS: &str = r#"{"object":"list","data":[{"id":"probe-mg3-base"},{"id":"probe-mg3-entry"},{"id":"probe-mg3-mgmt"}]}"#;

#[test]
fn base_route_keeps_the_production_body_for_both_families() {
    let (url, seen) = stub(MODELS);
    let c = InferenceClient::new(url, Duration::from_secs(5));
    assert_eq!(c.route(), &ModelRoute::Base);
    c.verify_route().unwrap();
    c.complete_meta_for(RequestFamily::Entry, "S1", "U1")
        .unwrap();
    c.complete_meta_for(RequestFamily::Management, "S2", "U2")
        .unwrap();
    c.complete_meta("S3", "U3").unwrap();
    let v = seen.lock().unwrap().clone();
    assert_eq!(
        v,
        vec![
            request_body("S1", "U1"),
            request_body("S2", "U2"),
            request_body("S3", "U3")
        ]
    );
}

#[test]
fn candidate_route_alternating_families_never_cross() {
    let (url, seen) = stub(MODELS);
    let route = ModelRoute::parse(
        Some("candidate"),
        Some("probe-mg3-entry"),
        Some("probe-mg3-mgmt"),
    )
    .unwrap();
    let c = Arc::new(InferenceClient::new(url, Duration::from_secs(5)).with_route(route));
    c.verify_route().unwrap();
    // unrouted call is refused in candidate mode (no silent default family)
    assert!(c.complete_meta("S", "U").is_err());
    let mut hs = vec![];
    for i in 0..40 {
        let c = Arc::clone(&c);
        hs.push(std::thread::spawn(move || {
            let fam = if i % 2 == 0 {
                RequestFamily::Entry
            } else {
                RequestFamily::Management
            };
            let r = c.complete_meta_for(fam, "SYS", &format!("U{i}")).unwrap();
            (fam, r.text)
        }));
    }
    for h in hs {
        let (fam, text) = h.join().unwrap();
        let want = if fam == RequestFamily::Entry {
            "probe-mg3-entry"
        } else {
            "probe-mg3-mgmt"
        };
        assert!(text.contains(want), "{fam:?} answered by {text}");
    }
    let v = seen.lock().unwrap().clone();
    assert_eq!(v.len(), 40);
    for b in &v {
        let u = b["messages"][1]["content"].as_str().unwrap();
        let i: usize = u[1..].parse().unwrap();
        let want = if i % 2 == 0 {
            "probe-mg3-entry"
        } else {
            "probe-mg3-mgmt"
        };
        assert_eq!(b["model"], want);
        let mut b2 = b.clone();
        b2.as_object_mut().unwrap().remove("model");
        assert_eq!(b2, request_body("SYS", u));
    }
}

#[test]
fn candidate_route_with_unserved_adapter_refuses() {
    let (url, _seen) = stub(r#"{"data":[{"id":"probe-mg3-base"},{"id":"probe-mg3-entry"}]}"#);
    let route = ModelRoute::parse(
        Some("candidate"),
        Some("probe-mg3-entry"),
        Some("probe-mg3-mgmt"),
    )
    .unwrap();
    let c = InferenceClient::new(url, Duration::from_secs(5)).with_route(route);
    let e = c.verify_route().unwrap_err();
    assert!(
        format!("{e}").contains("NotServed") && format!("{e}").contains("probe-mg3-mgmt"),
        "{e:?}"
    );
}

/// Writes the exact candidate bodies for both families (golden file consumed by the Python eval mirror's parity test).
#[test]
fn golden_candidate_bodies_for_python_parity() {
    use pump_quant_inference::{routed_request_body, ModelRoute, RequestFamily};
    let route = ModelRoute::parse(
        Some("candidate"),
        Some("probe-mg3-entry"),
        Some("probe-mg3-mgmt"),
    )
    .unwrap();
    let base = ModelRoute::parse(None, None, None).unwrap();
    let g = serde_json::json!({
        "candidate_entry": routed_request_body(&route, RequestFamily::Entry, "SYS", "USER"),
        "candidate_mgmt": routed_request_body(&route, RequestFamily::Management, "SYS", "USER"),
        "base_entry": routed_request_body(&base, RequestFamily::Entry, "SYS", "USER"),
        "base_mgmt": routed_request_body(&base, RequestFamily::Management, "SYS", "USER"),
    });
    if let Ok(p) = std::env::var("PQ_ROUTE_GOLDEN_OUT") {
        std::fs::write(p, serde_json::to_string_pretty(&g).unwrap()).unwrap();
    }
    assert_eq!(g["candidate_entry"]["model"], "probe-mg3-entry");
    assert_eq!(g["candidate_mgmt"]["model"], "probe-mg3-mgmt");
    assert!(g["base_entry"].get("model").is_none());
}
