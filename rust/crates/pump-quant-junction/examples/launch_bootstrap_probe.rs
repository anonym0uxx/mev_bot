//! Bounded, read-only launch-bootstrap probe over a FIXED mint list (one per line on stdin).
//! Uses the production adapter (`HeliusHttp` + `find_launch` + `EvidenceCache`); writes one JSON line per
//! mint to stdout. The key is read from the 0600 file; it is never printed. Not a feed, not a launch.
//!
//!   launch_bootstrap_probe <cache_dir> <max_pages_per_mint> < mints.txt > outcomes.jsonl
use std::io::BufRead;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pump_quant_junction::launch_bootstrap::{
    evidence_json, launch_cached, load_key, Budget, EvidenceCache, HeliusHttp, LaunchOutcome,
};
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cache = EvidenceCache::new(Path::new(&args[1]));
    let max_pages: u32 = args[2].parse().expect("max pages");
    let home = std::env::var("HOME").expect("HOME");
    let key = match load_key(&Path::new(&home).join(".config/pump-quant/helius.env")) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("credential: {e:?}");
            std::process::exit(2);
        }
    };
    let src = HeliusHttp::new(key, Duration::from_secs(30));
    for line in std::io::stdin().lock().lines() {
        let mint = line.expect("stdin").trim().to_string();
        if mint.is_empty() {
            continue;
        }
        let now = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis(),
        )
        .expect("ms");
        let out = launch_cached(&cache, &src, &mint, Budget { max_pages }, now);
        let row = match &out {
            LaunchOutcome::Verified(e) => {
                json!({"mint": mint, "outcome": "verified", "evidence": evidence_json(e)})
            }
            other => {
                json!({"mint": mint, "outcome": format!("{other}"), "detail": format!("{other:?}")})
            }
        };
        println!("{row}");
        std::thread::sleep(Duration::from_millis(150));
    }
}
