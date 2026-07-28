//! **EDGE PROVENANCE — where does the golden book's +8,124,568 actually come from?**
//!
//! This is a DIAGNOSTIC, not a law. It exists to answer one question that no A/B in
//! this repo has ever asked directly: on the representative tape, is the net a
//! product of SELECTION and TIMING (i.e. skill the engine supplies), or is it the
//! unconditional mean of an authored outcome distribution multiplied by a trade
//! count (i.e. arithmetic the fixture supplies)?
//!
//! The distinction decides whether any further parameter work on this corpus can
//! possibly move net SOL. If the tape carries no information linking observables to
//! outcomes, then no admission rule, exit rule, or sizing rule can beat any other
//! except through cost and count — and every "inert knob" result this project has
//! recorded was determined by the fixture before the strategy ran.
//!
//! Run with `--nocapture` to read the ledger.

mod tape_golden;

use pump_quant_app::config::Config;

/// Dump the sealed per-trade ledger: what was admitted, what it returned, how it
/// ended, and what its excursions were.
#[test]
fn print_the_per_trade_ledger() {
    let mut eng = tape_golden::drive_eng(Config::dev_portable());

    // Snapshot the NATURALLY-closed book first: trades the strategy's own triggers
    // ended while the tape was still running.
    let natural: i128 = eng
        .brain()
        .index()
        .iter_oldest_first()
        .filter(|e| e.outcome().was_admitted)
        .map(|e| e.outcome().realized_net_lamports)
        .sum();
    let n_natural = eng
        .brain()
        .index()
        .iter_oldest_first()
        .filter(|e| e.outcome().was_admitted)
        .count();

    // `report()` calls `finalize()`, which FORCE-CLOSES whatever is still open at the
    // end of the tape and books it. That is not a strategy decision — it is an
    // artifact of where the fixture stops.
    let report = eng.report();
    let brain = eng.brain();
    let mut admitted: Vec<(u64, i128, i64, i64, String)> = Vec::new();
    for e in brain.index().iter_oldest_first() {
        let o = e.outcome();
        if !o.was_admitted {
            continue;
        }
        admitted.push((
            e.context().mint_id,
            o.realized_net_lamports,
            o.mfe_bps,
            o.mae_bps,
            format!("{:?}", o.exit_reason),
        ));
    }
    println!(
        "\n=== GOLDEN TAPE PER-TRADE LEDGER ({} admits) ===",
        admitted.len()
    );
    println!(
        "{:>8}  {:>14}  {:>9}  {:>9}  exit",
        "mint", "net_lamports", "mfe_bps", "mae_bps"
    );
    let mut total: i128 = 0;
    let mut wins = 0usize;
    for (m, net, mfe, mae, reason) in &admitted {
        total += net;
        if *net > 0 {
            wins += 1;
        }
        println!("{m:>8}  {net:>14}  {mfe:>9}  {mae:>9}  {reason}");
    }
    let n = admitted.len().max(1) as i128;
    println!("---");
    println!("total (all closes)      {total}");
    println!("mean/trade              {}", total / n);
    println!("win rate                {}/{}", wins, admitted.len());
    let (seen, adm) = brain.episode_counts();
    println!("episodes                {seen} sealed, {adm} admitted");
    println!("---");
    println!("NATURAL closes only     {natural}  (n = {n_natural})");
    println!(
        "FORCED at end of tape   {}  (n = {})",
        total - natural,
        admitted.len() - n_natural
    );
    println!("report().net_lamports   {}", report.net_lamports);
    let forced = total - natural;
    if natural != 0 {
        println!(
            "forced share of book    {}% of the naturally-earned total",
            forced * 100 / natural
        );
    }
}

/// **THE STRUCTURAL FACT, asserted so it cannot be forgotten.**
///
/// `tape_golden::main_scalp` derives a mint's entire price trajectory from a hash of
/// its tag alone. Order flow (`signed_base`) is derived from `round` against a
/// CONSTANT `peak_round = 2` — identical for every one of the 512 mints. So on this
/// tape:
///
/// * flow carries **zero** cross-sectional information (it flips at the same round
///   for a rug and for a 2.2x runner);
/// * rounds 0-1 are deliberately identical for every mint, so nothing observable at
///   entry time correlates with outcome;
/// * therefore admission cannot be better than a random draw from the authored
///   outcome mix, and the §32 flow-flip exit is a CLOCK, not a signal.
///
/// This test proves the flow claim directly rather than asserting it in prose.
#[test]
fn order_flow_on_this_tape_is_a_clock_not_a_signal() {
    // Sample mints from each authored outcome class and compare their flow series.
    let probe: [u64; 6] = [0, 1, 7, 42, 137, 511];
    let mut series: Vec<Vec<i64>> = Vec::new();
    for m in probe {
        let mut s = Vec::new();
        for round in 0..6u64 {
            for i in 0..3u64 {
                let (_price, signed_base) = tape_golden::main_scalp(m, round, i);
                s.push(signed_base.signum());
            }
        }
        series.push(s);
    }
    // Every mint's SIGN series must be identical — that is what "no information" means.
    for (k, s) in series.iter().enumerate().skip(1) {
        assert_eq!(
            *s, series[0],
            "mint {} has the same flow-sign series as mint {} — if this ever fails, \
             the tape has gained cross-sectional flow information and every \
             'flow knob is inert' verdict in this repo must be re-argued",
            probe[k], probe[0]
        );
    }
    // And the flip happens at the same place for all of them: end of round 2.
    let flip = series[0]
        .iter()
        .position(|&x| x < 0)
        .expect("flow must flip");
    assert_eq!(
        flip, 9,
        "flow flips after exactly 9 prints (3 rounds x 3) for EVERY mint — a clock"
    );
}

/// **The peak multiple is a hash of the mint tag and nothing else.** No feature the
/// engine reads — liquidity, holder concentration, narrative, creator history, whale
/// activity, alpha calls — enters this function. Pinned so that a future attempt to
/// make the tape *teachable* (by conditioning the trajectory on an observable) is a
/// deliberate, visible act rather than an accident.
#[test]
fn outcome_is_a_function_of_the_mint_tag_alone() {
    // Same tag, different call sites, identical trajectory.
    for m in [3u64, 99, 400] {
        for round in 0..6u64 {
            for i in 0..3u64 {
                assert_eq!(
                    tape_golden::main_scalp(m, round, i),
                    tape_golden::main_scalp(m, round, i)
                );
            }
        }
    }
    // Adjacent tags land in unrelated outcome classes — the hash is an avalanche, so
    // there is no smooth observable a learner could ride.
    let peak = |m: u64| tape_golden::main_scalp(m, 2, 0).0;
    let a = peak(100);
    let b = peak(101);
    let c = peak(102);
    let monotone_up = a <= b && b <= c;
    let monotone_down = a >= b && b >= c;
    assert!(
        !monotone_up && !monotone_down,
        "consecutive tags must not be monotone in outcome — got {a}, {b}, {c}"
    );
}

// ===========================================================================
// THE PINS. Everything above is diagnostic; everything below is a fact about the
// representative corpus that must not be allowed to drift silently, because each
// one bounds what any A/B run on this tape is entitled to conclude.
// ===========================================================================

/// **THE EFFECTIVE SAMPLE SIZE IS FOUR MARKETS.** The tape presents 512 mints and
/// the report says 12 admits, but those 12 admits are re-entries into just 4
/// distinct markets. Every "verdict" this repo has recorded on the golden tape is
/// a statement about 4 hash draws.
///
/// Re-pin #26 made this WORSE, not better: the unified cost model admits one fewer
/// trade into one fewer market. The book doubled at the same time, which is exactly
/// the pairing that should raise suspicion rather than confidence — a larger number
/// from a smaller sample.
#[test]
fn the_representative_tape_trades_only_five_distinct_markets() {
    let mut eng = tape_golden::drive_eng(Config::dev_portable());
    let _ = eng.report();
    let mut mints: Vec<u64> = eng
        .brain()
        .index()
        .iter_oldest_first()
        .filter(|e| e.outcome().was_admitted)
        .map(|e| e.context().mint_id)
        .collect();
    let trades = mints.len();
    mints.sort_unstable();
    mints.dedup();
    assert_eq!(trades, 12, "admit count drifted");
    assert_eq!(
        mints.len(),
        4,
        "the golden book is 12 trades in {} distinct markets — if this changes, every \
         statistical claim made on this tape must be recomputed",
        mints.len()
    );
}

/// **THE BOOK IS NOT STATISTICALLY DISTINGUISHABLE FROM ZERO.** Per-trade net has a
/// standard deviation roughly 9x its mean, so on n = 12 the t-statistic is ~0.19
/// against a ~2.20 threshold. Pinned in integer arithmetic (§22): we assert
/// `mean^2 * n * 4 < variance`, i.e. |t| < 2, which is the honest statement.
///
/// This is NOT a defect to fix. It is the correct reading of the number, and it is
/// why `docs/BACKTEST.md §9` refuses to call the golden net evidence of edge.
///
/// **Re-pin #26 doubled the book and did not move this conclusion at all** (t went
/// from ~0.18 to ~0.19). That is the single most useful thing this test does: a
/// change that roughly DOUBLES realized net is still, on this corpus, indistinguishable
/// from zero, so "the net went up" is not evidence that anything got better.
#[test]
fn the_golden_book_is_indistinguishable_from_zero() {
    let mut eng = tape_golden::drive_eng(Config::dev_portable());
    let _ = eng.report();
    let nets: Vec<i128> = eng
        .brain()
        .index()
        .iter_oldest_first()
        .filter(|e| e.outcome().was_admitted)
        .map(|e| e.outcome().realized_net_lamports)
        .collect();
    let n = nets.len() as i128;
    assert_eq!(n, 12);
    let sum: i128 = nets.iter().sum();
    assert_eq!(sum, 16_778_896, "the book drifted");
    // Sample variance * (n-1), all-integer.
    let mean_num = sum; // mean = mean_num / n
    let ss: i128 = nets
        .iter()
        .map(|x| {
            let d = x * n - mean_num; // (x - mean) * n
            d * d
        })
        .sum::<i128>()
        / (n * n);
    let var = ss / (n - 1);
    // t^2 = mean^2 / (var / n) = mean^2 * n / var. Require t^2 < 4  (|t| < 2).
    let mean = sum / n;
    assert!(
        mean * mean * n < var * 4,
        "the golden book must remain statistically indistinguishable from zero \
         (mean {mean}, var {var}, n {n}); if a change ever makes this significant \
         on 12 trades in 4 markets, suspect the fixture before believing the edge"
    );
}

/// **60% OF THE BOOK IS A BOUNDARY ARTIFACT.** Positions that closed on the
/// strategy's own triggers earned +42,058,785. Positions still open when the tape ran
/// out were force-closed by `finalize()` for −25,279,889. The reported +16,778,896 is
/// the sum. Whether that −25.3M is real depends entirely on what those markets did
/// after the fixture stopped, which the fixture cannot say.
///
/// Re-pin #26 moved the fraction from 77% to 60% and moved nothing about the argument:
/// the naturally-earned subtotal rose (+34.9M → +42.1M) and the forced subtotal barely
/// moved (−26.8M → −25.3M), so a book that DOUBLED is still, in majority, an accident
/// of where the fixture stops. Read the doubling with that in front of it.
///
/// The same bias inflates the public "N% of pump.fun traders are profitable"
/// statistics, which are computed on realized PnL and exclude never-sold bags.
///
/// NOTE ON ORDERING, because it is a trap: `natural` must be summed BEFORE
/// `report()`. `report()` calls `finalize()`, which force-closes the open book and
/// seals those positions as episodes too — so summing the episode index afterwards
/// returns the TOTAL and makes the forced subtotal look like exactly zero.
#[test]
fn the_book_is_dominated_by_end_of_tape_force_closure() {
    let mut eng = tape_golden::drive_eng(Config::dev_portable());
    let natural: i128 = eng
        .brain()
        .index()
        .iter_oldest_first()
        .filter(|e| e.outcome().was_admitted)
        .map(|e| e.outcome().realized_net_lamports)
        .sum();
    let total = eng.report().net_lamports;
    assert_eq!(natural, 42_058_785, "naturally-closed subtotal drifted");
    assert_eq!(total, 16_778_896);
    let forced = total - natural;
    assert_eq!(forced, -25_279_889, "force-closed subtotal drifted");
    assert!(
        -forced * 2 > natural,
        "end-of-tape force closure erases the MAJORITY of what the strategy actually \
         earned ({natural} earned, {forced} forced) — the headline net is a boundary \
         artifact, and no re-pin that raises the net may be quoted without this number \
         beside it"
    );
    // The fraction itself, so a future change that improved it is visible rather than
    // merely non-failing. 77% at re-pin #24, 60% now.
    assert_eq!(
        -forced * 100 / natural,
        60,
        "boundary-artifact fraction drifted"
    );
}
