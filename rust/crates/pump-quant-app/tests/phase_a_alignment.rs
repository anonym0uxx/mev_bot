//! Phase-A alignment laws (remediation directive): unknown/stale critical data
//! fails closed, caller-supplied conclusions cannot authorize capital, and
//! optimistic evidence cannot satisfy promotion. Each test drives the REAL
//! engine through its public surface — no mocks, no private access.

use pump_quant_app::config::{Config, FillModeCfg};
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::screen::FlowScreen;
use pump_quant_domain::ids::Mint;

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xCD;
    Mint::from_bytes(b)
}

/// Deep, admissible flow for one mint: strong OFI, deep pool, broad entities.
fn feed_flow(eng: &mut Engine, mt: Mint, trades: u64) {
    for i in 0..trades {
        eng.tick(AppEvent::MarketTrade {
            mint: mt,
            price_fp: 1_000_000_000 + (i as i128) * 100_000,
            quote_lamports: 800_000,
            liquidity_lamports: 300_000_000,
            signed_base: 900_000 - (i as i64),
            buyer_entity: 10 + i % 9,
            age_slots: 12,
        });
    }
}

// ============================================================================
// §34.3: a fresh confirm can NEVER borrow a stale numeric snapshot.
// ============================================================================
#[test]
fn stale_numeric_snapshot_cannot_authorize_entry() {
    let cfg = Config::dev_portable();
    let ttl = cfg.lane_evidence_ttl_ticks;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    let mt = mint(1);
    feed_flow(&mut eng, mt, 20);
    // Age the numeric evidence PAST the lane TTL (no further trades)...
    for _ in 0..(ttl + 5) {
        eng.tick(AppEvent::Tick);
    }
    // ...then land a perfectly fresh on-chain confirm.
    eng.tick(AppEvent::OnchainConfirm {
        mint: mt,
        sellable_depth_lamports: 500_000_000,
    });
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    assert_eq!(
        r.admitted, 0,
        "depth proven now + liquidity observed long ago must NOT authorize"
    );
}

/// Control for the test above: identical stream WITHOUT the stale gap admits.
#[test]
fn fresh_numeric_snapshot_with_confirm_admits() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let mt = mint(2);
    feed_flow(&mut eng, mt, 20);
    eng.tick(AppEvent::OnchainConfirm {
        mint: mt,
        sellable_depth_lamports: 500_000_000,
    });
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    assert!(r.admitted > 0, "the control stream must admit");
}

// ============================================================================
// §15: caller-ASSERTED sellable depth is cross-checked against observed
// liquidity — an inflated claim buys no extra size.
// ============================================================================
#[test]
fn inflated_depth_claim_buys_no_size() {
    let run = |claimed_depth: u64| -> Vec<u64> {
        let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
        let mt = mint(3);
        feed_flow(&mut eng, mt, 20);
        eng.tick(AppEvent::OnchainConfirm {
            mint: mt,
            sellable_depth_lamports: claimed_depth,
        });
        for _ in 0..3 {
            eng.tick(AppEvent::Tick);
        }
        eng.journal()
            .recent()
            .filter_map(|d| match *d {
                pump_quant_app::journal_log::Decision::Admitted { size_lamports, .. } => {
                    Some(size_lamports)
                }
                _ => None,
            })
            .collect()
    };
    // Observed pool liquidity in feed_flow is 300M; claim 100× that.
    let honest = run(300_000_000);
    let inflated = run(30_000_000_000);
    assert!(!honest.is_empty());
    assert_eq!(
        honest, inflated,
        "an asserted depth beyond observed liquidity must change nothing"
    );
}

// ============================================================================
// §28/§29: caller-supplied wallet conclusions cannot authorize entry.
// ============================================================================
#[test]
fn caller_followable_cannot_authorize_entry() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let mt = mint(4);
    // A "smart wallet" screams BUY every round, and the market even has a
    // confirmed depth — but there is NO numeric flow evidence at all.
    for round in 0..6u64 {
        eng.tick(AppEvent::WalletAction {
            mint: mt,
            followable: true,
            size_lamports: 500_000_000,
        });
        if round == 0 {
            eng.tick(AppEvent::OnchainConfirm {
                mint: mt,
                sellable_depth_lamports: 500_000_000,
            });
        }
        for _ in 0..6 {
            eng.tick(AppEvent::Tick);
        }
    }
    let r = eng.report();
    assert_eq!(
        r.admitted, 0,
        "wallet conclusions + confirm without numeric flow must never enter"
    );
}

// ============================================================================
// §6.4/§33: thin-sample authenticity is a LABEL, not confirmation — the flow
// screen refuses to attest evidence below its swap-sample floor.
// ============================================================================
#[test]
fn thin_sample_authenticity_is_not_evidence() {
    let mut s = FlowScreen::new();
    let m = [7u8; 32];
    for i in 0..(pump_quant_app::screen::MIN_SWAPS_FOR_AUTH - 1) {
        s.record(&m, u64::from(i % 5), true, 100_000);
    }
    assert!(
        !s.has_auth_evidence(&m),
        "below the sample floor there is no authenticity EVIDENCE"
    );
    s.record(&m, 3, true, 100_000);
    assert!(s.has_auth_evidence(&m), "at the floor, evidence exists");
}

// ============================================================================
// §38/§64: optimistic evidence can never satisfy promotion; Mode C paper
// still fails the probe gate fail-closed; the blocker is capability/criteria,
// never a human approval.
// ============================================================================
#[test]
fn optimistic_run_is_labeled_and_never_promotion_eligible() {
    let eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let r = eng.promotion_readiness();
    assert!(!r.live_probe_eligible);
    assert_eq!(r.blocked_on, "mode_c_required");
}

#[test]
fn mode_c_paper_fails_probe_gate_closed() {
    let mut cfg = Config::dev_portable();
    cfg.fill_mode = FillModeCfg::AdversarialRealistic;
    let eng = Engine::new(cfg, RunMode::Replay);
    let r = eng.promotion_readiness();
    assert!(!r.live_probe_eligible);
    assert!(
        r.blocked_on.starts_with("probe_gate:"),
        "laptop Mode C blocks on unattested criteria, got {}",
        r.blocked_on
    );
}

// ============================================================================
// §24: conditional expectancy uses the configured PRIOR below the minimum
// lane sample, and the lane cell only graduates with realized fills.
// ============================================================================
#[test]
fn expectancy_is_prior_until_lane_sample_gate() {
    let cfg = Config::dev_portable();
    let prior = i128::from(cfg.gate_expected_move_bps);
    let mut eng = Engine::new(cfg, RunMode::Replay);
    // No fills yet: every lane's conditional edge is exactly the prior.
    for (sum, n, edge) in eng.expectancy_report() {
        assert_eq!((sum, n), (0, 0));
        assert_eq!(edge, prior);
    }
    // Drive a couple of realized fills; the cell accumulates but the edge
    // stays at the prior below `expectancy_min_lane_trades`.
    let mt = mint(5);
    feed_flow(&mut eng, mt, 20);
    eng.tick(AppEvent::OnchainConfirm {
        mint: mt,
        sellable_depth_lamports: 400_000_000,
    });
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    // Crash it so the position exits and realizes.
    for i in 0..16u64 {
        eng.tick(AppEvent::MarketTrade {
            mint: mt,
            price_fp: 600_000_000 - (i as i128) * 10_000_000,
            quote_lamports: 800_000,
            liquidity_lamports: 300_000_000,
            signed_base: 900_000,
            buyer_entity: 10 + i % 9,
            age_slots: 12,
        });
    }
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    let rep = eng.expectancy_report();
    let filled: u32 = rep.iter().map(|(_, n, _)| *n).sum();
    if filled > 0 && filled < eng_min(&eng) {
        for (_, n, edge) in rep {
            if n > 0 {
                assert_eq!(
                    edge, prior,
                    "below the sample gate the edge must remain the prior"
                );
            }
        }
    }
}

fn eng_min(_e: &Engine) -> u32 {
    Config::dev_portable().expectancy_min_lane_trades
}
