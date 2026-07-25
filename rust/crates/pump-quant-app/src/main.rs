//! `pump-quant-app` — laptop paper/replay runner for the Hermes nervous system.
//!
//! Usage:
//!
//! ```text
//! pump-quant-app <paper|replay> <config-file> <events-file>
//! ```
//!
//! Loads an operator config and a recorded event journal, drives the engine, and
//! prints the net-SOL report and the journal digest. A `live` mode is intentionally
//! unrepresentable: live capital is Tier-0 human-gated and this binary refuses to
//! synthesize authorization for it.

use std::process::ExitCode;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, CreatorActionKind};
use pump_quant_domain::ids::Mint as DomainMint;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!(
            "usage: {} <paper|replay> <config-file> <events-file> [--trade-jsonl PATH] [--config-ledger PATH]",
            args[0]
        );
        return ExitCode::from(2);
    }
    // Optional post-run export sinks (§40/§43: JSONL is SECONDARY — audit and
    // export, never authoritative over the journal digest or chain truth).
    let mut trade_jsonl: Option<String> = None;
    let mut config_ledger: Option<String> = None;
    let mut i = 4;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "--trade-jsonl" => trade_jsonl = Some(args[i + 1].clone()),
            "--config-ledger" => config_ledger = Some(args[i + 1].clone()),
            other => {
                eprintln!("unknown flag '{other}'");
                return ExitCode::from(2);
            }
        }
        i += 2;
    }

    let mode = match args[1].as_str() {
        "paper" => RunMode::Paper,
        "replay" => RunMode::Replay,
        "live" => {
            eprintln!(
                "refused: live capital is Tier-0 human-gated and is not available from this binary"
            );
            return ExitCode::from(3);
        }
        other => {
            eprintln!("unknown mode '{other}' (expected paper|replay)");
            return ExitCode::from(2);
        }
    };

    let cfg_text = match std::fs::read_to_string(&args[2]) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read config {}: {e}", args[2]);
            return ExitCode::from(1);
        }
    };
    let mut cfg = match Config::from_str_over_default(&cfg_text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bad config: {e}");
            return ExitCode::from(1);
        }
    };
    // LAW B6: the strategy-analysis artifact ships beside `live_status.json` at the
    // same conventional location, exactly as that artifact's path is a binary
    // convention rather than a config default. Setting it HERE (not in
    // `Config::dev_portable`) keeps the §19 config identity — and therefore the
    // golden digest — a property of the STRATEGY rather than of where this binary
    // happens to drop telemetry. An operator who names a path in the config keeps
    // theirs.
    if cfg.brain_analysis_path.is_empty() {
        if let Some(p) =
            pump_quant_app::config::CfgPath::from_str_checked("data/brain_analysis.json")
        {
            cfg.brain_analysis_path = p;
        }
    }

    let events_text = match std::fs::read_to_string(&args[3]) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read events {}: {e}", args[3]);
            return ExitCode::from(1);
        }
    };
    let events = match parse_events(&events_text) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("bad events: {e}");
            return ExitCode::from(1);
        }
    };

    let mut engine = Engine::new(cfg, mode);
    // LAW B5: arm the episodic journal when the operator configured a path. The
    // memory is ADVISORY — a store that will not open is reported and the run
    // continues memory-only, because refusing to trade over a missing journal
    // would be a strictly worse failure than trading without recall.
    if cfg.brain_enable && cfg.brain_persist_enable && !cfg.brain_path.is_empty() {
        match engine.attach_brain_store(pump_quant_app::brain::AppBlobStore::File(
            pump_quant_brain::persist::FileBlobStore,
        )) {
            Ok(report) => println!(
                "brain              restored {} episodes ({} snapshot, {} journal){}",
                report.admitted(),
                report.snapshot_admitted,
                report.journal_admitted,
                if report.saw_damage() {
                    " [DAMAGE SEEN — see corrupt/truncated counters]"
                } else {
                    ""
                }
            ),
            Err(e) => eprintln!(
                "brain persistence disarmed: cannot open {} ({e})",
                cfg.brain_path.as_str()
            ),
        }
    }
    // §60/§62 LAW 21: drive the engine loop, emitting the canonical
    // `data/live_status.json` artifact periodically (best-effort; a status-write
    // failure never aborts the run).
    let status_path = std::path::Path::new("data/live_status.json");
    let (report, _status_writes) = engine.run_with_status(&events, status_path, 64);

    println!("mode              {:?}", engine.mode());
    println!("ticks             {}", report.ticks);
    println!("promoted          {}", report.promoted);
    println!("admitted          {}", report.admitted);
    println!("rejected          {}", report.rejected);
    println!("universe_filtered {}", report.universe_filtered);
    println!("net_lamports      {}", report.net_lamports);
    for (lane, net) in report.per_lane_net {
        println!("  lane {lane:?}: net {net}");
    }
    for (lane, w) in report.final_weights {
        println!("  weight {lane:?}: {w} bp");
    }
    println!("journal_digest    {:#018x}", report.journal_digest);
    // LAW B6: say where the strategy-analysis artifact went, and what it is
    // currently nominating for the §56 review (report-only — a nomination retires
    // nothing; see `brain_analysis` and `pump_quant_governance::retirement_review`).
    if cfg.brain_analysis_enable && !cfg.brain_analysis_path.is_empty() {
        let analysis = engine.brain_analysis();
        println!(
            "brain_analysis    {} ({} setup classes, {} retirement nominations)",
            cfg.brain_analysis_path.as_str(),
            analysis.setup_classes.len(),
            analysis.retirement_flags.len()
        );
        for f in &analysis.retirement_flags {
            println!(
                "  nominate {} {} — {} (n={}, net={})",
                f.subject.name(),
                f.key,
                f.reason,
                f.n,
                f.realized_net_lamports
            );
        }
    }
    // §38 evidence law: every emitted report is labeled with the fill model
    // that produced it. Modes A/B are NOT promotion evidence — say so.
    let readiness = engine.promotion_readiness();
    let identity = engine.strategy_identity();
    println!(
        "evidence          {:?} / {:?} — {}",
        readiness.evidence_status,
        readiness.fill_model,
        if readiness.live_probe_eligible {
            "live-probe eligible"
        } else {
            "NOT promotion evidence"
        }
    );
    println!("blocked_on        {}", readiness.blocked_on);
    println!("strategy_hash     {}", identity.strategy_hash.to_hex());
    println!("config_fnv        {:#018x}", identity.config_fnv);

    // LAWs B1/B2 episodic-memory readouts (report plane; never a decision).
    println!(
        "brain_episodes    {} (recall known {} / unknown {})",
        report.brain_episodes_recorded, report.brain_recall_known, report.brain_recall_unknown
    );
    if report.brain_haircuts_applied > 0 || report.brain_vetoes > 0 {
        println!(
            "brain_reduce_only {} haircuts, {} vetoes",
            report.brain_haircuts_applied, report.brain_vetoes
        );
    }
    for c in &report.brain_setup_classes {
        println!(
            "  setup class sig={:#034x} phase={} meta={} lane={} conc={} n={} median_net={} \
             win_rate={} bp",
            c.signature,
            c.venue_phase_code,
            c.meta_category_id,
            c.discovery_lane_code,
            // §21.7 the parallel stream. `unknown` means the estimate pools every
            // float shape; a band name means it is local to that band.
            pump_quant_brain::concentration::concentration_code_label(c.concentration_code),
            c.n_matched,
            c.median_net_lamports,
            c.win_rate_bp
        );
    }
    for m in &report.brain_meta_state {
        println!(
            "  meta {} saturation={} net={} breadth={} launches={}",
            m.meta_category_id,
            m.saturation_code,
            m.aggregate_net_lamports,
            m.participant_breadth,
            m.episode_count
        );
    }
    for a in &report.brain_author_records {
        println!(
            "  author {} n={} median_net={} win_rate={} bp",
            a.author_id, a.n_markouts, a.median_net_lamports, a.win_rate_bp
        );
    }
    // LAW B5: collapse the journal tail so the next start restores from a snapshot.
    if cfg.brain_enable && cfg.brain_persist_enable && !cfg.brain_path.is_empty() {
        if let Err(e) = engine.snapshot_brain() {
            eprintln!("brain snapshot failed (journal is still intact): {e}");
        }
    }

    // Optional JSONL exports (§40: never authoritative over chain/journal).
    if let Some(path) = trade_jsonl {
        if let Err(e) = write_trade_jsonl(&path, &engine, &report) {
            eprintln!("trade-jsonl export failed: {e}");
            return ExitCode::from(1);
        }
    }
    if let Some(path) = config_ledger {
        if let Err(e) = write_config_ledger(&path, &engine, &report, &readiness) {
            eprintln!("config-ledger export failed: {e}");
            return ExitCode::from(1);
        }
    }

    ExitCode::SUCCESS
}

/// Export the retained decision journal as JSONL (one decision per line).
/// SECONDARY record (§40): the canonical truth is the rolling journal digest;
/// this file exists for audit, supervisor ingestion, and human inspection.
fn write_trade_jsonl(
    path: &str,
    engine: &Engine,
    report: &pump_quant_app::engine::Report,
) -> Result<(), String> {
    use pump_quant_app::journal_log::Decision;
    let mut out = String::new();
    for d in engine.journal().recent() {
        let line = match *d {
            Decision::Promoted { mint, lane, rank } => format!(
                "{{\"t\":\"promoted\",\"mint\":\"{}\",\"lane\":{lane},\"rank\":{rank}}}",
                hex32(&mint)
            ),
            Decision::Admitted {
                mint,
                size_lamports,
                x_min,
                x_cost,
                x_max,
                fail_rate_bps,
                rt_cost_bps,
            } => format!(
                "{{\"t\":\"admitted\",\"mint\":\"{}\",\"size_lamports\":{size_lamports},\"x_min\":{x_min},\"x_cost\":{x_cost},\"x_max\":{x_max},\"fail_rate_bps\":{fail_rate_bps},\"rt_cost_bps\":{rt_cost_bps}}}",
                hex32(&mint)
            ),
            Decision::Rejected { mint, reason } => format!(
                "{{\"t\":\"rejected\",\"mint\":\"{}\",\"reason\":{reason}}}",
                hex32(&mint)
            ),
            Decision::Filled {
                mint,
                net_pnl_lamports,
                reason,
            } => format!(
                "{{\"t\":\"filled\",\"mint\":\"{}\",\"net_pnl_lamports\":{net_pnl_lamports},\"reason\":{reason}}}",
                hex32(&mint)
            ),
            Decision::Reweighted {
                lane,
                before_bp,
                after_bp,
            } => format!(
                "{{\"t\":\"reweighted\",\"lane\":{lane},\"before_bp\":{before_bp},\"after_bp\":{after_bp}}}"
            ),
            Decision::Probe {
                mint,
                cost_lamports,
                measurement_id,
            } => format!(
                "{{\"t\":\"probe\",\"mint\":\"{}\",\"cost_lamports\":{cost_lamports},\"measurement_id\":{measurement_id}}}",
                hex32(&mint)
            ),
        };
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str(&format!(
        "{{\"t\":\"run_summary\",\"digest\":\"{:#018x}\",\"net_lamports\":{},\"ticks\":{},\"authoritative\":false}}\n",
        report.journal_digest, report.net_lamports, report.ticks
    ));
    std::fs::write(path, out).map_err(|e| e.to_string())
}

/// Append one configuration-identity line (§14.2 config hash logging): the
/// exact identity this run executed under, bound to its journal digest.
fn write_config_ledger(
    path: &str,
    engine: &Engine,
    report: &pump_quant_app::engine::Report,
    readiness: &pump_quant_app::authority::PromotionReadiness,
) -> Result<(), String> {
    let identity = engine.strategy_identity();
    let line = format!(
        "{{\"t\":\"config_identity\",\"strategy_hash\":\"{}\",\"config_fnv\":\"{:#018x}\",\"protocol_registry_fnv\":\"{:#018x}\",\"journal_digest\":\"{:#018x}\",\"evidence\":\"{:?}\",\"fill_model\":\"{:?}\",\"promotion_evidence\":{}}}\n",
        identity.strategy_hash.to_hex(),
        identity.config_fnv,
        identity.protocol_registry_fnv,
        report.journal_digest,
        readiness.evidence_status,
        readiness.fill_model,
        readiness.live_probe_eligible
    );
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(line.as_bytes()).map_err(|e| e.to_string())
}

/// Lowercase hex of a 32-byte id.
fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Parse the dependency-free event format (one event per line; `#` comments).
fn parse_events(text: &str) -> Result<Vec<AppEvent>, String> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        let err = |m: &str| format!("line {}: {m}", i + 1);
        let mint = |s: &str| DomainMint::from_hex(s).map_err(|e| err(&format!("bad mint: {e}")));
        let num = |s: &str| s.parse::<i64>().map_err(|_| err("bad integer"));
        let ev = match f[0] {
            "tick" => AppEvent::Tick,
            "migrate" if f.len() == 3 => AppEvent::Migration {
                mint: mint(f[1])?,
                slot: num(f[2])?.max(0) as u64,
            },
            "trade" if f.len() == 8 => AppEvent::MarketTrade {
                mint: mint(f[1])?,
                price_fp: f[2].parse::<i128>().map_err(|_| err("bad price_fp"))?,
                quote_lamports: num(f[3])?.max(0) as u64,
                liquidity_lamports: num(f[4])?.max(0) as u64,
                signed_base: num(f[5])?,
                buyer_entity: num(f[6])?.max(0) as u64,
                age_slots: num(f[7])?.max(0) as u32,
            },
            "narr" if f.len() == 4 => AppEvent::NarrativeSample {
                mint: mint(f[1])?,
                prior_active: num(f[2])?.max(0) as u64,
                new_mentions: num(f[3])?.max(0) as u64,
            },
            "social" if f.len() == 3 => AppEvent::SocialCall {
                mint: mint(f[1])?,
                source_quality_bp: num(f[2])?.max(0) as u32,
            },
            "wallet" if f.len() == 4 => AppEvent::WalletAction {
                mint: mint(f[1])?,
                followable: num(f[2])? != 0,
                size_lamports: num(f[3])?.max(0) as u64,
            },
            "confirm" if f.len() == 3 => AppEvent::OnchainConfirm {
                mint: mint(f[1])?,
                sellable_depth_lamports: num(f[2])?.max(0) as u64,
            },
            // Factual, on-chain-led category assignment (the classifier ran upstream;
            // the journal carries the resolved integer id, §85).
            "tokenmeta" if f.len() == 6 => AppEvent::TokenMetadata {
                mint: mint(f[1])?,
                category_id: num(f[2])?.max(0) as u64,
                taxonomy_version: num(f[3])?.max(0) as u32,
                creator: num(f[4])?.max(0) as u64,
                slot: num(f[5])?.max(0) as u64,
            },
            "creator_init" if f.len() == 5 => AppEvent::CreatorAction {
                mint: mint(f[1])?,
                kind: CreatorActionKind::Init {
                    initial_tokens: num(f[2])?.max(0) as u64,
                    total_supply: num(f[3])?.max(0) as u64,
                },
                slot: num(f[4])?.max(0) as u64,
            },
            "creator_buy" if f.len() == 5 => AppEvent::CreatorAction {
                mint: mint(f[1])?,
                kind: CreatorActionKind::Buy {
                    tokens: num(f[2])?.max(0) as u64,
                    quote_lamports: num(f[3])?.max(0) as u64,
                },
                slot: num(f[4])?.max(0) as u64,
            },
            "creator_sell" if f.len() == 5 => AppEvent::CreatorAction {
                mint: mint(f[1])?,
                kind: CreatorActionKind::Sell {
                    tokens: num(f[2])?.max(0) as u64,
                    quote_lamports: num(f[3])?.max(0) as u64,
                },
                slot: num(f[4])?.max(0) as u64,
            },
            "creator_link" if f.len() == 5 => AppEvent::CreatorAction {
                mint: mint(f[1])?,
                kind: CreatorActionKind::LinkedBuy {
                    cluster: num(f[2])?.max(0) as u64,
                    tokens: num(f[3])?.max(0) as u64,
                },
                slot: num(f[4])?.max(0) as u64,
            },
            other => return Err(err(&format!("unknown or malformed event '{other}'"))),
        };
        out.push(ev);
    }
    Ok(out)
}
