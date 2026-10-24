//! Research arms (EXPERIMENT_SPEC_ABSORPTION): several ISOLATED simulated books in one daemon,
//! sharing the data feed and — where rendered prompt bytes are identical — one memoised inference.
//!
//! Each arm is a complete [`Engine`] in `RunMode::Paper` with its own orders, reservations, cash,
//! held positions, loss budget, protection state, safety latch, model-I/O journal and durable
//! held ledger under `data/arms/<id>/`. The primary daemon engine (arm A, unchanged Qwen) is NOT
//! in this list; it keeps its historical files. Arms receive every event the primary receives,
//! BEFORE any arm-dependent capital/held-mint veto is applied (each engine applies its own), so
//! A's depleted account cannot suppress B/C opportunities. Live order authority is impossible
//! here by construction: arms are only built when the daemon is not in live mode and are always
//! `RunMode::Paper`.
//!
//! Arm kinds:
//! - `veto:<r_star_bps>`  — arm B: Qwen (shared inference) + declared absorption admission veto.
//! - `rule:<r_star_bps>`  — arm C: transparent no-LLM rule (`absorption::RuleSource`), SMALL only.

use pump_quant_app::absorption::RuleSource;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::model_authority::ModelSource;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmKind {
    Veto { r_star_bps: u64 },
    Rule { r_star_bps: u64 },
}

pub struct Arm {
    pub id: String,
    pub kind: ArmKind,
    pub dir: PathBuf,
    pub engine: Engine,
}

#[derive(Default)]
pub struct Arms {
    pub arms: Vec<Arm>,
}

/// Parse `PQ_RESEARCH_ARMS="B:veto:15000;C:rule:15000"`. Unknown kinds are an error (refuse to
/// arm rather than silently run a different experiment). Ids must be unique and path-safe.
pub fn parse_spec(spec: &str) -> Result<Vec<(String, ArmKind)>, String> {
    let mut out = Vec::new();
    for part in spec.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let f: Vec<&str> = part.split(':').collect();
        if f.len() != 3 {
            return Err(format!("arm spec `{part}`: expected <id>:<veto|rule>:<r_star_bps>"));
        }
        let id = f[0].to_string();
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(format!("arm id `{id}` is not path-safe"));
        }
        if out.iter().any(|(i, _)| *i == id) {
            return Err(format!("duplicate arm id `{id}`"));
        }
        let r: u64 = f[2].parse().map_err(|_| format!("arm `{id}`: r_star_bps `{}` not a u64", f[2]))?;
        let kind = match f[1] {
            "veto" => ArmKind::Veto { r_star_bps: r },
            "rule" => ArmKind::Rule { r_star_bps: r },
            other => return Err(format!("arm `{id}`: unknown kind `{other}`")),
        };
        out.push((id, kind));
    }
    Ok(out)
}

/// What the primary engine was configured with, replicated verbatim into each arm so the only
/// declared difference is the arm kind.
pub struct ArmTemplate {
    pub cfg: pump_quant_app::config::Config,
    pub entry_contract: Option<pump_quant_app::engine::EntryContract>,
    pub management_policy: Option<pump_quant_app::engine::ManagementPolicy>,
    pub loss_budget_lamports: Option<u64>,
    pub cancel_lag_ms: Option<u64>,
}

impl Arms {
    /// Build the arms. `shared_qwen` is the SAME memoised source the primary engine uses (identical
    /// rendered prompt bytes are answered once). Journals/ledgers live under `<data_root>/arms/<id>/`.
    pub fn build(
        specs: &[(String, ArmKind)],
        tpl: &ArmTemplate,
        shared_qwen: Option<Arc<dyn ModelSource + Send + Sync>>,
        data_root: &Path,
        live_mode: bool,
    ) -> Result<Self, String> {
        if live_mode && !specs.is_empty() {
            return Err("research arms are paper-only: refused in live mode".into());
        }
        let mut arms = Vec::new();
        for (id, kind) in specs {
            let dir = data_root.join("arms").join(id);
            std::fs::create_dir_all(&dir).map_err(|e| format!("arm `{id}`: mkdir {}: {e}", dir.display()))?;
            let mut engine = Engine::new(tpl.cfg.clone(), RunMode::Paper);
            if let Some(c) = tpl.entry_contract {
                engine.set_entry_contract(c);
            }
            if let Some(p) = tpl.management_policy {
                engine.set_management_policy(p);
            }
            if let Some(b) = tpl.loss_budget_lamports {
                engine.set_model_loss_budget(b, 0);
            }
            if let Some(lag) = tpl.cancel_lag_ms {
                engine.set_model_cancel_lag_ms(Some(lag));
            }
            match kind {
                ArmKind::Veto { r_star_bps } => {
                    let src = shared_qwen.clone().ok_or_else(|| format!("arm `{id}`: veto arm needs the shared Qwen source"))?;
                    engine.set_entry_absorption_veto_bps(Some(*r_star_bps));
                    engine.enable_paper_model_arc(src);
                }
                ArmKind::Rule { r_star_bps } => {
                    let src: Arc<dyn ModelSource + Send + Sync> = Arc::new(RuleSource { r_star_bps: *r_star_bps });
                    engine.enable_paper_model_arc(src);
                }
            }
            // Durable per-arm journal, safety latch and held ledger.
            let io_path = dir.join("model_io.jsonl");
            match std::fs::OpenOptions::new().create(true).append(true).open(&io_path) {
                Ok(mut f) => {
                    use std::io::Write;
                    engine.set_model_io_sink(Box::new(move |line: &str| {
                        let _ = writeln!(f, "{line}");
                    }));
                }
                Err(e) => return Err(format!("arm `{id}`: journal {}: {e}", io_path.display())),
            }
            let _ = engine.model_safety_attach(&dir.join("MODEL_SAFETY.json"));
            match crate::model_lifecycle::restore_held_state(&mut engine, &dir.join("MODEL_HELD.json")) {
                crate::model_lifecycle::StartupRestore::Refused(why) => {
                    return Err(format!("arm `{id}`: held-state restore refused: {why}"));
                }
                _ => {}
            }
            arms.push(Arm { id: id.clone(), kind: kind.clone(), dir, engine });
        }
        Ok(Self { arms })
    }

    pub fn is_empty(&self) -> bool {
        self.arms.is_empty()
    }

    /// Deliver one feed event to every arm (the primary has already received it).
    pub fn tick(&mut self, ev: &AppEvent) {
        for a in &mut self.arms {
            a.engine.tick(ev.clone());
        }
    }

    /// Quiet-pool RPC snapshot: delivered to every arm that holds the mint.
    #[allow(clippy::too_many_arguments)]
    pub fn on_amm_state_snapshot(
        &mut self,
        mint: [u8; 32],
        pool: [u8; 32],
        pool_quote_mint: [u8; 32],
        base_reserves_raw: u64,
        quote_reserves_lamports: u64,
        virtual_quote: i128,
        slot: u64,
        ts_ms: i64,
    ) -> usize {
        self.arms
            .iter_mut()
            .filter(|a| a.engine.model_position_open(&mint))
            .map(|a| {
                usize::from(a.engine.model_on_amm_state_snapshot(
                    mint, pool, pool_quote_mint, base_reserves_raw, quote_reserves_lamports, virtual_quote, slot, ts_ms,
                ))
            })
            .sum()
    }

    /// Union of every arm's exposure (held + pending) — the daemon protects the UNION of all
    /// arms' exposure feeds within the declared reserve.
    pub fn exposure_mints(&self) -> Vec<[u8; 32]> {
        let mut v: Vec<[u8; 32]> = self.arms.iter().flat_map(|a| a.engine.exposure_mints()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    pub fn held_mints(&self) -> Vec<[u8; 32]> {
        let mut v: Vec<[u8; 32]> = self.arms.iter().flat_map(|a| a.engine.model_held_mints()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    pub fn set_entries_blocked(&mut self, blocked: bool) {
        for a in &mut self.arms {
            a.engine.set_model_entries_blocked(blocked);
        }
    }

    pub fn set_entry_paused(&mut self, paused: bool) {
        for a in &mut self.arms {
            a.engine.set_entry_paused(paused);
        }
    }

    pub fn persist_held(&mut self) {
        for a in &mut self.arms {
            let _ = a.engine.model_held_persist_now();
        }
    }

    /// Per-arm status line for the operator log / desk.
    pub fn status_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.arms
                .iter()
                .map(|a| {
                    serde_json::json!({
                        "id": a.id, "kind": format!("{:?}", a.kind), "dir": a.dir.display().to_string(),
                        "held": a.engine.model_held_mints().len(), "pending": a.engine.model_pending_orders(),
                        "bankroll_balance_lamports": a.engine.bankroll_balance(),
                        "lane_report": a.engine.model_lane_report(),
                    })
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spec_parsing_is_strict() {
        let v = parse_spec("B:veto:15000; C:rule:15000").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].1, ArmKind::Veto { r_star_bps: 15_000 });
        assert!(parse_spec("B:veto").is_err());
        assert!(parse_spec("B:magic:1").is_err());
        assert!(parse_spec("B:veto:1;B:rule:1").is_err());
        assert!(parse_spec("../x:veto:1").is_err());
        assert!(parse_spec("").unwrap().is_empty());
    }
}
