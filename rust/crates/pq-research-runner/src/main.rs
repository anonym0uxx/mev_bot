//! `pq-research-runner` — the §62 `experiment_run` artifact.
//!
//! Replays a *sealed recorded experiment* (a decision/outcome tape, from a path
//! argument or stdin) and emits the §50 ablation report plus the §52 baseline
//! family net-SOL table as one JSON object. The ablation harness in
//! `pump-quant-evaluator` drives an [`AblationReplay`] closure; here that closure
//! is [`RecordedReplay`], which answers each replay from the experiment's
//! recorded outcome table — proving the harness runs end-to-end over sealed data
//! with no live engine.
//!
//! Thin by construction: parsing lives in `tape`, the harness and baselines in
//! the library. Deterministic, integer-only outcome path (§22). Exit `0` on
//! success, `2` on a bad experiment or bad arguments.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::ExitCode;

use pump_quant_evaluator::ablation::{
    run_ablation, AblationReplay, AblationVariant, FeatureFamily, FeatureToggleMask, ReplayOutcome,
};
use pump_quant_evaluator::baseline_family::{run_family, FamilyParams, FeeModel};
use pump_quant_evaluator::evaluator_pin::fnv1a_64;
use pump_quant_evaluator::tape::{parse_jsonl, AblationRecord, Tape};

/// A replay closure backed by the experiment's recorded outcome table. Every
/// answer is looked up from sealed data, so it is a deterministic pure function
/// of its arguments — exactly the contract the harness requires.
struct RecordedReplay {
    /// Per-`(family, variant)` recorded outcome.
    table: BTreeMap<(u32, AblationVariant), ReplayOutcome>,
    /// The recorded all-on combined baseline outcome.
    combined: ReplayOutcome,
}

impl RecordedReplay {
    fn from_records(records: &[AblationRecord]) -> Self {
        let mut table = BTreeMap::new();
        let mut combined = ReplayOutcome::default();
        for r in records {
            let outcome = ReplayOutcome::new(r.net_lamports, r.right_tail_bps);
            if r.variant == AblationVariant::Combined {
                combined = outcome;
            } else {
                table.insert((r.family.0, r.variant), outcome);
            }
        }
        RecordedReplay { table, combined }
    }

    /// The distinct non-combined families in the recorded table, ascending.
    fn families(&self) -> Vec<FeatureFamily> {
        let mut fams: Vec<u32> = self.table.keys().map(|(f, _)| *f).collect();
        fams.sort_unstable();
        fams.dedup();
        fams.into_iter().map(FeatureFamily).collect()
    }

    /// The distinct non-combined variants recorded, in enum order.
    fn variants(&self) -> Vec<AblationVariant> {
        let mut vs: Vec<AblationVariant> = self.table.keys().map(|(_, v)| *v).collect();
        vs.sort_unstable();
        vs.dedup();
        vs
    }
}

impl AblationReplay for RecordedReplay {
    fn replay(
        &self,
        _toggles: FeatureToggleMask,
        variant: AblationVariant,
        family: Option<FeatureFamily>,
    ) -> ReplayOutcome {
        match family {
            None => self.combined,
            Some(f) => self.table.get(&(f.0, variant)).copied().unwrap_or_default(),
        }
    }
}

struct Args {
    path: Option<String>,
    per_entry_fee: u128,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        path: None,
        per_entry_fee: 0,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--per-entry-fee" => {
                let raw = it.next().ok_or("--per-entry-fee needs a value")?;
                a.per_entry_fee = raw
                    .parse()
                    .map_err(|_| format!("--per-entry-fee: '{raw}' is not a number"))?;
            }
            "-h" | "--help" => return Err(usage()),
            other if other.starts_with('-') => {
                return Err(format!("unknown flag '{other}'\n{}", usage()))
            }
            other => a.path = Some(other.to_string()),
        }
    }
    Ok(a)
}

fn usage() -> String {
    "usage: pq-research-runner [EXPERIMENT.jsonl] [--per-entry-fee N]\n\
     reads the sealed experiment from the path argument, or stdin if omitted."
        .to_string()
}

fn read_input(path: &Option<String>) -> Result<String, String> {
    match path {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("cannot read '{p}': {e}")),
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("cannot read stdin: {e}"))?;
            Ok(buf)
        }
    }
}

fn variant_str(v: AblationVariant) -> &'static str {
    match v {
        AblationVariant::Removed => "removed",
        AblationVariant::Alone => "alone",
        AblationVariant::Combined => "combined",
        AblationVariant::Delayed => "delayed",
        AblationVariant::Noised => "noised",
        AblationVariant::Shuffled => "shuffled",
    }
}

fn run(args: &Args, input: &str) -> Result<String, String> {
    let tape: Tape = parse_jsonl(input).map_err(|e| e.to_string())?;

    // §50 ablation harness over the recorded outcome table.
    let replay = RecordedReplay::from_records(&tape.ablation);
    let families = replay.families();
    let variants = replay.variants();
    let report = run_ablation(&replay, &families, &variants);

    // §52 baseline family net-SOL table over the recorded decision events.
    let fee = FeeModel::new(args.per_entry_fee);
    let family = run_family(&tape.baseline_events, &fee, &FamilyParams::default_params());

    let experiment_hash = fnv1a_64(input.as_bytes());

    let mut ablation_json = String::from("[");
    for (idx, r) in report.results.iter().enumerate() {
        if idx > 0 {
            ablation_json.push(',');
        }
        ablation_json.push_str(&format!(
            "{{\"family\":{},\"variant\":\"{}\",\"net_lamports\":{},\"right_tail_bps\":{},\
\"incremental_net_lamports\":{},\"incremental_right_tail_bps\":{}}}",
            r.family.0,
            variant_str(r.variant),
            r.net_lamports,
            r.right_tail_bps,
            r.incremental_net_lamports,
            r.incremental_right_tail_bps,
        ));
    }
    ablation_json.push(']');

    let mut baselines_json = String::from("[");
    for (idx, r) in family.iter().enumerate() {
        if idx > 0 {
            baselines_json.push(',');
        }
        baselines_json.push_str(&format!(
            "{{\"kind\":\"{:?}\",\"entries\":{},\"net_lamports\":{}}}",
            r.kind, r.entries, r.net_lamports
        ));
    }
    baselines_json.push(']');

    let out = format!(
        "{{\
\"experiment_hash\":\"{experiment_hash:016x}\",\
\"baseline\":{{\"net_lamports\":{bnet},\"right_tail_bps\":{btail}}},\
\"families\":{n_families},\
\"ablation\":{ablation_json},\
\"baselines\":{baselines_json}\
}}",
        bnet = report.baseline.net_lamports,
        btail = report.baseline.right_tail_bps,
        n_families = families.len(),
    );

    Ok(out)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let input = match read_input(&args.path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    match run(&args, &input) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
