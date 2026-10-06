//! Offline replay of the production reserve-delta producer over captured curve-account snapshots.
//!
//! Input (stdin): JSON lines `[record_index, curve_pubkey, slot, recv_unix_ms, vsol, vtoken, sig]`
//! in capture order. For each snapshot the REAL `derive_market_trade_from_delta` and
//! `classify_delta_miss` run exactly as the daemon's account handler runs them (prev = the last
//! snapshot for that curve account, updated after every snapshot).
//! Output (stdout): `pubkey \t sig \t slot \t recv_ms \t outcome` with outcome one of
//! derived | no_print | stale_snapshot | invalid_observation | possible_trade.
use std::collections::HashMap;
use std::io::{BufRead, Write};

use pump_quant_junction::reserve_delta::{
    classify_delta_miss, derive_market_trade_from_delta, DeltaMiss, ReserveSnapshot,
};
use pump_quant_protocol::decode::PumpCurve;

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::BufWriter::new(std::io::stdout());
    let mut prev: HashMap<String, ReserveSnapshot> = HashMap::new();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let (Some(pk), Some(slot), Some(ms), Some(vs), Some(vt)) = (
            v[1].as_str(),
            v[2].as_u64(),
            v[3].as_i64(),
            v[4].as_u64(),
            v[5].as_u64(),
        ) else {
            continue;
        };
        let sig = v[6].as_str().unwrap_or("");
        let curve = PumpCurve {
            virtual_sol: vs,
            virtual_token: vt,
            real_sol: 0,
            real_token: 0,
            complete: false,
        };
        let p = prev.get(pk).copied();
        let mint = [0u8; 32];
        let outcome =
            if derive_market_trade_from_delta(&mint, p, &curve, slot, true, Some(ms)).is_some() {
                "derived"
            } else {
                match classify_delta_miss(p.as_ref(), &curve, slot) {
                    DeltaMiss::NoPrint => "no_print",
                    DeltaMiss::StaleSnapshot => "stale_snapshot",
                    DeltaMiss::InvalidObservation => "invalid_observation",
                    DeltaMiss::UpstreamDropped => "possible_trade",
                }
            };
        let _ = writeln!(out, "{pk}\t{sig}\t{slot}\t{ms}\t{outcome}");
        prev.insert(
            pk.to_string(),
            ReserveSnapshot {
                virtual_sol: vs,
                virtual_token: vt,
                slot,
            },
        );
    }
}
