p='/home/alon/build/mev_bot_consol/rust/crates/pump-quant-junction/src/bin/pq_daemon.rs'
s=open(p).read()
old='''            eprintln!("[pq-daemon] flow-history {fh_file}: {a:?}");
'''
new='''            eprintln!("[pq-daemon] flow-history {fh_file}: {a:?}");
            // READINESS CLASS (reporting only; no refusal or trading behaviour changes here). An intentional cold start is
            // DECLARED (`PQ_FLOW_SEED_SOURCE=cold_start:<label>`) and its zeros mean "none observed in the declared history".
            // A fresh history with no declaration is a MISSING expected bootstrap; an untrusted file is a FAILED one.
            {
                let declared = std::env::var("PQ_FLOW_SEED_SOURCE").unwrap_or_default();
                let class = match &a {
                    pump_quant_app::engine::model_admit::FlowAttach::Fresh if declared.starts_with("cold_start:") => "cold_start_declared_history_limited",
                    pump_quant_app::engine::model_admit::FlowAttach::Fresh => "bootstrap_missing_undeclared",
                    pump_quant_app::engine::model_admit::FlowAttach::Untrusted(_) => "bootstrap_failed_untrusted",
                    pump_quant_app::engine::model_admit::FlowAttach::Restored { complete: true, .. } => "restored_complete",
                    pump_quant_app::engine::model_admit::FlowAttach::Restored { .. } => "restored_with_unavailable_interval",
                };
                if class == "bootstrap_missing_undeclared" {
                    eprintln!("[pq-daemon] ALERT: flow-history has NO checkpoint and NO declared cold start: the expected bootstrap is MISSING. Smart-wallet/co-entry/lookback fields are 'none observed', not 'none exist'.");
                }
                let _ = std::fs::write(
                    "data/flow_readiness.json",
                    format!("{{\\"class\\":\\"{class}\\",\\"seed_source\\":\\"{}\\",\\"resume_clock_declared\\":{},\\"note\\":\\"history-limited classes: zeros mean none observed in this declared history, not none globally\\"}}", declared.replace('"', "'"), std::env::var("PQ_FLOW_RESUME_MS").is_ok()),
                );
            }
'''
assert s.count(old)==1
open(p,'w').write(s.replace(old,new))
