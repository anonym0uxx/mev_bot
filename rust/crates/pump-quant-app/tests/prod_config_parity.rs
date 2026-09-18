//! The production / fixture split for the honest-fill and concentration knobs.
//!
//! Both of these were INVERTED between the shipped operator config and the compiled
//! `dev_portable()` base:
//!
//! * `curve_exact_fill_enable` was `true` in `dev_portable()` (a profile with no depth
//!   model, where charging own-impact against stylized depth fabricates nonsense) and
//!   `0` in `data/CHAMPION_CONFIG.txt` — so PRODUCTION ran phantom fills while a
//!   unit-test profile ran honest ones. Every champion-selection / PnL number ranked
//!   in production was optimistic (R7).
//! * `holder_concentration_enable` is `1` in the shipped config (the concentration law
//!   is armed in production) but the fixtures measure an UNDEFENDED hazard.
//!
//! Neither inversion is visible to a compile error, which is exactly why they survived
//! a refactor. These assertions make the split structural: flip either side and CI
//! fails with the intent spelled out.

use pump_quant_app::config::Config;

/// `rust/data/CHAMPION_CONFIG.txt` — the daemon's source of truth (loaded over
/// `dev_portable()` at `pq_daemon.rs`). Resolved from the crate manifest, because the
/// daemon reads it cwd-relative and a test must not depend on the cwd.
///
/// `rust/data/` is GITIGNORED: this is operator-managed RUNTIME config, not source.
/// A fresh clone legitimately does not have it, so its absence SKIPS the shipped-side
/// assertions instead of failing — the fixture-side invariant below is in code and
/// always runs. When the file IS present (any real deployment), the shipped side is
/// asserted too, and that is where the R7 inversion actually lived.
fn shipped_config() -> Option<Config> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/CHAMPION_CONFIG.txt");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            eprintln!(
                "[prod_config_parity] {path:?} absent (gitignored runtime config) — \
                 skipping the shipped-side assertions; the fixture-side invariant still runs"
            );
            return None;
        }
    };
    Some(
        Config::from_str_over_default(&text)
            .expect("the shipped operator config must parse over dev_portable()"),
    )
}

#[test]
fn the_fixture_base_disarms_the_honest_fill_and_production_arms_it() {
    // Fixture/portable base: no depth model, so the fill must stay disarmed. The
    // regression tapes build on this; a tape that declares real depth arms it itself.
    assert!(
        !Config::dev_portable().curve_exact_fill_enable,
        "dev_portable() must ship curve_exact_fill_enable = false — it has no depth \
         model, and arming it against stylized depth fabricates fills"
    );

    // Production: the operator config is the source of truth and MUST arm it, or every
    // paper PnL the daemon ranks is phantom (R7).
    if let Some(shipped) = shipped_config() {
        assert!(
            shipped.curve_exact_fill_enable,
            "data/CHAMPION_CONFIG.txt must set curve_exact_fill_enable = 1: production \
             has the market's real liquidity, and paper fills that omit own-impact are \
             phantom"
        );
    }
}

#[test]
fn the_shipped_config_arms_the_concentration_law_the_fixture_leaves_open() {
    // The concentration law is ARMED in production. The hazard-tape fixtures measure
    // the UNDEFENDED hazard deliberately (they are relative instruments), and
    // `entry_exit_frontier` asserts how large that undefended loss is allowed to get.
    // If production ever ships it disarmed, that assertion silently stops describing
    // anything that runs.
    let Some(shipped) = shipped_config() else {
        return;
    };
    assert!(
        shipped.holder_concentration_enable,
        "data/CHAMPION_CONFIG.txt must arm holder_concentration_enable: the \
         concentration-hazard tape exists precisely because production defends it"
    );
}
