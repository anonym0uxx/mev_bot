//! M9 — management-magnitude parity between the CORPUS (the label authority) and
//! the serving seam.
//!
//! The corpus sizes the two management families off DIFFERENT bases:
//!
//! * `ADD`    — the ACCOUNT's capital, capped by free cash
//! * `REDUCE` — the CURRENT position, 50%
//! * `EXIT`   — the CURRENT position, all of it
//!
//! Serving `ADD` off inventory is a ~4x error on the highest-stakes action: the
//! policy is graded on one bet and executes another, and no size can be learned.
//! These fixtures are **not hand-written** — `parity_rust/gen_mgmt_parity_fixtures.py`
//! drives the real simulator (`score_action_sequence` in continuation mode) and
//! records the clip IT executed. Regenerate with:
//!
//! ```text
//! /home/alon/qwen27b-venv/bin/python parity_rust/gen_mgmt_parity_fixtures.py
//! ```

use pump_quant_inference::seam::{
    management_base, management_fraction_bps, resolve_management_clip_lamports, ManagementBase,
};
use pump_quant_inference::Action;

#[derive(Debug, serde::Deserialize)]
struct Row {
    action: String,
    #[serde(default)]
    case: Option<String>,
    inventory_value_lamports: u64,
    account_capital_lamports: u64,
    free_cash_lamports: u64,
    python_clip_lamports: u64,
}

fn rows() -> Vec<Row> {
    include_str!("fixtures/management_parity.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("fixture row must parse"))
        .collect()
}

fn action_of(name: &str) -> Action {
    match name {
        "BUY" => Action::Buy,
        "WATCH" => Action::Watch,
        "SKIP" => Action::Skip,
        "HOLD" => Action::Hold,
        "ADD" => Action::Add,
        "REDUCE" => Action::Reduce,
        "EXIT" => Action::Exit,
        other => panic!("unknown action in fixture: {other}"),
    }
}

/// The control: the fixtures must actually exercise both branches of the corpus
/// `min()`. A generator that silently produced one shape would make the equality
/// below pass while testing nothing.
#[test]
fn the_fixture_exercises_both_branches_of_the_corpus_min() {
    let rs = rows();
    assert!(rs.len() >= 3, "expected at least 3 fixture rows, got {}", rs.len());
    let capital_binds = rs
        .iter()
        .any(|r| r.inventory_value_lamports != r.python_clip_lamports
            && r.python_clip_lamports < r.free_cash_lamports);
    let cash_binds = rs.iter().any(|r| r.python_clip_lamports == r.free_cash_lamports);
    assert!(capital_binds, "no row is capital-limited: {rs:#?}");
    assert!(cash_binds, "no row is cash-limited: {rs:#?}");
}

/// THE PARITY: for every corpus row, the seam resolves the clip the simulator
/// actually executed.
#[test]
fn the_seam_reproduces_every_corpus_clip() {
    let rs = rows();
    assert!(!rs.is_empty());
    for r in &rs {
        let got = resolve_management_clip_lamports(
            action_of(&r.action),
            r.inventory_value_lamports,
            r.account_capital_lamports,
            r.free_cash_lamports,
        );
        assert_eq!(
            got,
            Some(r.python_clip_lamports),
            "seam disagrees with the corpus for {:?} (case {:?}): \
             inventory={} capital={} cash={}",
            r.action,
            r.case,
            r.inventory_value_lamports,
            r.account_capital_lamports,
            r.free_cash_lamports
        );
    }
}

/// The defect the parity exists to prevent: sizing `ADD` off INVENTORY gives a
/// different (smaller) number on every row, and exactly 4.04x smaller on the
/// documented worked example. If this ever stops differing, the fixtures have
/// stopped discriminating and the test above is vacuous.
#[test]
fn the_inventory_base_would_not_reproduce_the_corpus() {
    let rs = rows();
    let mut differed = 0;
    for r in &rs {
        let Some(bps) = management_fraction_bps(Action::Add) else {
            panic!("ADD must carry a management magnitude");
        };
        let inventory_based = u128::from(r.inventory_value_lamports) * u128::from(bps) / 10_000;
        if inventory_based != u128::from(r.python_clip_lamports) {
            differed += 1;
        }
    }
    assert_eq!(
        differed,
        rs.len(),
        "every row must discriminate the two bases, or this test proves nothing"
    );

    // The documented worked example: 0.25 SOL position, 0.76 cash, 1.01 account.
    let r = rs
        .iter()
        .find(|r| r.account_capital_lamports == 1_010_000_000)
        .expect("the worked-example row must be present");
    let corpus = r.python_clip_lamports;
    let inventory = r.inventory_value_lamports / 2;
    assert_eq!(corpus, 505_000_000, "corpus ADD off account capital");
    assert_eq!(inventory, 125_000_000, "inventory-based ADD");
    assert_eq!(
        corpus / inventory,
        4,
        "the documented ~4.04x schism between the two bases"
    );
}

/// The base is named per action, and non-management actions have none.
#[test]
fn management_base_names_the_unit_per_action() {
    assert_eq!(management_base(Action::Add), Some(ManagementBase::AccountCapital));
    assert_eq!(management_base(Action::Reduce), Some(ManagementBase::Inventory));
    assert_eq!(management_base(Action::Exit), Some(ManagementBase::Inventory));
    for a in [Action::Buy, Action::Watch, Action::Skip, Action::Hold] {
        assert_eq!(management_base(a), None, "{a:?} is not a management magnitude");
        assert_eq!(
            resolve_management_clip_lamports(a, 1, 1, 1),
            None,
            "{a:?} must not resolve to a clip"
        );
    }
}

/// REDUCE/EXIT are fractions of the POSITION — the uncontested half of the rule,
/// pinned so the fix cannot over-reach onto them.
#[test]
fn reduce_and_exit_are_fractions_of_the_position() {
    let inv = 250_000_000u64;
    let cap = 1_010_000_000u64;
    let cash = 760_000_000u64;
    assert_eq!(
        resolve_management_clip_lamports(Action::Reduce, inv, cap, cash),
        Some(125_000_000),
        "REDUCE closes half the position"
    );
    assert_eq!(
        resolve_management_clip_lamports(Action::Exit, inv, cap, cash),
        Some(250_000_000),
        "EXIT closes the whole position"
    );
}

/// Saturating, integer-only, and monotone in capital — so a huge account cannot
/// wrap the clip into a tiny number.
#[test]
fn the_resolver_saturates_and_is_monotone_in_capital() {
    // A maximal account resolves to HALF of it (the 5_000 bp fraction applied to
    // the capital), and must never wrap to a small number.
    let huge = resolve_management_clip_lamports(Action::Add, 0, u64::MAX, u64::MAX)
        .expect("ADD resolves");
    assert_eq!(
        huge,
        u64::MAX / 2,
        "half of a maximal account, saturating without wrap"
    );
    assert!(
        huge > u64::MAX / 4,
        "a clip must never wrap to a small number (got {huge})"
    );
    let low = resolve_management_clip_lamports(Action::Add, 0, 1_000_000_000, u64::MAX).unwrap();
    let high = resolve_management_clip_lamports(Action::Add, 0, 2_000_000_000, u64::MAX).unwrap();
    assert!(high > low, "clip must be monotone in account capital");
    // Free cash caps it.
    assert_eq!(
        resolve_management_clip_lamports(Action::Add, 0, 10_000_000_000, 7),
        Some(7),
        "free cash caps the clip"
    );
}
