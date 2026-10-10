//! Milestone harness: captured raw transactions -> PRODUCTION parser/decoder -> the engine's decision
//! join + flow reducer -> windowed features, for BOTH trade sources, on the same wire lines.
//!
//! Input: a directory of `wire_*.ndjson` (see `docs/missing_history_causal/scripts/to_wire.py`: raw
//! capture records re-serialised into the daemon's wire-line contract; the only non-Rust step).
//! Output (stdout): one JSON line per (mint, clock): event-path and snapshot-path flow aggregates +
//! the snapshot path's flag state. Causality: a clock `t` is served BEFORE any line with
//! `recv_unix_ms >= t` is applied, so no later-received event can reach an earlier clock.
//!
//! `before` = the legacy producer: curve snapshot deltas (derive_market_trade_from_delta) joined to
//! the instruction prints exactly as `pq_daemon` does. `after` = TradeEvents (curve_trade_events).
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};
use std::str::FromStr;

use pump_quant_app::decision_join::{DecisionCache, TradeObs};
use pump_quant_app::event::AppEvent;
use pump_quant_app::state_ledger::VenueLabel;
use pump_quant_junction::curve_trade_events::{ingest_curve_tx, EventDedup, EventIngest};
use pump_quant_junction::laserstream::{
    classify_pump_instructions, instructions_to_events_with_meta, parse_ndjson_line,
    LaserStreamUpdate, PumpInstruction, PUMP_FUN_PROGRAM,
};
use pump_quant_junction::reserve_delta::{
    classify_delta_miss, derive_market_trade_from_delta, DeltaMiss, ReserveSnapshot,
};
use pump_quant_junction::trade_join::{JoinOutcome, TradeJoin};
use pump_quant_market_state::flow_reducer::FlowOutcome;
use pump_quant_protocol::decode::decode_pump_curve;
use pump_quant_protocol::pda::find_program_address;

const GRID_MS: i64 = 60_000;
const MIN_EVENTS: u64 = 5;

fn lines(dir: &str) -> Vec<String> {
    let mut fs: Vec<_> = std::fs::read_dir(dir)
        .expect("dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("wire_"))
        })
        .collect();
    fs.sort();
    fs.into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect()
}

fn each_line(files: &[String], mut f: impl FnMut(&str)) {
    for p in files {
        let r = std::io::BufReader::new(std::fs::File::open(p).expect("open"));
        for l in r.lines().map_while(Result::ok) {
            f(&l);
        }
    }
}

fn obs(e: &AppEvent) -> Option<TradeObs> {
    if let AppEvent::MarketTrade {
        mint,
        price_fp,
        quote_lamports,
        signed_base,
        buyer_entity,
        trader_pubkey,
        recv_unix_ms,
        slot,
        fee_lamports,
        cu_consumed,
        event_id,
        feature,
        ..
    } = e
    {
        Some(TradeObs {
            mint: *mint.as_bytes(),
            price_fp: *price_fp,
            quote_lamports: *quote_lamports,
            signed_base: *signed_base,
            buyer_entity: *buyer_entity,
            trader: *trader_pubkey,
            recv_unix_ms: *recv_unix_ms,
            slot: *slot,
            fee_lamports: *fee_lamports,
            cu_consumed: *cu_consumed,
            venue: VenueLabel::Pumpfun,
            event_id: *event_id,
            feature: *feature,
        })
    } else {
        None
    }
}

fn agg_json(c: &DecisionCache, mint: &[u8; 32], t: i64) -> serde_json::Value {
    match c.flow_aggregates(mint, t) {
        FlowOutcome::NoPriorFlow => serde_json::json!({"no_prior_flow": true}),
        FlowOutcome::Aggregates(a) => serde_json::json!({
            "entrants_60s": a.entrants_60s, "entrants_300s": a.entrants_300s,
            "net_flow_micro": a.net_flow_sol_300s_micro,
            "bot_uniform_micro": a.bot_uniform_share_300s_micro,
            "fee_p90": a.entrant_fee_p90_lamports, "cu_p50": a.entrant_cu_p50,
        }),
    }
}

fn b58(m: &[u8; 32]) -> String {
    solana_program::pubkey::Pubkey::new_from_array(*m).to_string()
}

fn main() {
    let dir = std::env::args().nth(1).expect("wire dir");
    let files = lines(&dir);

    // Pass 1: mints with enough TradeEvents -> clock grid (first..last event recv).
    let mut span: BTreeMap<[u8; 32], (i64, i64, u64)> = BTreeMap::new();
    let mut dd = EventDedup::new(1 << 16);
    each_line(&files, |l| {
        if let Some(LaserStreamUpdate::Transaction(tx)) = parse_ndjson_line(l) {
            let mut out = Vec::new();
            let _ = ingest_curve_tx(&tx, &mut dd, &mut out);
            for pe in out {
                if let (
                    AppEvent::MarketTrade {
                        mint,
                        recv_unix_ms: Some(r),
                        ..
                    },
                    _,
                ) = (pe.event, 0)
                {
                    let s = span.entry(*mint.as_bytes()).or_insert((r, r, 0));
                    s.1 = r;
                    s.2 += 1;
                }
            }
        }
    });
    let mut clocks: Vec<(i64, [u8; 32])> = Vec::new();
    for (m, (a, b, n)) in &span {
        if *n < MIN_EVENTS {
            continue;
        }
        let mut t = (*a / GRID_MS + 1) * GRID_MS;
        while t <= *b + GRID_MS {
            clocks.push((t, *m));
            t += GRID_MS;
        }
    }
    clocks.sort();
    let pump = solana_program::pubkey::Pubkey::new_from_array(PUMP_FUN_PROGRAM);
    let _ = pump;
    let mut pda_to_mint: HashMap<[u8; 32], [u8; 32]> = HashMap::new();
    for m in span.keys() {
        if let Ok((pda, _)) = find_program_address(&[b"bonding-curve", m], &PUMP_FUN_PROGRAM) {
            pda_to_mint.insert(pda, *m);
        }
    }

    // Pass 2: stream, serve clocks before applying lines at/after them.
    let mut ev_cache = DecisionCache::new();
    let mut sn_cache = DecisionCache::new();
    for m in span.keys() {
        ev_cache.track_flow_mint_for_measurement(*m);
        sn_cache.track_flow_mint_for_measurement(*m);
    }
    let mut dedup = EventDedup::new(1 << 16);
    let mut join = TradeJoin::new(4096, 8);
    let mut tracker: HashMap<[u8; 32], ReserveSnapshot> = HashMap::new();
    let mut drops: HashMap<[u8; 32], Vec<i64>> = HashMap::new();
    let mut ev_gaps: HashMap<[u8; 32], Vec<i64>> = HashMap::new();
    let mut ci = 0usize;
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    let (mut n_incomplete, mut n_ev, mut n_dup, mut n_snap_trades, mut n_unres) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut ingest_ev: BTreeMap<String, u64> = BTreeMap::new();
    let mut ingest_sn: BTreeMap<String, u64> = BTreeMap::new();
    let mut join_out: BTreeMap<String, u64> = BTreeMap::new();
    each_line(&files, |l| {
        let Some(u) = parse_ndjson_line(l) else {
            return;
        };
        let r = match &u {
            LaserStreamUpdate::Transaction(t) => t.recv_unix_ms,
            LaserStreamUpdate::Account { recv_unix_ms, .. } => *recv_unix_ms,
            _ => None,
        };
        let Some(r) = r else { return };
        while ci < clocks.len() && clocks[ci].0 <= r {
            let (t, m) = clocks[ci];
            let dl = drops
                .get(&m)
                .map(|v| v.iter().filter(|d| **d >= t - 300_000 && **d < t).count())
                .unwrap_or(0);
            let rec = serde_json::json!({
                "mint": b58(&m), "t": t,
                "after": agg_json(&ev_cache, &m, t), "before": agg_json(&sn_cache, &m, t),
                "snapshot_drops_in_window": dl,
                "event_gaps_in_window": ev_gaps.get(&m).map(|v| v.iter().filter(|d| **d >= t - 300_000 && **d < t).count()).unwrap_or(0),
            });
            let _ = writeln!(out, "{rec}");
            ci += 1;
        }
        match u {
            LaserStreamUpdate::Transaction(tx) => {
                // AFTER: TradeEvents.
                let mut evs = Vec::new();
                match ingest_curve_tx(&tx, &mut dedup, &mut evs) {
                    EventIngest::Produced { events, duplicates } => {
                        n_ev += events as u64;
                        n_dup += duplicates as u64;
                    }
                    EventIngest::Incomplete(_) => {
                        n_incomplete += 1;
                        // Same naming rule as pq_daemon: every mint the instructions name gets a gap.
                        if let Some(ms) = tx.recv_unix_ms {
                            for c in classify_pump_instructions(&tx) {
                                if let PumpInstruction::Buy { mint, .. }
                                | PumpInstruction::Sell { mint, .. } = c
                                {
                                    ev_gaps.entry(mint).or_default().push(ms);
                                }
                            }
                        }
                    }
                    EventIngest::Nothing => {}
                }
                for pe in &evs {
                    if let Some(o) = obs(&pe.event) {
                        *ingest_ev
                            .entry(format!("{:?}", ev_cache.observe_trade(&o)))
                            .or_default() += 1;
                    }
                }
                // BEFORE: legacy instruction prints join the snapshot-derived trades.
                let classified = classify_pump_instructions(&tx);
                for pe in instructions_to_events_with_meta(
                    &classified,
                    tx.slot,
                    true,
                    tx.recv_unix_ms,
                    tx.fee_lamports,
                    tx.cu_consumed,
                ) {
                    if let AppEvent::MarketTrade {
                        mint,
                        signed_base,
                        buyer_entity,
                        trader_pubkey: Some(pk),
                        recv_unix_ms,
                        ..
                    } = pe.event
                    {
                        if !matches!(classified.first(), Some(PumpInstruction::AmmSwap(_))) {
                            join.note_instruction_with_meta(
                                mint.as_bytes(),
                                pe.slot,
                                buyer_entity,
                                pk,
                                signed_base > 0,
                                recv_unix_ms,
                                tx.fee_lamports,
                                tx.cu_consumed,
                            );
                        }
                    }
                }
            }
            LaserStreamUpdate::Account {
                pubkey,
                data,
                slot,
                recv_unix_ms,
                ..
            } => {
                let Some(mb) = pda_to_mint.get(&pubkey).copied() else {
                    n_unres += 1;
                    return;
                };
                let Some(curve) = decode_pump_curve(&data) else {
                    return;
                };
                let prev = tracker.get(&mb).copied();
                if let Some(mut pe) =
                    derive_market_trade_from_delta(&mb, prev, &curve, slot, true, recv_unix_ms)
                {
                    let is_buy = matches!(pe.event, AppEvent::MarketTrade { signed_base, .. } if signed_base > 0);
                    let jo = join.take_identity(&mb, slot, is_buy);
                    *join_out
                        .entry(
                            match jo {
                                JoinOutcome::Identity { .. } => "identity",
                                JoinOutcome::Ambiguous => "ambiguous",
                                JoinOutcome::Unknown => "unknown",
                            }
                            .to_string(),
                        )
                        .or_default() += 1;
                    if let JoinOutcome::Identity {
                        entity,
                        pubkey,
                        fee_lamports: f,
                        cu_consumed: c,
                    } = jo
                    {
                        if let AppEvent::MarketTrade {
                            buyer_entity,
                            trader_pubkey,
                            fee_lamports,
                            cu_consumed,
                            ..
                        } = &mut pe.event
                        {
                            *buyer_entity = entity;
                            *trader_pubkey = Some(pubkey);
                            *fee_lamports = f;
                            *cu_consumed = c;
                        }
                    }
                    if let Some(o) = obs(&pe.event) {
                        n_snap_trades += 1;
                        *ingest_sn
                            .entry(format!("{:?}", sn_cache.observe_trade(&o)))
                            .or_default() += 1;
                    }
                } else if let Some(ms) = recv_unix_ms {
                    match classify_delta_miss(prev.as_ref(), &curve, slot) {
                        DeltaMiss::UpstreamDropped | DeltaMiss::InvalidObservation => {
                            drops.entry(mb).or_default().push(ms);
                        }
                        _ => {}
                    }
                }
                tracker.insert(
                    mb,
                    ReserveSnapshot::of(&curve, slot),
                );
            }
            _ => {}
        }
    });
    let _ = writeln!(
        out,
        "{}",
        serde_json::json!({"summary": {
            "clocks": clocks.len(), "mints_with_clocks": span.values().filter(|s| s.2 >= MIN_EVENTS).count(),
            "event_trades": n_ev, "event_duplicates": n_dup, "event_incomplete_tx": n_incomplete,
            "snapshot_trades": n_snap_trades, "snapshot_join_outcomes": join_out, "snapshot_unresolved_curve": n_unres,
            "ingest_after": ingest_ev, "ingest_before": ingest_sn,
        }})
    );
    let _ = solana_program::pubkey::Pubkey::from_str("11111111111111111111111111111111");
}
