//! Funnel on a captured event stream with vs without chain-launch facts, through `Engine::tick`.
//! Chain facts are applied at stream start: HINDSIGHT-labelled (retrieved after the capture).
//! Args: <event_stream.jsonl> <outcomes.jsonl|-> ; prints the sample's per-mint last refusal + funnel.
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_junction::event_stream::read_event_stream;
use pump_quant_junction::launch_bootstrap::{evidence_from_json, to_event};

fn chain_events(path: &str) -> Vec<AppEvent> {
    if path == "-" {
        return Vec::new();
    }
    let text = std::fs::read_to_string(path).expect("outcomes");
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| evidence_from_json(&v["evidence"]))
        .map(|e| to_event(&e))
        .collect()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (events, skipped) = read_event_stream(&a[1]).expect("stream");
    let chain = chain_events(&a[2]);
    let mut e = Engine::new(Config::dev_portable(), RunMode::Replay);
    e.arm_model_mode_without_source_for_test();
    for ev in &chain {
        e.tick(*ev);
    }
    for ev in events {
        e.tick(ev);
    }
    let out = serde_json::json!({
        "events_skipped": skipped,
        "chain_facts": chain.len(),
        "funnel": e.model_funnel(),
        "lane": e.model_lane_report(),
    });
    println!("{out}");
}
