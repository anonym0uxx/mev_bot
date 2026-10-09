//! `paper_fill_v2_shadow` shadow-pool laws (spec: docs/missing_history_causal/SHADOW_POOL_SPEC.md).
use pump_quant_app::exec_quote::{curve_buy, curve_sell, QuoteRefusal};
use pump_quant_protocol::pumpswap_event::CashbackField;
/// Older-layout cashback state: with a non-zero creator fee the legacy rule prices no cashback (behaviour the
/// shadow laws were written against). Unknown cashback with a zero creator fee is refused (test below).
const CB_LEGACY: CashbackField = CashbackField::Missing { layout_len: 352 };
use pump_quant_app::shadow_pool::{
    ApplyOutcome, CurveBase, Divergence, FillBasis, LegKind, ObservedVenue, PaperFillVersion,
    PoolBase, ShadowBook, ShadowRefusal, ShadowVenue, NETWORK_FEE_PER_LANDED_LEG_ESTIMATE,
    PAPER_FILL_V2_SHADOW,
};

const M: [u8; 32] = [7u8; 32];
const VOFF: u64 = 30_000_000_000;
const TOFF: u64 = 279_900_000_000_000;

/// A standard curve state with `rsol` real SOL on the constant product of the initial curve.
fn curve(rsol: u64, slot: u64) -> CurveBase {
    let vsol = VOFF + rsol;
    let k: u128 = u128::from(VOFF) * 1_073_000_000_000_000u128;
    let vtok = u64::try_from(k / u128::from(vsol)).unwrap();
    CurveBase {
        vsol,
        vtok,
        real_sol: rsol,
        real_tok: vtok - TOFF,
        slot,
    }
}

fn buy(book: &mut ShadowBook, b: CurveBase, id: u64, spend: u64) -> (u64, u64) {
    let q = curve_buy(b.vsol, b.vtok, b.real_tok, spend).unwrap();
    let o = book.apply_own_fill(
        M,
        FillBasis::Curve(b),
        LegKind::Entry,
        id,
        q.tokens,
        q.net_in,
    );
    assert!(matches!(o, ApplyOutcome::Applied { .. }), "{o:?}");
    (q.tokens, q.net_in)
}

#[test]
fn version_label_is_separate_and_parse_is_strict() {
    assert_eq!(PaperFillVersion::V2Shadow.label(), PAPER_FILL_V2_SHADOW);
    assert_eq!(PAPER_FILL_V2_SHADOW, "paper_fill_v2_shadow");
    assert_ne!(PaperFillVersion::V1Observed.label(), PAPER_FILL_V2_SHADOW);
    assert_eq!(PaperFillVersion::default(), PaperFillVersion::V1Observed);
    assert_eq!(PaperFillVersion::parse("paper_fill_v2"), None);
}

#[test]
fn buy_then_sell_round_trip_is_conserved_and_external_benchmark_is_reported_separately() {
    let mut book = ShadowBook::default();
    let b = curve(2_000_000_000, 10);
    let (tok, net_in) = buy(&mut book, b, 1, 500_000_000);
    // Same observed state (nobody else traded): the observed record is untouched by our fill.
    let v = book.curve_sell_view(&M, &b, tok);
    assert!(v.delta_carried);
    let ext = v.external.unwrap();
    let sh = v.shadow.unwrap();
    // The shadow sell walks back down OUR delta: gross ~= net SOL we put in, never more.
    assert!(
        sh.gross <= net_in && net_in - sh.gross <= 2,
        "{} {}",
        sh.gross,
        net_in
    );
    // External benchmark: observed curve without our buy pays less for the same tokens.
    assert!(ext.gross < sh.gross);
    assert_eq!(
        v.capacity_lamports,
        u128::from(b.real_sol) + u128::from(net_in)
    );
    let o = book.apply_own_fill(M, FillBasis::Curve(b), LegKind::Sell, 2, tok, sh.gross);
    assert_eq!(
        o,
        ApplyOutcome::Applied {
            tokens: tok,
            lamports: sh.gross
        }
    );
    let m = book.market(&M).unwrap();
    assert_eq!(m.tok_out, m.tok_in);
    assert_eq!(m.conserved_sol(), u128::from(net_in - sh.gross));
    // After the round trip the shadow reserves are (to rounding) the observed reserves.
    let (s, _) = book.shadow_curve(&M, &b).unwrap();
    assert_eq!(s.vtok, b.vtok);
    assert!(s.real_sol - b.real_sol <= 2);
}

#[test]
fn partial_fills_accumulate_by_cumulative_report() {
    let mut book = ShadowBook::default();
    let b = curve(1_000_000_000, 10);
    let fb = FillBasis::Curve(b);
    assert_eq!(
        book.apply_own_fill(M, fb, LegKind::Entry, 9, 100, 1_000),
        ApplyOutcome::Applied {
            tokens: 100,
            lamports: 1_000
        }
    );
    assert_eq!(
        book.apply_own_fill(M, fb, LegKind::Entry, 9, 250, 2_600),
        ApplyOutcome::Applied {
            tokens: 150,
            lamports: 1_600
        }
    );
    assert_eq!(
        book.apply_own_fill(M, fb, LegKind::Entry, 9, 400, 4_000),
        ApplyOutcome::Applied {
            tokens: 150,
            lamports: 1_400
        }
    );
    let m = book.market(&M).unwrap();
    assert_eq!((m.tok_out, m.sol_in), (400, 4_000));
    assert_eq!(m.applied.get(&(LegKind::Entry, 9)), Some(&(400, 4_000)));
}

#[test]
fn duplicate_and_backwards_reports_change_nothing() {
    let mut book = ShadowBook::default();
    let fb = FillBasis::Curve(curve(1_000_000_000, 10));
    book.apply_own_fill(M, fb, LegKind::Entry, 1, 500, 5_000);
    let before = book.clone();
    assert_eq!(
        book.apply_own_fill(M, fb, LegKind::Entry, 1, 500, 5_000),
        ApplyOutcome::Duplicate
    );
    assert_eq!(
        book.apply_own_fill(M, fb, LegKind::Entry, 1, 400, 5_000),
        ApplyOutcome::Backwards
    );
    assert_eq!(
        book.apply_own_fill(M, fb, LegKind::Entry, 1, 600, 4_000),
        ApplyOutcome::Backwards
    );
    assert_eq!(book, before);
    // Same numeric id in another leg namespace is a different fill.
    assert!(matches!(
        book.apply_own_fill(M, fb, LegKind::Add, 1, 500, 5_000),
        ApplyOutcome::Applied { .. }
    ));
    assert_eq!(book.market(&M).unwrap().sol_in, 10_000);
}

#[test]
fn fresh_consistent_snapshot_replaces_base_and_carries_delta_without_rewriting_it() {
    let mut book = ShadowBook::default();
    let b0 = curve(1_000_000_000, 10);
    let (tok, net_in) = buy(&mut book, b0, 1, 300_000_000);
    // Another participant buys: a fresh observed state on the same curve (offsets unchanged).
    let b1 = curve(1_700_000_000, 11);
    let obs_copy = b1;
    assert_eq!(book.on_curve_observation(&M, &b1), None);
    assert_eq!(b1, obs_copy, "observed evidence is never modified");
    let (s, cap) = book.shadow_curve(&M, &b1).unwrap();
    assert_eq!(s.real_sol, b1.real_sol + net_in);
    assert_eq!(s.vsol, b1.vsol + net_in);
    assert_eq!(s.real_tok, b1.real_tok - tok);
    assert_eq!(cap, u128::from(b1.real_sol) + u128::from(net_in));
    // Our delta is added once, not once per snapshot.
    assert_eq!(book.on_curve_observation(&M, &b1), None);
    let (s2, _) = book.shadow_curve(&M, &b1).unwrap();
    assert_eq!(s2, s);
    assert_eq!(book.market(&M).unwrap().sol_in, u128::from(net_in));
}

#[test]
fn virtual_offset_change_is_named_divergence_and_drops_delta_permanently() {
    let mut book = ShadowBook::default();
    let b0 = curve(1_000_000_000, 10);
    let (tok, _) = buy(&mut book, b0, 1, 300_000_000);
    let mut bad = curve(1_000_000_000, 11);
    bad.vsol -= 5_000_000; // vsol moved without real SOL moving: not a trade
    assert_eq!(
        book.on_curve_observation(&M, &bad),
        Some(Divergence::CurveVirtualOffsetChanged)
    );
    assert_eq!(
        book.divergence(&M),
        Some(Divergence::CurveVirtualOffsetChanged)
    );
    // A later consistent snapshot does NOT revive the delta.
    let b2 = curve(1_000_000_000, 12);
    assert_eq!(book.on_curve_observation(&M, &b2), None);
    let v = book.curve_sell_view(&M, &b2, tok);
    assert!(!v.delta_carried);
    assert_eq!(v.capacity_lamports, u128::from(b2.real_sol));
    assert_eq!(
        book.net_liquidation_estimate(&M, Some(&ObservedVenue::Curve(b2)), tok),
        None,
        "diverged -> unknown, never zero or cost"
    );
    // Our later fills are identity-recorded but not carried.
    assert_eq!(
        book.apply_own_fill(M, FillBasis::Curve(b2), LegKind::Add, 3, 10, 10),
        ApplyOutcome::NotCarried
    );
    assert_eq!(book.market(&M).unwrap().sol_in, 0);
}

#[test]
fn adverse_liquidity_change_removes_fictional_sale_capacity() {
    // Curve: token offset moved (non-trade change).
    let mut book = ShadowBook::default();
    let b0 = curve(1_000_000_000, 10);
    let (tok, _) = buy(&mut book, b0, 1, 300_000_000);
    let mut adv = curve(1_000_000_000, 11);
    adv.real_tok -= 1_000_000_000_000;
    assert_eq!(
        book.on_curve_observation(&M, &adv),
        Some(Divergence::AdverseLiquidityChange)
    );
    let v = book.curve_sell_view(&M, &adv, tok);
    assert!(!v.delta_carried);
    assert_eq!(v.capacity_lamports, u128::from(adv.real_sol));

    // Pool: constant product decreased (liquidity withdrawn).
    let mut book = ShadowBook::default();
    let p0 = PoolBase {
        base: 1_000_000_000_000,
        quote: 50_000_000_000,
        vq: 0,
        slot: 1,
    };
    book.apply_own_fill(
        M,
        FillBasis::Pool(p0),
        LegKind::Entry,
        1,
        10_000_000_000,
        500_000_000,
    );
    let p1 = PoolBase {
        base: 500_000_000_000,
        quote: 25_000_000_000,
        vq: 0,
        slot: 2,
    };
    assert_eq!(
        book.on_pool_observation(&M, &p1),
        Some(Divergence::AdverseLiquidityChange)
    );
    let v = book.pool_sell_view(&M, &p1, Some((20, 5, 5)), CB_LEGACY, 10_000_000_000);
    assert!(!v.delta_carried);
    assert_eq!(v.capacity_lamports, 25_000_000_000);
    assert_eq!(
        v.shadow.unwrap(),
        v.external.unwrap(),
        "no delta -> shadow == observed"
    );
}

#[test]
fn unreconcilable_snapshot_is_reported_not_papered_over() {
    let mut book = ShadowBook::default();
    let p0 = PoolBase {
        base: 1_000_000_000_000,
        quote: 1_000_000_000,
        vq: 0,
        slot: 1,
    };
    // Post-graduation pool segment: we SELL curve-bought tokens: SOL leaves the pool (dsol < 0).
    book.apply_own_fill(
        M,
        FillBasis::Pool(p0),
        LegKind::Sell,
        4,
        50_000_000_000,
        900_000_000,
    );
    // A snapshot whose real quote vault cannot have paid what the shadow says we took out.
    let p1 = PoolBase {
        base: 2_000_000_000_000,
        quote: 800_000_000,
        vq: 0,
        slot: 2,
    };
    assert_eq!(
        book.on_pool_observation(&M, &p1),
        Some(Divergence::UnreconcilableSnapshot)
    );
    assert_eq!(
        book.divergence(&M).unwrap().label(),
        "shadow_divergence:unreconcilable_snapshot"
    );
}

#[test]
fn conservation_capacity_caps_shadow_sale_and_no_double_application() {
    let mut book = ShadowBook::default();
    let b = curve(50_000_000, 10); // nearly empty curve
    let (tok, net_in) = buy(&mut book, b, 1, 100_000_000);
    // Re-reporting the same cumulative fill twice must not double our SOL contribution.
    let q = curve_buy(b.vsol, b.vtok, b.real_tok, 100_000_000).unwrap();
    book.apply_own_fill(
        M,
        FillBasis::Curve(b),
        LegKind::Entry,
        1,
        q.tokens,
        q.net_in,
    );
    assert_eq!(book.market(&M).unwrap().sol_in, u128::from(net_in));
    // Selling MORE than we bought (inventory from elsewhere) cannot draw beyond capacity.
    let v = book.curve_sell_view(&M, &b, tok * 3);
    let cap = u128::from(b.real_sol) + u128::from(net_in);
    assert_eq!(v.capacity_lamports, cap);
    match v.shadow {
        Ok(qs) => assert!(u128::from(qs.gross) <= cap),
        Err(ShadowRefusal::CapacityExceeded { capacity }) => assert_eq!(capacity, cap),
        Err(ShadowRefusal::Quote(QuoteRefusal::CurveRealSolInsufficient { .. })) => {}
        Err(e) => panic!("unexpected {e:?}"),
    }
}

#[test]
fn restart_persist_restore_is_identical_and_keeps_idempotency() {
    let mut book = ShadowBook::default();
    let b = curve(1_000_000_000, 10);
    let (tok, net_in) = buy(&mut book, b, 1, 300_000_000);
    let mut m2 = M;
    m2[0] = 9;
    let p0 = PoolBase {
        base: 1_000_000_000_000,
        quote: 50_000_000_000,
        vq: 1,
        slot: 1,
    };
    book.apply_own_fill(m2, FillBasis::Pool(p0), LegKind::Entry, 5, 77, 88);
    let mut adv = curve(1_000_000_000, 11);
    adv.real_tok -= 5;
    let mut m3 = M;
    m3[0] = 3;
    book.apply_own_fill(m3, FillBasis::Curve(b), LegKind::Entry, 6, 1, 1);
    book.on_curve_observation(&m3, &adv);
    let text = serde_json::to_string(&book.to_json()).unwrap();
    let mut back = ShadowBook::from_json(&serde_json::from_str(&text).unwrap()).unwrap();
    assert_eq!(back, book);
    assert_eq!(
        back.divergence(&m3),
        Some(Divergence::AdverseLiquidityChange)
    );
    // A replayed fill report after restart is still a duplicate.
    assert_eq!(
        back.apply_own_fill(M, FillBasis::Curve(b), LegKind::Entry, 1, tok, net_in),
        ApplyOutcome::Duplicate
    );
    // Malformed / wrong-version durable form is refused, never an empty book.
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["version"] = serde_json::json!("paper_fill_v1_observed");
    assert_eq!(ShadowBook::from_json(&v), Err("shadow_pool.version"));
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["markets"][0]["sol_in"] = serde_json::json!(5);
    assert!(ShadowBook::from_json(&v).is_err());
}

#[test]
fn graduation_ends_curve_shadow_and_starts_empty_pool_segment() {
    let mut book = ShadowBook::default();
    let b = curve(1_000_000_000, 10);
    let (tok, net_in) = buy(&mut book, b, 1, 300_000_000);
    assert!(book.on_graduation(&M), "a curve delta was dropped");
    let m = book.market(&M).unwrap();
    assert_eq!(m.venue, ShadowVenue::Pool);
    assert_eq!((m.sol_in, m.tok_out), (0, 0));
    // A late duplicate report of the curve fill stays a duplicate (identity survives migration).
    assert_eq!(
        book.apply_own_fill(M, FillBasis::Curve(b), LegKind::Entry, 1, tok, net_in),
        ApplyOutcome::Duplicate
    );
    // A late NEW curve increment is identity-recorded but never enters the pool delta.
    assert_eq!(
        book.apply_own_fill(
            M,
            FillBasis::Curve(b),
            LegKind::Entry,
            1,
            tok + 1,
            net_in + 1
        ),
        ApplyOutcome::NotCarried
    );
    let p0 = PoolBase {
        base: 200_000_000_000_000,
        quote: 85_000_000_000,
        vq: 0,
        slot: 20,
    };
    let v = book.pool_sell_view(&M, &p0, Some((20, 5, 5)), CB_LEGACY, tok);
    assert_eq!(
        v.capacity_lamports, 85_000_000_000,
        "curve SOL is not carried into the pool"
    );
    assert!(!book.on_graduation(&M), "second graduation is a no-op");
}

#[test]
fn engine_hooks_divergence_and_net_liquidation_estimate() {
    let mut book = ShadowBook::default();
    let b = curve(1_000_000_000, 10);
    assert_eq!(book.divergence(&M), None);
    assert_eq!(
        book.net_liquidation_estimate(&M, None, 1_000),
        None,
        "no observation -> unknown"
    );
    let (tok, _) = buy(&mut book, b, 1, 300_000_000);
    let est = book
        .net_liquidation_estimate(&M, Some(&ObservedVenue::Curve(b)), tok)
        .unwrap();
    let q = book.curve_sell_view(&M, &b, tok).shadow.unwrap();
    assert_eq!(
        est,
        i128::from(q.net) - i128::from(NETWORK_FEE_PER_LANDED_LEG_ESTIMATE)
    );
    assert_eq!(NETWORK_FEE_PER_LANDED_LEG_ESTIMATE, 10_000);
    // Venue mismatch (curve shadow, pool observation) -> unknown.
    let p = PoolBase {
        base: 1,
        quote: 1,
        vq: 1,
        slot: 1,
    };
    assert_eq!(
        book.net_liquidation_estimate(
            &M,
            Some(&ObservedVenue::Pool(p, Some((20, 5, 5)), CB_LEGACY)),
            tok
        ),
        None
    );
}

/// Integration seam (shadow x cashback): unknown cashback on a zero-creator-fee pool = unknown liquidation value.
#[test]
fn unknown_cashback_on_zero_creator_fee_pool_is_unknown_liquidation_not_zero() {
    let mut book = ShadowBook::default();
    let p = PoolBase {
        base: 1_000_000_000_000,
        quote: 50_000_000_000,
        vq: 0,
        slot: 1,
    };
    book.apply_own_fill(
        M,
        FillBasis::Pool(p),
        LegKind::Entry,
        1,
        10_000_000_000,
        500_000_000,
    );
    for cb in [
        CashbackField::Missing { layout_len: 352 },
        CashbackField::Unsupported { layout_len: 999 },
        CashbackField::NotRecorded,
    ] {
        let v = book.pool_sell_view(&M, &p, Some((20, 5, 0)), cb, 1_000_000_000);
        assert!(
            matches!(v.external, Err(QuoteRefusal::AmmCashbackUnknown)),
            "{cb:?}"
        );
        assert!(
            v.shadow.is_err(),
            "{cb:?}: shadow never prices unknown cashback"
        );
        let obs = ObservedVenue::Pool(p, Some((20, 5, 0)), cb);
        assert_eq!(
            book.net_liquidation_estimate(&M, Some(&obs), 1_000_000_000),
            None,
            "{cb:?}"
        );
    }
    // Known zero cashback is priced (a known zero, not unknown).
    let known0 = CashbackField::Known {
        bps: 0,
        lamports: 0,
        layout_len: 369,
    };
    let obs = ObservedVenue::Pool(p, Some((20, 5, 0)), known0);
    assert!(book
        .net_liquidation_estimate(&M, Some(&obs), 1_000_000_000)
        .is_some());
}

// ---------------------------------------------------------------- 804a86fe regression fixture
// Tape rows (proc/shadow_fx_804a86fe.txt, mint 9do6fVUE..pump = 804a86fe..), CurveObserved only.
// (slot, vsol, vtok, real_sol, real_tok)
const FX: &[(u64, u64, u64, u64, u64)] = &[
    (
        445637772,
        9644973031,
        1061225758739047,
        179532847,
        781325758739047,
    ),
    (
        445637775,
        7278386871,
        1066163371549581,
        134865076,
        786263371549581,
    ),
    (
        445637777,
        6360126858,
        1068702057721707,
        117575375,
        788802057721707,
    ),
    (
        445637782,
        6244735923,
        1088449655094557,
        2184440,
        808549655094557,
    ),
    (445637790, 6242551485, 1088830533311135, 2, 808930533311135),
    (
        445637874,
        6280209011,
        1082301663962052,
        37657528,
        802401663962052,
    ),
];
const FX_SPEND: u64 = 115_840_829;
const FX_INVENTORY: u64 = 12_440_905_893_289; // held.json inventory_tokens for 804a86fe

fn fx(i: usize) -> CurveBase {
    let (slot, vsol, vtok, real_sol, real_tok) = FX[i];
    CurveBase {
        vsol,
        vtok,
        real_sol,
        real_tok,
        slot,
    }
}

#[test]
fn regression_804a86fe_external_and_shadow_results_are_separate() {
    let mut book = ShadowBook::default();
    let b0 = fx(0);
    // Our paper entry priced on the landing state reproduces the held inventory exactly.
    let q = curve_buy(b0.vsol, b0.vtok, b0.real_tok, FX_SPEND).unwrap();
    assert_eq!(q.tokens, FX_INVENTORY);
    assert_eq!(q.net_in, 114_410_694);
    book.apply_own_fill(
        M,
        FillBasis::Curve(b0),
        LegKind::Entry,
        4,
        q.tokens,
        q.net_in,
    );

    // (A) EXTERNAL benchmark on the final observed state: the v1 refusal, unchanged.
    let last = fx(FX.len() - 1);
    assert_eq!(
        curve_sell(last.vsol, last.vtok, last.real_sol, FX_INVENTORY),
        Err(QuoteRefusal::CurveRealSolInsufficient {
            max_tokens: 6_528_869_872_346
        })
    );

    // (B) SHADOW at the landing state: our delta is liquidity we put in, so the shadow can sell back,
    // bounded by what we put in (no manufactured profit).
    let v0 = book.curve_sell_view(&M, &b0, FX_INVENTORY);
    let s0 = v0.shadow.unwrap();
    assert!(s0.gross <= q.net_in && q.net_in - s0.gross <= 2);

    // (C) Replaying the tape's later snapshots. 9do6fVUE.. is a Mayhem-mode curve (BondingCurve byte 81 = 1). The next
    // snapshot is the post-state of a REAL trade, a Mayhem-routed sell of 4,937,612,810,534 tokens for 44,667,771
    // lamports (tx at cont_wire05 line 32627). The sell was priced constant-product on the pre-state, then the
    // program reset `vsol - real_sol` (9,465,440,184 -> 7,143,521,795). Whatever rule the Mayhem program uses to
    // reset virtual SOL is not modelled, so the shadow cannot carry our delta across it -> named divergence, delta
    // dropped. Not a refusal loop, not a fake exit. Evidence: proc/OFFSET_bM3a_REPORT.md.
    let mut first_div = None;
    for i in 1..FX.len() {
        if let Some(d) = book.on_curve_observation(&M, &fx(i)) {
            first_div.get_or_insert((fx(i).slot, d));
        }
    }
    assert_eq!(
        first_div,
        Some((445637775, Divergence::CurveVirtualOffsetChanged))
    );
    let vl = book.curve_sell_view(&M, &last, FX_INVENTORY);
    assert!(!vl.delta_carried);
    assert_eq!(vl.capacity_lamports, 37_657_528);
    assert!(
        vl.shadow.is_err(),
        "no fictional sale capacity after divergence"
    );
    assert_eq!(
        book.net_liquidation_estimate(&M, Some(&ObservedVenue::Curve(last)), FX_INVENTORY),
        None
    );
    // The v1 defect magnitude for the record: 71,369,737 paper gross vs 37,657,528 real SOL.
    assert!(71_369_737 > last.real_sol);
}
