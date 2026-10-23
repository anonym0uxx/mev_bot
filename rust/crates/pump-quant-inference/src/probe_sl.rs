//! `probe_sl_v1` policy contract — the prompt/response contract that the dense entry policy
//! `probe_step78_hf` was trained and scored under (dev506 modeled control). Everything here is
//! copied from the authoritative artifacts, never paraphrased:
//! * system instruction: the single system message of `/training/v2/probe_sl_v1/dev_eval.jsonl`
//!   (913 bytes, sha256 `ab4c70d2…0a1aa`), stored verbatim in `contracts/probe_sl_v1_system_prompt.txt`;
//! * response validator: `q3c/v3a_contract.py::contract` (natural stop, ≤128 generated tokens,
//!   no template markers, every line `KEY: value`, exact field order DECISION[,SIZE],REASON,RISK,
//!   entry actions {BUY, WATCH, SKIP}, sizes {SMALL, FULL}) — reason strings are the scorer's;
//! * decoding: temperature 0, max_tokens 128, enable_thinking=false (gen_eval_v3a.py body).
//!
//! The contract carries NO price limit; a BUY executes under the engine's deterministic bounds.

/// The verbatim system instruction (authoritative artifact copy).
pub const PROBE_SL_V1_SYSTEM_PROMPT: &str = include_str!("../contracts/probe_sl_v1_system_prompt.txt");
/// sha256 of [`PROBE_SL_V1_SYSTEM_PROMPT`] as recorded from the artifact.
pub const PROBE_SL_V1_SYSTEM_PROMPT_SHA256: &str =
    "ab4c70d2f7f9e6b1dfa5c14b381c2b88c844e680f82ef58623ec48232bf0a1aa";
/// Generation cap of the contract (scorer `MAX_TOKENS`).
pub const PROBE_SL_V1_MAX_TOKENS: u32 = 128;
/// `probe_sl_v1_compact`: the server-side stop sequence that ends generation at the complete
/// executable record. The probe_sl_v1 answer is `DECISION[\nSIZE]\nREASON…\nRISK…`; the route reads
/// DECISION and SIZE only (REASON/RISK are journal text), so stopping at `\nREASON` yields the
/// decision-complete record with the same greedy prefix (dev506: 482/482 DECISION+SIZE agreement
/// with the long record; 24 long-record truncations become complete records). Same model, same
/// prompt bytes, same cap; only the response boundary and the validator differ.
pub const PROBE_SL_V1_COMPACT_STOP: &str = "\nREASON";
const MARKERS: [&str; 4] = ["<|im_start|>", "<|im_end|>", "<think>", "</think>"];

/// A valid probe_sl entry answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeSlDecision {
    /// `BUY` | `WATCH` | `SKIP`.
    pub action: ProbeSlAction,
    /// `SMALL` | `FULL`, present iff BUY.
    pub size: Option<ProbeSlSize>,
}

/// Entry actions of the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeSlAction {
    /// Open a position.
    Buy,
    /// Do nothing now; may be asked again.
    Watch,
    /// Do nothing.
    Skip,
}

/// Sizes of the contract (MID is not trained/served).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeSlSize {
    /// 0.25 SOL clip.
    Small,
    /// 1.00 SOL clip.
    Full,
}

impl ProbeSlAction {
    /// Token as it appears in the answer.
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeSlAction::Buy => "BUY",
            ProbeSlAction::Watch => "WATCH",
            ProbeSlAction::Skip => "SKIP",
        }
    }
}

/// Mirror of `v3a_contract.contract(finish_reason, text, "entry", n_generated_tokens)`.
/// `Err` carries the scorer's reason string. `n_generated_tokens` is `None` when the server did
/// not report usage (the cap is then enforced by `finish_reason` alone, as the scorer does).
pub fn validate_probe_sl_entry(
    finish_reason: Option<&str>,
    text: &str,
    n_generated_tokens: Option<u32>,
) -> Result<ProbeSlDecision, &'static str> {
    match finish_reason {
        Some("stop") => {}
        Some(_) => return Err("not_natural_stop"),
        None => return Err("no_finish_reason"),
    }
    if n_generated_tokens.is_some_and(|n| n > PROBE_SL_V1_MAX_TOKENS) {
        return Err("over_token_limit");
    }
    if MARKERS.iter().any(|m| text.contains(m)) {
        return Err("template_marker_in_content");
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop(); // one trailing newline tolerated (template |trim)
    }
    if lines.is_empty() || lines.iter().any(|l| l.trim().is_empty()) {
        return Err("empty_line");
    }
    let mut kv: Vec<(&str, &str)> = Vec::with_capacity(lines.len());
    for l in &lines {
        let Some((k, v)) = l.split_once(": ") else {
            return Err("line_without_field");
        };
        if v.trim().is_empty() {
            // scorer: f"empty_value:{k}" — the key is reported separately by the caller.
            return Err("empty_value");
        }
        kv.push((k, v));
    }
    let keys: Vec<&str> = kv.iter().map(|(k, _)| *k).collect();
    if keys[0] != "DECISION" {
        return Err("first_line_not_decision");
    }
    let action = match kv[0].1 {
        "BUY" => ProbeSlAction::Buy,
        "WATCH" => ProbeSlAction::Watch,
        "SKIP" => ProbeSlAction::Skip,
        _ => return Err("untrained_action"),
    };
    let want: &[&str] = if action == ProbeSlAction::Buy {
        &["DECISION", "SIZE", "REASON", "RISK"]
    } else {
        &["DECISION", "REASON", "RISK"]
    };
    if keys != want {
        if action != ProbeSlAction::Buy && keys.contains(&"SIZE") {
            return Err("size_on_non_buy");
        }
        if action == ProbeSlAction::Buy && !keys.contains(&"SIZE") {
            return Err("buy_without_size");
        }
        if keys.iter().any(|k| keys.iter().filter(|x| *x == k).count() > 1) {
            return Err("repeated_field");
        }
        if keys.iter().any(|k| !["DECISION", "SIZE", "REASON", "RISK"].contains(k)) {
            return Err("unknown_field");
        }
        return Err("field_order_or_missing");
    }
    let size = if action == ProbeSlAction::Buy {
        Some(match kv[1].1 {
            "SMALL" => ProbeSlSize::Small,
            "FULL" => ProbeSlSize::Full,
            _ => return Err("buy_with_untrained_size"),
        })
    } else {
        None
    };
    Ok(ProbeSlDecision { action, size })
}

/// Versioned policy-contract binding: what a served policy is allowed to be asked and how its
/// answer is read. One per adapter/policy; nothing is shared by accident.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PolicyContractBinding {
    /// Contract id, e.g. `probe_sl_v1`.
    pub contract_id: String,
    /// Served model/adapter id addressed in `"model"`.
    pub model_id: String,
    /// Root artifact the server reports for that id.
    pub model_root: Option<String>,
    /// sha256 of the system instruction sent.
    pub system_prompt_sha256: String,
    /// Prompt renderer identity (user prompt).
    pub prompt_renderer: String,
    /// Feature schema identity.
    pub feature_schema: String,
    /// Chat template / tokenizer identity (server-side; recorded, not re-implemented).
    pub chat_template: String,
    /// Response parser identity.
    pub response_parser: String,
    /// Decoding fields of the request body.
    pub temperature: f64,
    /// Generation cap.
    pub max_tokens: u32,
    /// `chat_template_kwargs.enable_thinking`.
    pub enable_thinking: bool,
}

impl PolicyContractBinding {
    /// The probe_sl_v1 binding for a served base id.
    pub fn probe_sl_v1(model_id: &str, model_root: Option<&str>) -> Self {
        PolicyContractBinding {
            contract_id: "probe_sl_v1".into(),
            model_id: model_id.into(),
            model_root: model_root.map(str::to_string),
            system_prompt_sha256: PROBE_SL_V1_SYSTEM_PROMPT_SHA256.into(),
            prompt_renderer: "pump-quant decision_join renderer (byte-parity with /training/v2/probe_sl_v1 user prompts)".into(),
            feature_schema: "v2 causal state snapshot + ENRICHED CANDIDATE STATE + CURVE/AMM/PRICE UNITS blocks".into(),
            chat_template: "server-side Qwen3 chat template of the served base (vLLM applies; enable_thinking=false)".into(),
            response_parser: "q3c/v3a_contract.py::contract mirror (validate_probe_sl_entry)".into(),
            temperature: 0.0,
            max_tokens: PROBE_SL_V1_MAX_TOKENS,
            enable_thinking: false,
        }
    }

    /// The probe_sl_v1_compact binding: identical model/prompt/cap/decoding; response boundary
    /// `stop=["\nREASON"]` and the compact validator.
    pub fn probe_sl_v1_compact(model_id: &str, model_root: Option<&str>) -> Self {
        let mut b = Self::probe_sl_v1(model_id, model_root);
        b.contract_id = "probe_sl_v1_compact".into();
        b.response_parser = format!(
            "validate_probe_sl_entry_compact (server stop {:?}; finish=stop AND stop_reason==stop; exact DECISION[/SIZE] record)",
            PROBE_SL_V1_COMPACT_STOP
        );
        b
    }
}

/// `probe_sl_v1_compact` validator. VALID iff the server reports `finish_reason="stop"` AND
/// `stop_reason == PROBE_SL_V1_COMPACT_STOP` (the record boundary was reached by the server — not
/// EOS, not the token cap) AND the text is exactly `DECISION: <BUY|WATCH|SKIP>` optionally followed
/// by `\nSIZE: <SMALL|FULL>`, with SIZE present iff BUY. No trailing newline, no blank line, no other
/// field: a prefix the client happened to read, a length-cap cut, or an EOS-ended short answer is
/// NOT a valid compact record. Returns the same decision type as the long validator.
pub fn validate_probe_sl_entry_compact(
    finish_reason: Option<&str>,
    stop_reason: Option<&str>,
    text: &str,
    n_generated_tokens: Option<u32>,
) -> Result<ProbeSlDecision, &'static str> {
    match finish_reason {
        Some("stop") => {}
        Some(_) => return Err("not_natural_stop"),
        None => return Err("no_finish_reason"),
    }
    if stop_reason != Some(PROBE_SL_V1_COMPACT_STOP) {
        return Err("not_at_record_boundary");
    }
    if n_generated_tokens.is_some_and(|n| n > PROBE_SL_V1_MAX_TOKENS) {
        return Err("over_token_limit");
    }
    if MARKERS.iter().any(|m| text.contains(m)) {
        return Err("template_marker_in_content");
    }
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.is_empty() || lines.iter().any(|l| l.trim().is_empty()) {
        return Err("empty_line");
    }
    let Some(d) = lines[0].strip_prefix("DECISION: ") else {
        return Err("first_line_not_decision");
    };
    let action = match d {
        "BUY" => ProbeSlAction::Buy,
        "WATCH" => ProbeSlAction::Watch,
        "SKIP" => ProbeSlAction::Skip,
        _ => return Err("untrained_action"),
    };
    match (action, lines.len()) {
        (ProbeSlAction::Buy, 2) => {
            let Some(sz) = lines[1].strip_prefix("SIZE: ") else {
                return Err("buy_without_size");
            };
            let size = match sz {
                "SMALL" => ProbeSlSize::Small,
                "FULL" => ProbeSlSize::Full,
                _ => return Err("buy_with_untrained_size"),
            };
            Ok(ProbeSlDecision { action, size: Some(size) })
        }
        (ProbeSlAction::Buy, 1) => Err("buy_without_size"),
        (_, 1) => Ok(ProbeSlDecision { action, size: None }),
        (_, 2) if lines[1].starts_with("SIZE: ") => Err("size_on_non_buy"),
        _ => Err("field_order_or_missing"),
    }
}

/// Recompute the system prompt digest (startup self-check against the recorded constant).
pub fn system_prompt_sha256() -> String {
    pump_quant_protocol::sha256::to_hex(&pump_quant_protocol::sha256::sha256(PROBE_SL_V1_SYSTEM_PROMPT.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_validator_accepts_only_server_terminated_exact_records() {
        let stop = Some(PROBE_SL_V1_COMPACT_STOP);
        let ok = |t: &str| validate_probe_sl_entry_compact(Some("stop"), stop, t, Some(7));
        assert_eq!(ok("DECISION: SKIP").unwrap(), ProbeSlDecision { action: ProbeSlAction::Skip, size: None });
        assert_eq!(ok("DECISION: BUY\nSIZE: FULL").unwrap(), ProbeSlDecision { action: ProbeSlAction::Buy, size: Some(ProbeSlSize::Full) });
        assert_eq!(ok("DECISION: BUY\nSIZE: SMALL").unwrap().size, Some(ProbeSlSize::Small));
        // boundary not reached by the server: EOS-ended short answer, length cap, missing finish
        assert_eq!(validate_probe_sl_entry_compact(Some("stop"), None, "DECISION: SKIP", Some(7)), Err("not_at_record_boundary"));
        assert_eq!(validate_probe_sl_entry_compact(Some("length"), stop, "DECISION: SKIP", Some(128)), Err("not_natural_stop"));
        assert_eq!(validate_probe_sl_entry_compact(None, stop, "DECISION: SKIP", None), Err("no_finish_reason"));
        assert_eq!(validate_probe_sl_entry_compact(Some("stop"), Some("\nRISK"), "DECISION: SKIP", None), Err("not_at_record_boundary"));
        // malformed records
        assert_eq!(ok(""), Err("empty_line"));
        assert_eq!(ok("\nDECISION: SKIP"), Err("empty_line"));
        assert_eq!(ok("DECISION: SKIP\n"), Err("empty_line"));
        assert_eq!(ok(" DECISION: SKIP"), Err("first_line_not_decision"));
        assert_eq!(ok("DECISION: HOLD"), Err("untrained_action"));
        assert_eq!(ok("DECISION: BUY"), Err("buy_without_size"));
        assert_eq!(ok("DECISION: BUY\nSIZE: MID"), Err("buy_with_untrained_size"));
        assert_eq!(ok("DECISION: SKIP\nSIZE: FULL"), Err("size_on_non_buy"));
        assert_eq!(ok("DECISION: SKIP\nRISK: x"), Err("field_order_or_missing"));
        assert_eq!(ok("DECISION: BUY\nSIZE: FULL\nREASON: x"), Err("field_order_or_missing"));
        assert_eq!(ok("DECISION: BUY<|im_end|>\nSIZE: FULL"), Err("template_marker_in_content"));
        assert_eq!(validate_probe_sl_entry_compact(Some("stop"), stop, "DECISION: SKIP", Some(129)), Err("over_token_limit"));
    }

    #[test]
    fn compact_binding_shares_identity_and_differs_only_in_parser_and_id() {
        let a = PolicyContractBinding::probe_sl_v1("m", None);
        let b = PolicyContractBinding::probe_sl_v1_compact("m", None);
        assert_eq!(b.contract_id, "probe_sl_v1_compact");
        assert_eq!((a.system_prompt_sha256, a.max_tokens, a.temperature, a.enable_thinking), (b.system_prompt_sha256.clone(), b.max_tokens, b.temperature, b.enable_thinking));
        assert!(b.response_parser.contains("\\nREASON"));
    }

    #[test]
    fn system_prompt_is_the_artifact_byte_for_byte() {
        assert_eq!(PROBE_SL_V1_SYSTEM_PROMPT.len(), 913);
        assert_eq!(system_prompt_sha256(), PROBE_SL_V1_SYSTEM_PROMPT_SHA256);
        assert!(PROBE_SL_V1_SYSTEM_PROMPT.ends_with("RISK (the main counter-fact)."));
        assert!(!PROBE_SL_V1_SYSTEM_PROMPT.contains("PRICE LIMIT"));
    }

    #[test]
    fn validator_mirrors_the_scorer() {
        let ok = |t: &str| validate_probe_sl_entry(Some("stop"), t, Some(70));
        assert_eq!(ok("DECISION: SKIP\nREASON: x\nRISK: y"), Ok(ProbeSlDecision { action: ProbeSlAction::Skip, size: None }));
        assert_eq!(ok("DECISION: SKIP\nREASON: x\nRISK: y\n"), Ok(ProbeSlDecision { action: ProbeSlAction::Skip, size: None }));
        assert_eq!(ok("DECISION: BUY\nSIZE: FULL\nREASON: x\nRISK: y"), Ok(ProbeSlDecision { action: ProbeSlAction::Buy, size: Some(ProbeSlSize::Full) }));
        assert_eq!(ok("DECISION: WATCH\nREASON: x\nRISK: y").map(|d| d.action), Ok(ProbeSlAction::Watch));
        assert_eq!(validate_probe_sl_entry(Some("length"), "DECISION: SKIP\nREASON: x\nRISK: y", Some(128)), Err("not_natural_stop"));
        assert_eq!(validate_probe_sl_entry(None, "DECISION: SKIP\nREASON: x\nRISK: y", None), Err("no_finish_reason"));
        assert_eq!(validate_probe_sl_entry(Some("stop"), "DECISION: SKIP\nREASON: x\nRISK: y", Some(129)), Err("over_token_limit"));
        assert_eq!(ok("DECISION: BUY\nREASON: x\nRISK: y"), Err("buy_without_size"));
        assert_eq!(ok("DECISION: BUY\nSIZE: MID\nREASON: x\nRISK: y"), Err("buy_with_untrained_size"));
        assert_eq!(ok("DECISION: SKIP\nSIZE: NONE\nREASON: x\nRISK: y"), Err("size_on_non_buy"));
        assert_eq!(ok("DECISION: SKIP\nREASON: x\nRISK: y\nEVIDENCE_STATUS: complete"), Err("unknown_field"));
        assert_eq!(ok("DECISION: SKIP\nRISK: y\nREASON: x"), Err("field_order_or_missing"));
        assert_eq!(ok("DECISION: SKIP\n\nREASON: x\nRISK: y"), Err("empty_line"));
        assert_eq!(ok("DECISION: SKIP\nREASON x\nRISK: y"), Err("line_without_field"));
        assert_eq!(ok("REASON: x\nDECISION: SKIP\nRISK: y"), Err("first_line_not_decision"));
        assert_eq!(ok("DECISION: SELL\nREASON: x\nRISK: y"), Err("untrained_action"));
        assert_eq!(ok("DECISION: SKIP\nREASON: x\nRISK: y<|im_end|>"), Err("template_marker_in_content"));
        // the live-renderer (v16) answer shape is off-contract here
        assert!(ok("DECISION: SKIP\nSIZE: NONE\nINVALIDATION: n\nEVIDENCE: e\nCOUNTEREVIDENCE: c\nEVIDENCE_STATUS: complete").is_err());
    }
}
