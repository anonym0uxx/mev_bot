//! W3/R5 (PLIM_CONTRACT.md §S5): paper fill and live builder AGREE on the same inputs.
//!
//! For one (curve state, clip, PRICE LIMIT) the paper executor's decision (`exec_quote::curve_buy` +
//! `price_anchor::plan_curve_buy`, exactly what `model_try_fills` runs) and the live sink's
//! (`LiveOutboundSink::on_admit`, fee rates from the same `exec_quote::curve_fees()` the daemon wires)
//! must be the same verdict with the same name, and when both proceed the live instruction must spend
//! exactly the paper clip with a floor the paper quote meets.
//!
//! NOTHING IS SENT. The signer only records the bytes it is asked to sign; the submitter refuses every
//! call. Synthetic curve states — engineering evidence only, not executable-quote evidence.

use std::sync::{Arc, Mutex};

use pump_quant_app::exec_quote::{curve_buy, curve_fees};
use pump_quant_execution::ex_live_io_traits::{
    LiveBlockhash, LiveCurveState, LiveSigner, LiveStateFetcher, LiveSubmitter, SignError,
    StateFetchError, SubmitError,
};
use pump_quant_execution::ex_live_sink::{LiveOutboundSink, LiveSinkConfig};
use pump_quant_execution::ex_outbound_sink::{AdmitRecord, OutboundOutcome, OutboundSink};
use pump_quant_execution::price_anchor::plan_curve_buy;
use pump_quant_protocol::ix::{BUY_DISCRIMINATOR, BUY_EXACT_SOL_IN_DISCRIMINATOR};
use pump_quant_protocol::layout::{
    LayoutKey, LayoutRegistry, Side, Variant, Venue, VerifiedLayout,
};
use pump_quant_protocol::tx_build::ComputePlan;
use pump_quant_protocol::venue_accounts::{FeeTail, PumpCurveCtx, TOKEN_PROGRAM_ID, WSOL_MINT};

const K: u128 = 30_000_000_000u128 * 1_073_000_000_000_000u128;
const VTOK_OFFSET: u64 = 279_900_000_000_000;

fn key(tag: u8) -> [u8; 32] {
    let mut k = [0u8; 32];
    k[0] = tag;
    k
}

struct Fetcher {
    vsol: u64,
    vtok: u64,
}

impl LiveStateFetcher for Fetcher {
    fn fetch_state_hot(
        &self,
        _m: &[u8; 32],
        _u: &[u8; 32],
    ) -> Result<LiveCurveState, StateFetchError> {
        Ok(LiveCurveState {
            curve_ctx: PumpCurveCtx {
                mint: key(1),
                user: key(2),
                fee_recipient: key(3),
                creator: key(4),
                token_program: TOKEN_PROGRAM_ID,
                is_cashback_coin: false,
                quote_mint: WSOL_MINT,
            },
            virtual_sol_reserves: self.vsol,
            virtual_token_reserves: self.vtok,
            is_complete: false,
            observed_slot: 440_000_000,
            buyback_fee_recipients: core::array::from_fn(|i| key(0x40 + i as u8)),
        })
    }
    fn prefetch_state(
        &self,
        m: &[u8; 32],
        u: &[u8; 32],
    ) -> Result<LiveCurveState, StateFetchError> {
        self.fetch_state_hot(m, u)
    }
    fn latest_blockhash(&self) -> Result<LiveBlockhash, StateFetchError> {
        Ok(LiveBlockhash {
            blockhash: [0x42; 32],
            slot: 440_000_000,
            last_valid_block_height: 440_000_150,
        })
    }
    fn fetch_ata_balance(
        &self,
        _m: &[u8; 32],
        _u: &[u8; 32],
        _t: &[u8; 32],
    ) -> Result<u64, StateFetchError> {
        Ok(0)
    }
}

struct Recorder(Arc<Mutex<Vec<Vec<u8>>>>);
impl LiveSigner for Recorder {
    fn sign(&self, m: &[u8]) -> Result<[u8; 64], SignError> {
        self.0.lock().unwrap().push(m.to_vec());
        Ok([7u8; 64])
    }
    fn public_key(&self) -> [u8; 32] {
        key(2)
    }
}
struct NoSubmit;
impl LiveSubmitter for NoSubmit {
    fn submit(&self, _w: &[u8], _b: bool) -> Result<[u8; 64], SubmitError> {
        Err(SubmitError::EndpointRejected(
            "submission disabled in tests".into(),
        ))
    }
}

fn registry() -> LayoutRegistry {
    let mut r = LayoutRegistry::new();
    let mut sig = [0u8; 64];
    for (i, b) in sig.iter_mut().enumerate() {
        *b = ((i as u32 * 37 + 5) % 256) as u8;
    }
    r.record_verified(VerifiedLayout {
        key: LayoutKey {
            venue: Venue::PumpFun,
            side: Side::Buy,
            variant: Variant::plain(),
        },
        account_count: 18,
        verifying_slot: 448_249_388,
        verifying_signature: sig,
    })
    .unwrap();
    r
}

/// Live verdict: Ok((spend, min_tokens_out)) parsed from the signed bytes, or Err(refusal label).
fn live(vsol: u64, vtok: u64, clip: u64, limit: Option<f64>) -> Result<(u64, u64), String> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = LiveOutboundSink::new(
        LiveSinkConfig {
            compute: ComputePlan {
                unit_limit: 200_000,
                unit_price_micro_lamports: 100_000,
            },
            tip: None,
            fee_tail: FeeTail::None,
            max_slippage_bps: 500,
            // Exactly what pq_daemon wires.
            curve_buy_fee_bps: curve_fees(),
            mcap_band_enable: false,
            mcap_band_lo_lamports: 0,
            mcap_band_hi_lamports: u64::MAX,
        },
        Arc::new(registry()),
        Arc::new(Fetcher { vsol, vtok }),
        Arc::new(Recorder(Arc::clone(&seen))),
        Arc::new(NoSubmit),
    );
    let out = sink.on_admit(&AdmitRecord {
        mint: key(1),
        user: [0u8; 32],
        is_buy: true,
        size_lamports: clip,
        entry_price: 0,
        max_slippage_bps: 500,
        price_limit_lamports_per_raw_token: limit,
    });
    let msgs = seen.lock().unwrap().clone();
    match out {
        OutboundOutcome::Sender(m) => {
            assert!(m.contains("submission disabled"), "{m}");
            assert_eq!(msgs.len(), 1);
            let b = &msgs[0];
            assert!(!b.windows(8).any(|w| w == BUY_DISCRIMINATOR));
            let at = b
                .windows(8)
                .position(|w| w == BUY_EXACT_SOL_IN_DISCRIMINATOR)
                .unwrap();
            let u = |o: usize| u64::from_le_bytes(b[at + o..at + o + 8].try_into().unwrap());
            Ok((u(8), u(16)))
        }
        OutboundOutcome::Construction(m) => {
            assert!(msgs.is_empty(), "a refusal must sign nothing");
            Err(m
                .split(':')
                .next()
                .unwrap()
                .trim_start_matches("price_")
                .to_string())
        }
        o => panic!("unexpected live outcome {o:?}"),
    }
}

/// Paper verdict as `model_try_fills` computes it: Ok(tokens delivered for the whole clip) or Err(label).
fn paper(vsol: u64, vtok: u64, clip: u64, limit: Option<f64>) -> Result<u64, String> {
    let real_tok = vtok - VTOK_OFFSET;
    let q = curve_buy(vsol, vtok, real_tok, clip).expect("quotable synthetic curve");
    assert_eq!(q.spend, clip, "paper spends the whole clip");
    if limit.is_some() {
        plan_curve_buy(clip, limit, Some(q.tokens), 0).map_err(|r| r.label().to_string())?;
    }
    Ok(q.tokens)
}

#[test]
fn the_daemon_wires_the_paper_fee_rates_into_the_live_quote() {
    assert_eq!(curve_fees(), Some((95, 30)));
}

#[test]
fn paper_and_live_agree_on_every_case_of_the_grid() {
    let mut n = (0, 0);
    for vsol in [32_000_000_000u64, 50_000_000_000, 80_000_000_000] {
        let vtok = (K / u128::from(vsol)) as u64;
        let spot = vsol as f64 / vtok as f64;
        for clip in [125_000_000u64, 250_000_000, 500_000_000] {
            let q = curve_buy(vsol, vtok, vtok - VTOK_OFFSET, clip).unwrap();
            let avg = clip as f64 / q.tokens as f64;
            let limits = [
                None,
                Some(spot), // the copied prompt price: never meetable (impact + fees)
                Some(avg),  // the exact achieved average
                Some(avg * 0.9999), // a hair under it
                Some(avg * 1.01),
                Some(spot * 1.05),
                Some(spot * 1e-9), // SOL/raw unit mistake: unconvertible
            ];
            for limit in limits {
                let p = paper(vsol, vtok, clip, limit);
                let l = live(vsol, vtok, clip, limit);
                match (&p, &l) {
                    (Ok(tokens), Ok((spend, floor))) => {
                        assert_eq!(*spend, clip, "live spends exactly the paper clip");
                        assert!(floor <= tokens, "paper's quote meets the live floor");
                        n.0 += 1;
                    }
                    (Err(a), Err(b)) => {
                        assert_eq!(a, b, "same named refusal");
                        n.1 += 1;
                    }
                    _ => panic!(
                        "DISAGREE vsol={vsol} clip={clip} limit={limit:?}: paper={p:?} live={l:?}"
                    ),
                }
                if limit == Some(spot) {
                    assert_eq!(p, Err("limit_not_met".into()), "copied price never fills");
                }
            }
        }
    }
    // 9 (state, clip) cells x 7 limits = 63. Per cell: absent, avg*1.01 build; spot, avg*0.9999, SOL/raw
    // refuse; `avg` (the f64 nearest clip/tokens) builds only when that f64 is not below the exact
    // ratio, and spot*1.05 only on cells whose impact+fees stay under 5%. No case disagreed.
    assert_eq!(n, (30, 33), "(both build, both refuse)");
}

/// The +187.5 bp regression fixture (PLIM_REPORT R4 B.1): 80 SOL pool, FULL clip 0.5 SOL, limit = the
/// supplied marginal price 0.000198819. Both paths refuse it by the same name; neither re-sizes.
#[test]
fn the_187_5_bp_fixture_is_refused_by_both_paths() {
    let (vsol, vtok) = (80_000_000_000u64, (K / 80_000_000_000u128) as u64);
    let clip = 500_000_000;
    let limit = Some(198_819e-9);
    assert_eq!(paper(vsol, vtok, clip, limit), Err("limit_not_met".into()));
    assert_eq!(live(vsol, vtok, clip, limit), Err("limit_not_met".into()));
    let tokens = paper(vsol, vtok, clip, None).unwrap();
    assert_eq!(tokens, 2_468_558_269_818);
    let over_bp = ((clip as f64 / tokens as f64) / 198_819e-9 - 1.0) * 10_000.0;
    assert!((over_bp - 187.5).abs() < 0.1, "{over_bp}");
}
