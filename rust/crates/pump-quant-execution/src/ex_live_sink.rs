//! Live outbound sink: the real state-fetch → build → sign → submit pipeline.
//!
//! Replaces the `NoopSink` placeholder. This module implements `OutboundSink`
//! with a real pipeline that:
//! 1. Fetches decoded on-chain state (bonding curve, global, mint) via
//!    `dyn LiveStateFetcher` — using a pre-warmed cache for lowest latency.
//! 2. Builds the unsigned transaction via `tx_build::build_pump_buy_message`
//!    or `build_pump_sell_message` — the production-grade builder with layout
//!    gate verification, real account lists, and fee-tail inclusion.
//! 3. Signs the compiled message bytes via `dyn LiveSigner` (ed25519, ~100μs).
//! 4. Assembles the wire transaction via `message::assemble_transaction`.
//! 5. Submits via `dyn LiveSubmitter` (Helius Sender or direct RPC).
//!
//! ## Fail-closed design
//! Every step returns `OutboundOutcome` on failure:
//! - State fetch failure → `StateFetch`
//! - Construction gate / layout refusal → `Construction`
//! - Signer refusal → `Signer`
//! - Sender rejection → `Sender`
//! - Success → `Accepted { signature }`
//!
//! The sink NEVER fabricates a signature. If any step fails, the outcome
//! records the failure class.
//!
//! ## Latency budget (hot path)
//! The `on_admit` call is the latency-critical surface. With a pre-warmed
//! cache (blockhash + bonding curve fetched by a background updater thread),
//! the hot path is:
//! - State fetch (cached): ~0ms
//! - tx_build (pure compute): ~μs
//! - ed25519 sign (pure compute): ~100μs
//! - Wire assemble (pure compute): ~μs
//! - Submit (network RTT): ~5-50ms (unavoidable, single round-trip)
//!
//! The only synchronous network I/O is the final submit. State and blockhash
//! are served from cache. If the cache is cold, the state fetch falls back to
//! a synchronous RPC round-trip (~50-100ms) — a degradation, not a failure.
//!
//! ## Operator refs
//! - §24(b): paper/replay mode is byte-identical — the `NoopSink` is used
//!   there; this sink is only wired in live mode.
//! - §36: the failure taxonomy classifies every rejection.
//! - §41: construction parity — the sink refuses to build if the
//!   LayoutRegistry has no verified fixture for the requested layout.
//! - Wallet history (2026-08-15): 941/1000 prior txs failed due to wrong
//!   token_program (IncorrectProgramId on ATA) and slippage (Custom:11 on
//!   buy). The fix is using tx_build with decoded token_program from the
//!   mint owner, never hardcoded.

use std::sync::Arc;

use crate::ex_live_io_traits::{LiveSigner, LiveStateFetcher, LiveSubmitter};
use crate::ex_outbound_sink::{AdmitRecord, OutboundOutcome, OutboundSink};

use pump_quant_protocol::curve_sell_quote::curve_buy_exact_in;
use pump_quant_protocol::ix::{BuyExactSolInParams, SellParams};
use pump_quant_protocol::layout::LayoutRegistry;
use pump_quant_protocol::message::{assemble_transaction, MessageError};
use pump_quant_protocol::tx_build::{
    build_pump_buy_exact_sol_in_message, build_pump_sell_message, BuildEnv, ComputePlan, TipPlan,
    TxBuildError,
};
use pump_quant_protocol::venue_accounts::FeeTail;

/// Configuration for the live outbound sink.
#[derive(Debug, Clone)]
pub struct LiveSinkConfig {
    /// Compute-budget envelope: CU limit and priority fee (micro-lamports/CU).
    pub compute: ComputePlan,
    /// Sender tip plan (destination + lamports). `None` = no tip (direct RPC,
    /// not Sender). When using Helius Sender, this is non-zero.
    pub tip: Option<TipPlan>,
    /// The fee-tail variant to emit. Must match the on-chain Global account's
    /// buyback_fee_recipients list (2026-08-03 finding: BuybackVault PDAs).
    pub fee_tail: FeeTail,
    /// Max slippage in basis points for the buy instruction's min_tokens_out.
    pub max_slippage_bps: u16,
    /// Curve venue fee rates `(protocol_bps, creator_bps)` the BUY quote is priced with — the same
    /// rates the paper fill uses (`pump_quant_app::exec_quote::curve_fees`). `None` refuses every
    /// BUY by name (`quote_unavailable`): a buy is never built without a quote for its clip.
    pub curve_buy_fee_bps: Option<(u64, u64)>,
    /// Rev-36: TOCTOU mcap band re-validation at execution time.
    /// When true, `on_admit` re-checks the mcap band using the FRESH vsol
    /// fetched from on-chain state at buy time — closing the gap where the
    /// gate approved an entry whose vsol later moved above the band.
    pub mcap_band_enable: bool,
    /// The mcap band low bound in lamports (inclusive).
    pub mcap_band_lo_lamports: u64,
    /// The mcap band high bound in lamports (inclusive).
    pub mcap_band_hi_lamports: u64,
}

/// The live outbound sink. Holds injected I/O trait objects + config.
///
/// In paper/replay mode the engine uses `NoopSink` instead. This sink is
/// only constructed when live arming is authorised by the operator.
///
/// The `Arc<dyn>` indirection is the cost of dynamic dispatch on the hot path
/// — one vtable lookup per trait call, negligible vs the network RTT on submit.
/// The alternative (generic type parameters) would monomorphise the sink for
/// every concrete I/O type and bloat the binary; the trait-object path is the
/// right trade for a single-call-per-admit sink.
pub struct LiveOutboundSink {
    config: LiveSinkConfig,
    registry: Arc<LayoutRegistry>,
    state_fetcher: Arc<dyn LiveStateFetcher>,
    signer: Arc<dyn LiveSigner>,
    submitter: Arc<dyn LiveSubmitter>,
}

impl LiveOutboundSink {
    /// Create a new live sink with injected I/O.
    ///
    /// All four dependencies are injected: the caller (daemon) constructs the
    /// concrete implementations and passes them in. The sink itself has no
    /// I/O construction — it is pure wiring.
    #[must_use]
    pub fn new(
        config: LiveSinkConfig,
        registry: Arc<LayoutRegistry>,
        state_fetcher: Arc<dyn LiveStateFetcher>,
        signer: Arc<dyn LiveSigner>,
        submitter: Arc<dyn LiveSubmitter>,
    ) -> Self {
        Self {
            config,
            registry,
            state_fetcher,
            signer,
            submitter,
        }
    }
}

/// Convert a `TxBuildError` into a human-readable construction-failure string.
fn build_err_str(e: &TxBuildError) -> String {
    format!("tx_build refused: {e:?}")
}

/// Convert a `MessageError` into a construction-failure string.
fn msg_err_str(e: &MessageError) -> String {
    format!("message assembly refused: {e:?}")
}

impl OutboundSink for LiveOutboundSink {
    fn on_admit(&self, record: &AdmitRecord) -> OutboundOutcome {
        // ── Step 0: Override the user pubkey with the real signer pubkey ──
        //
        // The engine passes `user: [0u8; 32]` in the AdmitRecord because it
        // doesn't know the signer's pubkey (the junction owns the signer).
        // The sink IS the junction — it holds the signer via `Arc<dyn
        // LiveSigner>`, so it can supply the real pubkey here. This is critical:
        // the state fetcher copies `user` into the PumpCurveCtx, which the tx
        // builder uses to construct the account list. A zero pubkey would
        // produce an invalid transaction that the Solana runtime rejects.
        let real_user = self.signer.public_key();

        // ── Step 1: Fetch decoded on-chain state (cached → ~0ms) ──────────
        //
        // The state fetcher returns a LiveCurveState with the decoded
        // PumpCurveCtx (mint, user, fee_recipient, creator, token_program,
        // is_cashback_coin, quote_mint) plus virtual reserves and is_complete.
        //
        // The token_program is decoded from the mint account's owner — this
        // is the fix for the 941-failure root cause: the old code hardcoded
        // spl-token, but many mints use Token-2022.
        let state = match self.state_fetcher.fetch_state_hot(&record.mint, &real_user) {
            Ok(s) => s,
            Err(e) => {
                return OutboundOutcome::StateFetch(format!("state fetch failed: {e:?}"));
            }
        };

        // A complete curve cannot be traded via the bonding-curve program.
        if state.is_complete {
            return OutboundOutcome::StateFetch(
                "bonding curve complete — graduated to pumpswap".to_string(),
            );
        }

        // ── Rev-36: TOCTOU mcap band re-validation ──────────────────────
        //
        // The gate checks the mcap band using a vsol snapshot from gate-eval
        // time, but the live sink fetches FRESH on-chain state here. Between
        // gate approval and this buy, other traders may have pumped SOL into
        // the curve, raising vsol and the mcap above the operator's band.
        // Without this re-check the bot buys at a higher mcap than the gate
        // ever approved — the "21 trades above 45 SOL cap" leak.
        //
        // mcap = vsol² / MCAP_DIVISOR (pump.fun constant-product curve).
        // MCAP_DIVISOR_LAMPORTS = 32_190_000_000 (inlined to avoid a cross-
        // crate dependency; the value is a venue constant, not a config knob).
        if record.is_buy && self.config.mcap_band_enable {
            const MCAP_DIVISOR_LAMPORTS: u128 = 32_190_000_000;
            let vsol = state.virtual_sol_reserves;
            if vsol > 0 {
                let v = u128::from(vsol);
                let mcap = v.saturating_mul(v) / MCAP_DIVISOR_LAMPORTS;
                let lo = u128::from(self.config.mcap_band_lo_lamports);
                let hi = u128::from(self.config.mcap_band_hi_lamports);
                if mcap < lo || mcap > hi {
                    return OutboundOutcome::StateFetch(format!(
                        "mcap band TOCTOU reject: fresh mcap {mcap} lamports outside [{lo}, {hi}] (vsol={vsol})"
                    ));
                }
            }
        }

        // ── Step 2: Build the unsigned transaction via tx_build ───────────
        //
        // tx_build::build_pump_buy_message / build_pump_sell_message:
        // - Build the real account list from the decoded PumpCurveCtx.
        // - Verify the layout against the LayoutRegistry (§41 parity gate).
        // - Prepend compute-budget instructions.
        // - Include the ATA-idempotent create for buys.
        // - Include the fee-tail (BuybackVault PDA, 2026-08-03 finding).
        // - Compile the message (shortvec account keys + header + bytes).
        //
        // The result is a CompiledMessage: the exact bytes to sign.

        // ── Per-mint BuybackVault selection (2026-08-17 fix for error 6062) ──
        //
        // The pump.fun program requires a BuybackVault PDA as the trailing
        // account in Buy and Sell instructions. The 8 BuybackVault addresses
        // are stored in the Global account's buyback_fee_recipients list
        // (offset 741, 8×32 bytes). The program selects one per-mint; our
        // on-chain analysis found the selection formula is `mint_bytes[0] % 8`
        // (verified against 3 successful direct bonding-curve txs).
        //
        // If the buyback_fee_recipients are all-zero (short Global account or
        // test fixture), we fall back to the configured fee_tail (which may
        // be FeeTail::None for paper/test mode).
        let fee_tail = {
            let recipients = &state.buyback_fee_recipients;
            let non_zero = recipients.iter().any(|r| *r != [0u8; 32]);
            if non_zero {
                let mint_first_byte = state.curve_ctx.mint[0];
                let idx = (mint_first_byte % 8) as usize;
                let bv_addr = recipients[idx];
                if bv_addr == [0u8; 32] {
                    // The selected slot is zero — try the next non-zero entry.
                    let mut chosen = [0u8; 32];
                    for r in recipients {
                        if *r != [0u8; 32] {
                            chosen = *r;
                            break;
                        }
                    }
                    if chosen != [0u8; 32] {
                        FeeTail::BuybackVault(chosen)
                    } else {
                        self.config.fee_tail
                    }
                } else {
                    FeeTail::BuybackVault(bv_addr)
                }
            } else {
                self.config.fee_tail
            }
        };

        let build_env = BuildEnv {
            compute: self.config.compute,
            tip: self.config.tip,
            recent_blockhash: match self.state_fetcher.latest_blockhash() {
                Ok(bh) => bh.blockhash,
                Err(e) => {
                    return OutboundOutcome::StateFetch(format!("blockhash fetch failed: {e:?}"));
                }
            },
            registry: &self.registry,
            fee_tail,
        };

        let compiled_msg = if record.is_buy {
            // W3/R5 (PLIM_CONTRACT.md): the live curve BUY is `buy_exact_sol_in` — spend EXACTLY the
            // clip the authority sized (fees inside), accept no fewer than `min_tokens_out`. The
            // legacy exact-OUT `buy` is no longer built for an entry: its `amount` is the token count
            // bought, so any bound written there re-sized the trade.
            let vtokens = state.virtual_token_reserves;
            let vsol = state.virtual_sol_reserves;
            if vsol == 0 {
                return OutboundOutcome::StateFetch(
                    "virtual_sol_reserves is zero — cannot compute min_tokens".to_string(),
                );
            }
            // The clip's quote on the FRESH state, with the same arithmetic and fee rates the paper
            // fill uses (`curve_buy_exact_in`). No rates configured => no quote => named refusal.
            let Some((protocol_bps, creator_bps)) = self.config.curve_buy_fee_bps else {
                return OutboundOutcome::Construction(
                    "quote_unavailable: curve buy fee rates not configured".to_string(),
                );
            };
            // `LiveCurveState` carries no real-token reserve, so the cap is not applied here (the
            // program's own `min_tokens_out` check still binds on chain). PLIM_CONTRACT.md §S5 gap.
            let Some(quote) = curve_buy_exact_in(
                vsol,
                vtokens,
                u64::MAX,
                record.size_lamports,
                protocol_bps,
                creator_bps,
            ) else {
                return OutboundOutcome::Construction(format!(
                    "quote_unavailable: curve buy of {} lamports is unpriceable",
                    record.size_lamports
                ));
            };
            let quoted_tokens = u64::try_from(quote.tokens_out).unwrap_or(u64::MAX);
            // No limit (68% of trained BUYs): the slippage floor off the clip's OWN quote.
            let slippage_factor = 10_000u32.saturating_sub(self.config.max_slippage_bps as u32);
            let slippage_min_tokens =
                (u128::from(quoted_tokens) * slippage_factor as u128 / 10_000u128) as u64;
            // The brain's PRICE LIMIT is a floor on tokens for THIS clip. A present-but-unconvertible
            // limit, or a quote below the floor, is a named refusal — never a smaller clip, a widened
            // limit, the slippage count, or a fallback instruction.
            let plan = match crate::price_anchor::plan_curve_buy(
                record.size_lamports,
                record.price_limit_lamports_per_raw_token,
                Some(quoted_tokens),
                slippage_min_tokens,
            ) {
                Ok(p) => p,
                Err(r) => {
                    return OutboundOutcome::Construction(format!(
                        "price_{}: {r:?} (limit {:?} lamports/raw token, size {} lamports)",
                        r.label(),
                        record.price_limit_lamports_per_raw_token,
                        record.size_lamports
                    ));
                }
            };
            let params = BuyExactSolInParams {
                spendable_sol_in: plan.spendable_sol_in,
                min_tokens_out: plan.min_tokens_out,
            };

            match build_pump_buy_exact_sol_in_message(&state.curve_ctx, params, &build_env) {
                Ok(msg) => msg,
                Err(e) => return OutboundOutcome::Construction(build_err_str(&e)),
            }
        } else {
            // Sell: fetch the REAL on-chain ATA balance before building the
            // sell instruction. The paper-computed `record.size_lamports`
            // (from exit_token_amount) can overestimate the actual balance
            // due to price drift → on-chain error 6023 (NotEnoughTokensToSell).
            // Using the real balance guarantees the sell succeeds.
            let ata_balance = match self.state_fetcher.fetch_ata_balance(
                &record.mint,
                &real_user,
                &state.curve_ctx.token_program,
            ) {
                Ok(bal) => bal,
                Err(e) => {
                    return OutboundOutcome::StateFetch(format!("ATA balance fetch failed: {e:?}"));
                }
            };

            // If the ATA has 0 tokens, skip the sell (fail-safe — nothing to sell).
            if ata_balance == 0 {
                return OutboundOutcome::StateFetch(
                    "ATA balance is 0 — nothing to sell".to_string(),
                );
            }

            // Use the real on-chain balance, capped at the paper-computed amount
            // to never sell more than the position tracking says we hold.
            let sell_amount = ata_balance.min(record.size_lamports);
            if sell_amount == 0 {
                return OutboundOutcome::StateFetch(
                    "sell_amount is 0 after min(ata, paper)".to_string(),
                );
            }

            let vtokens = state.virtual_token_reserves;
            let vsol = state.virtual_sol_reserves;
            if vtokens == 0 {
                return OutboundOutcome::StateFetch(
                    "virtual_token_reserves is zero — cannot compute min_sol".to_string(),
                );
            }
            // expected_sol = sell_amount * vsol / vtokens
            let expected_sol =
                (sell_amount as u128).saturating_mul(vsol as u128) / (vtokens as u128);
            let slippage_factor = 10_000u32.saturating_sub(self.config.max_slippage_bps as u32);
            let min_sol = (expected_sol * slippage_factor as u128 / 10_000u128) as u64;

            // ── LOCAL SELL PREFLIGHT (E4) ─────────────────────────────────────
            // The Sender endpoint cannot run a preflight check (it answers HTTP 500
            // "running preflight check is not supported" for skip_preflight=false), so a
            // sell whose own `min_sol_out` can never be met would burn a submit and only
            // surface as an on-chain failure. Simulate the curve sell against the SAME
            // fresh reserves the bound is priced from, using the ported constant-product
            // simulation (no second copy of the fill math), and refuse locally with a
            // named cause. The simulated amount is an UPPER bound — the curve's fee is
            // not in the model — so this can only refuse a sell that is provably
            // unsendable, never one that would have filled.
            {
                use crate::si_incident_gate::{simulate_sell, DecodedMarket, Position};
                let simulated = simulate_sell(
                    &Position {
                        token_amount: sell_amount,
                        mint: 0,
                    },
                    &DecodedMarket {
                        base_reserve: vtokens,
                        quote_reserve: vsol,
                        constructible: true,
                    },
                );
                match simulated {
                    Ok(proof) if proof.out_amount >= min_sol => {}
                    Ok(proof) => {
                        return OutboundOutcome::Construction(format!(
                            "local sell preflight refused: simulated out {} < min_sol_out {} \
                             (own impact exceeds the {} bp slippage bound)",
                            proof.out_amount, min_sol, self.config.max_slippage_bps
                        ));
                    }
                    Err(e) => {
                        return OutboundOutcome::Construction(format!(
                            "local sell preflight refused: {e:?}"
                        ));
                    }
                }
            }

            let params = SellParams {
                token_amount: sell_amount,
                min_sol_out: min_sol,
            };

            // Only close the ATA if we're selling the ENTIRE balance (full exit).
            // If ata_balance <= sell_amount, we're selling everything → close OK.
            // If ata_balance > sell_amount (partial ladder rung), don't close.
            // This prevents Custom:11 (CloseAccount fails when dust remains).
            let close_token_account = ata_balance <= sell_amount;

            match build_pump_sell_message(&state.curve_ctx, params, &build_env, close_token_account)
            {
                Ok(msg) => msg,
                Err(e) => return OutboundOutcome::Construction(build_err_str(&e)),
            }
        };

        // ── Step 3: Sign the compiled message (~100μs, pure compute) ──────
        //
        // The signer signs the message bytes (the exact wire-format bytes
        // that will be assembled into the transaction). The signature is
        // 64 bytes of ed25519.
        let signature = match self.signer.sign(&compiled_msg.bytes) {
            Ok(sig) => sig,
            Err(e) => {
                return OutboundOutcome::Signer(format!("signing failed: {e:?}"));
            }
        };

        // ── Step 4: Assemble the wire transaction (~μs, pure compute) ─────
        //
        // wire = shortvec(n_sigs) ‖ sigs ‖ message_bytes
        // The signature order must match the signer order in the message
        // (payer first). For a single-signer transaction (our case), there
        // is exactly one signature.
        let signatures = vec![signature];
        let wire_tx = match assemble_transaction(&compiled_msg, &signatures) {
            Ok(wire) => wire,
            Err(e) => return OutboundOutcome::Construction(msg_err_str(&e)),
        };

        // ── Step 5: Submit via Helius Sender / RPC (~5-50ms RTT) ──────────
        //
        // This is the only network I/O on the hot path. The submitter
        // base64-encodes the wire bytes and POSTs to the Sender endpoint.
        // For lowest latency, we return the signature on sendTransaction
        // success without waiting for full confirmation — the reconciliation
        // layer confirms asynchronously.
        // E11: time the submit leg on its own. `submit_call_us` (measured by the
        // engine around this whole call) folds in the state fetch and the local
        // build/sign work; this is the pure network round trip.
        let t_submit = std::time::Instant::now();
        let on_chain_sig = match self.submitter.submit(&wire_tx, record.is_buy) {
            Ok(sig) => sig,
            Err(e) => {
                return OutboundOutcome::Sender(format!("submission failed: {e:?}"));
            }
        };

        // ── Success ───────────────────────────────────────────────────────
        //
        // The on-chain signature is the transaction signature returned by
        // the RPC/Sender endpoint. This is a real, non-fabricated signature
        // from a real on-chain submission.
        OutboundOutcome::Accepted {
            signature: on_chain_sig,
            submit_rpc_us: t_submit.elapsed().as_micros() as u64,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ex_live_io_traits::{
        LiveBlockhash, LiveCurveState, LiveSigner, LiveStateFetcher, LiveSubmitter, SignError,
        StateFetchError, SubmitError,
    };
    use pump_quant_protocol::venue_accounts::PumpCurveCtx;

    /// A test signer that signs with a fixed key — verifies the sign+assemble
    /// path produces a non-zero signature.
    struct TestSigner {
        pk: [u8; 32],
    }

    impl LiveSigner for TestSigner {
        fn sign(&self, _message: &[u8]) -> Result<[u8; 64], SignError> {
            // Return a deterministic non-zero signature for testing.
            let mut sig = [0u8; 64];
            for (i, b) in sig.iter_mut().enumerate() {
                *b = (i as u8).wrapping_add(1);
            }
            Ok(sig)
        }

        fn public_key(&self) -> [u8; 32] {
            self.pk
        }
    }

    /// A test submitter that returns a fixed on-chain signature.
    struct TestSubmitter;

    impl LiveSubmitter for TestSubmitter {
        fn submit(&self, _wire_tx: &[u8], _is_buy: bool) -> Result<[u8; 64], SubmitError> {
            let mut sig = [0u8; 64];
            sig[0] = 0xAA;
            sig[63] = 0xBB;
            Ok(sig)
        }
    }

    /// A test state fetcher that returns fixed state.
    struct TestStateFetcher {
        blockhash: LiveBlockhash,
    }

    impl LiveStateFetcher for TestStateFetcher {
        fn fetch_state_hot(
            &self,
            _mint: &[u8; 32],
            _user: &[u8; 32],
        ) -> Result<LiveCurveState, StateFetchError> {
            // Use a non-zero mint/user to pass validation.
            let mut mint = [0u8; 32];
            mint[0] = 1;
            let mut user = [0u8; 32];
            user[0] = 2;
            let mut fee_recipient = [0u8; 32];
            fee_recipient[0] = 3;
            let mut creator = [0u8; 32];
            creator[0] = 4;

            let ctx = PumpCurveCtx {
                mint,
                user,
                fee_recipient,
                creator,
                token_program: pump_quant_protocol::venue_accounts::TOKEN_PROGRAM_ID,
                is_cashback_coin: false,
                quote_mint: pump_quant_protocol::venue_accounts::WSOL_MINT,
            };

            Ok(LiveCurveState {
                curve_ctx: ctx,
                virtual_sol_reserves: 50_000_000_000, // 50 SOL in lamports
                virtual_token_reserves: 1_000_000_000_000, // ~1e12 tokens
                is_complete: false,
                observed_slot: 440_000_000,
                buyback_fee_recipients: [[0u8; 32]; 8], // test: no BV (FeeTail::None fallback)
            })
        }

        fn prefetch_state(
            &self,
            _mint: &[u8; 32],
            _user: &[u8; 32],
        ) -> Result<LiveCurveState, StateFetchError> {
            self.fetch_state_hot(_mint, _user)
        }

        fn latest_blockhash(&self) -> Result<LiveBlockhash, StateFetchError> {
            Ok(self.blockhash)
        }

        fn fetch_ata_balance(
            &self,
            _mint: &[u8; 32],
            _user: &[u8; 32],
            _token_program: &[u8; 32],
        ) -> Result<u64, StateFetchError> {
            // Return a non-zero balance so the sell path proceeds in tests.
            Ok(500_000_000_000)
        }
    }

    #[test]
    fn live_sink_does_not_fabricate_acceptance() {
        // This test verifies the fail-closed property: without real I/O
        // injection, the sink cannot be constructed. With injected I/O,
        // the sink produces a real (non-zero) signature on success.
        //
        // The old test checked that the sink returned Signer (fail-closed).
        // The new sink requires I/O injection — if you construct it, it
        // produces real signatures. The fail-closed property is now at
        // construction time: you cannot construct a LiveOutboundSink without
        // a real signer and submitter.
        //
        // We verify the non-zero signature property here.

        let mut blockhash = [0u8; 32];
        blockhash[0] = 0x42;

        let fetcher = TestStateFetcher {
            blockhash: LiveBlockhash {
                blockhash,
                slot: 440_000_000,
                last_valid_block_height: 440_000_150,
            },
        };

        // We need a LayoutRegistry with a verified entry for the pump.fun
        // buy layout. An empty registry will refuse the build — that's the
        // §41 parity gate working. For this test we verify the gate fires.
        let registry = Arc::new(LayoutRegistry::new());

        let mut signer_pk = [0u8; 32];
        signer_pk[0] = 0xFF;

        let sink = LiveOutboundSink::new(
            LiveSinkConfig {
                compute: ComputePlan {
                    unit_limit: 200_000,
                    unit_price_micro_lamports: 100_000,
                },
                tip: None,
                fee_tail: FeeTail::None,
                max_slippage_bps: 500,
                curve_buy_fee_bps: Some((95, 30)),
                mcap_band_enable: false, // test sink: no TOCTOU re-check
                mcap_band_lo_lamports: 0,
                mcap_band_hi_lamports: u64::MAX,
            },
            registry,
            Arc::new(fetcher),
            Arc::new(TestSigner { pk: signer_pk }),
            Arc::new(TestSubmitter),
        );

        let record = AdmitRecord {
            mint: {
                let mut m = [0u8; 32];
                m[0] = 1;
                m
            },
            user: {
                let mut u = [0u8; 32];
                u[0] = 2;
                u
            },
            is_buy: true,
            size_lamports: 100_000_000, // 0.1 SOL
            entry_price: 28_000,
            max_slippage_bps: 500,
            // The model's price limit is not plumbed to this call site yet:
            // when the Qwen wiring lands, the engine fills it in from the
            // parsed decision. Until then the slippage budget protects the order.
            price_limit_lamports_per_raw_token: None,
        };

        let outcome = sink.on_admit(&record);

        // With an empty LayoutRegistry, the build should fail with
        // Construction (the layout gate refuses). This proves the §41
        // parity gate is active — we never bypass it.
        match &outcome {
            OutboundOutcome::Construction(msg) => {
                // Expected: layout not verified.
                assert!(
                    msg.contains("tx_build refused"),
                    "expected construction refusal, got: {msg}"
                );
            }
            OutboundOutcome::Accepted { signature, .. } => {
                // If somehow the registry had entries, verify non-zero sig.
                assert!(
                    *signature != [0u8; 64],
                    "live sink must never return a zero-signature acceptance"
                );
            }
            _ => {
                // StateFetch or other is also acceptable in test context
                // (the TestStateFetcher's curve_ctx might trigger validation
                // issues). The key property: we never get a zero-sig Accepted.
            }
        }

        // The critical assertion: we NEVER get a zero-signature Accepted.
        // That's NoopSink's job, not the live sink's.
        if let OutboundOutcome::Accepted { signature, .. } = &outcome {
            assert!(
                *signature != [0u8; 64],
                "live sink must never fabricate a zero-signature acceptance"
            );
        }
    }

    #[test]
    fn noop_sink_returns_zero_signature() {
        let sink = crate::ex_outbound_sink::NoopSink;
        let record = AdmitRecord {
            mint: [0u8; 32],
            user: [0u8; 32],
            is_buy: true,
            size_lamports: 0,
            entry_price: 0,
            max_slippage_bps: 0,
            // The model's price limit is not plumbed to this call site yet:
            // when the Qwen wiring lands, the engine fills it in from the
            // parsed decision. Until then the slippage budget protects the order.
            price_limit_lamports_per_raw_token: None,
        };
        let outcome = sink.on_admit(&record);
        match outcome {
            OutboundOutcome::Accepted { signature, .. } => assert!(signature == [0u8; 64]),
            _ => panic!("noop sink must accept with zero signature"),
        }
    }

    /// W3: a PRESENT limit that cannot become a token count is refused by name before any build, never
    /// replaced by the slippage fallback. The empty registry makes any build that IS reached refuse with
    /// "tx_build refused", so the message tells which branch ran. Overflow is the SOL-per-raw-token unit
    /// mistake (1e-9 x the lamports price); Unusable is a NaN/negative from a caller that skipped the parser.
    /// (RoundsToZero is unreachable for a positive size: ceil of a positive quotient is >= 1.)
    #[test]
    fn a_present_but_unconvertible_limit_is_refused_not_replaced_by_slippage() {
        let sink = sell_test_sink();
        let buy = |limit: Option<f64>| AdmitRecord {
            mint: [1u8; 32],
            user: [0u8; 32],
            is_buy: true,
            size_lamports: 500_000_000,
            entry_price: 0,
            max_slippage_bps: 500,
            price_limit_lamports_per_raw_token: limit,
        };
        for (limit, want) in [
            (2.0e-13, "Overflow"),
            (f64::NAN, "Unusable"),
            (-1.0, "Unusable"),
        ] {
            match sink.on_admit(&buy(Some(limit))) {
                OutboundOutcome::Construction(m) => assert!(
                    m.starts_with("price_limit_unusable") && m.contains(want),
                    "{limit}: {m}"
                ),
                o => panic!("{limit}: expected a named refusal, got {o:?}"),
            }
        }
        // A usable limit the clip's quote meets, and no limit, both reach the (gated) build. The
        // test curve's spot is 0.05 lamports/raw token; a 0.5 SOL clip averages ~0.0511 all-in.
        for limit in [Some(0.06), None] {
            match sink.on_admit(&buy(limit)) {
                OutboundOutcome::Construction(m) => {
                    assert!(m.contains("tx_build refused"), "{limit:?}: {m}")
                }
                o => panic!("{limit:?}: {o:?}"),
            }
        }
    }

    /// A live sink with the fixed test I/O: a 50 SOL / 1e12-token curve and a 500e9-token
    /// ATA balance, over an EMPTY layout registry (so a build that is reached still refuses).
    fn sell_test_sink() -> LiveOutboundSink {
        let mut blockhash = [0u8; 32];
        blockhash[0] = 0x42;
        let fetcher = TestStateFetcher {
            blockhash: LiveBlockhash {
                blockhash,
                slot: 440_000_000,
                last_valid_block_height: 440_000_150,
            },
        };
        let registry = Arc::new(LayoutRegistry::new());
        let mut signer_pk = [0u8; 32];
        signer_pk[0] = 0xFF;
        LiveOutboundSink::new(
            LiveSinkConfig {
                compute: ComputePlan {
                    unit_limit: 200_000,
                    unit_price_micro_lamports: 100_000,
                },
                tip: None,
                fee_tail: FeeTail::None,
                max_slippage_bps: 500,
                curve_buy_fee_bps: Some((95, 30)),
                mcap_band_enable: false,
                mcap_band_lo_lamports: 0,
                mcap_band_hi_lamports: u64::MAX,
            },
            registry,
            Arc::new(fetcher),
            Arc::new(TestSigner { pk: signer_pk }),
            Arc::new(TestSubmitter),
        )
    }

    fn sell_record(size_lamports: u64) -> AdmitRecord {
        AdmitRecord {
            mint: {
                let mut m = [0u8; 32];
                m[0] = 1;
                m
            },
            user: {
                let mut u = [0u8; 32];
                u[0] = 2;
                u
            },
            is_buy: false,
            size_lamports,
            entry_price: 28_000,
            max_slippage_bps: 500,
            price_limit_lamports_per_raw_token: None,
        }
    }

    /// E4: the local sell preflight is the ONLY guard between a sell whose own
    /// `min_sol_out` cannot be met and a wasted submit — the Sender cannot preflight. It
    /// must refuse that sell locally, and must NOT over-refuse one that can fill.
    #[test]
    fn local_sell_preflight_refuses_only_the_unmeetable_sell() {
        let sink = sell_test_sink();

        // 5e11 raw tokens against a 1e12-token / 50 SOL curve: the constant-product out is
        // ~2/3 of the linear expectation, far below a 95% `min_sol_out`, so the tx would
        // revert on chain. It must never reach the wire.
        match sink.on_admit(&sell_record(1_000_000_000_000)) {
            OutboundOutcome::Construction(msg) => assert!(
                msg.contains("local sell preflight refused"),
                "expected a local sell preflight refusal, got: {msg}"
            ),
            _ => panic!("an unmeetable sell must be refused locally, before build/submit"),
        }

        // ~0.1% of the curve: clears the same bound, so control must reach the §41 layout
        // gate (which refuses on the empty registry) — proving the preflight is not blanket.
        match sink.on_admit(&sell_record(1_000_000_000)) {
            OutboundOutcome::Construction(msg) => assert!(
                msg.contains("tx_build refused"),
                "a meetable sell must reach the builder, got: {msg}"
            ),
            OutboundOutcome::Accepted { .. } => {}
            _ => panic!("a meetable sell must not be refused by the preflight"),
        }
    }

    // ───────────── W3/R5: buy_exact_sol_in construction (no transaction is ever sent) ─────────────

    use std::sync::Mutex;

    /// Fetcher with settable reserves and a non-zero BuybackVault list (so the buy is 18 accounts).
    struct CurveFetcher {
        vsol: u64,
        vtok: u64,
    }

    fn bv(i: u8) -> [u8; 32] {
        let mut k = [0x60u8; 32];
        k[0] = i;
        k
    }

    impl LiveStateFetcher for CurveFetcher {
        fn fetch_state_hot(
            &self,
            _mint: &[u8; 32],
            _user: &[u8; 32],
        ) -> Result<LiveCurveState, StateFetchError> {
            let mut st = TestStateFetcher {
                blockhash: test_blockhash(),
            }
            .fetch_state_hot(_mint, _user)?;
            st.virtual_sol_reserves = self.vsol;
            st.virtual_token_reserves = self.vtok;
            st.buyback_fee_recipients = core::array::from_fn(|i| bv(i as u8 + 1));
            Ok(st)
        }
        fn prefetch_state(
            &self,
            m: &[u8; 32],
            u: &[u8; 32],
        ) -> Result<LiveCurveState, StateFetchError> {
            self.fetch_state_hot(m, u)
        }
        fn latest_blockhash(&self) -> Result<LiveBlockhash, StateFetchError> {
            Ok(test_blockhash())
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

    fn test_blockhash() -> LiveBlockhash {
        let mut blockhash = [0u8; 32];
        blockhash[0] = 0x42;
        LiveBlockhash {
            blockhash,
            slot: 440_000_000,
            last_valid_block_height: 440_000_150,
        }
    }

    /// Records the message bytes it is asked to sign. Nothing is submitted anywhere: the paired
    /// submitter below refuses, so a build that is reached ends as `Submit` — never on a wire.
    struct CapturingSigner {
        pk: [u8; 32],
        seen: Arc<Mutex<Vec<Vec<u8>>>>,
    }
    impl LiveSigner for CapturingSigner {
        fn sign(&self, m: &[u8]) -> Result<[u8; 64], SignError> {
            self.seen.lock().unwrap().push(m.to_vec());
            Ok([7u8; 64])
        }
        fn public_key(&self) -> [u8; 32] {
            self.pk
        }
    }
    struct RefusingSubmitter;
    impl LiveSubmitter for RefusingSubmitter {
        fn submit(&self, _w: &[u8], _b: bool) -> Result<[u8; 64], SubmitError> {
            Err(SubmitError::EndpointRejected(
                "test: submission is disabled".to_string(),
            ))
        }
    }

    fn verified_buy_registry() -> LayoutRegistry {
        use pump_quant_protocol::layout::{LayoutKey, Side, Variant, Venue, VerifiedLayout};
        let mut r = LayoutRegistry::new();
        let mut sig = [0u8; 64];
        for (i, b) in sig.iter_mut().enumerate() {
            *b = ((i as u32 * 37 + 11) % 256) as u8;
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

    fn capture_sink(vsol: u64, vtok: u64) -> (LiveOutboundSink, Arc<Mutex<Vec<Vec<u8>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut pk = [0u8; 32];
        pk[0] = 2; // == the fetched ctx's user
        let sink = LiveOutboundSink::new(
            LiveSinkConfig {
                compute: ComputePlan {
                    unit_limit: 200_000,
                    unit_price_micro_lamports: 100_000,
                },
                tip: None,
                fee_tail: FeeTail::None,
                max_slippage_bps: 500,
                curve_buy_fee_bps: Some((95, 30)),
                mcap_band_enable: false,
                mcap_band_lo_lamports: 0,
                mcap_band_hi_lamports: u64::MAX,
            },
            Arc::new(verified_buy_registry()),
            Arc::new(CurveFetcher { vsol, vtok }),
            Arc::new(CapturingSigner {
                pk,
                seen: Arc::clone(&seen),
            }),
            Arc::new(RefusingSubmitter),
        );
        (sink, seen)
    }

    fn buy_rec(size: u64, limit: Option<f64>) -> AdmitRecord {
        AdmitRecord {
            mint: [1u8; 32],
            user: [0u8; 32],
            is_buy: true,
            size_lamports: size,
            entry_price: 0,
            max_slippage_bps: 500,
            price_limit_lamports_per_raw_token: limit,
        }
    }

    /// The pump instruction's 25-byte data inside a signed message: located by discriminator.
    fn pump_data(msg: &[u8]) -> [u8; 25] {
        use pump_quant_protocol::ix::BUY_EXACT_SOL_IN_DISCRIMINATOR as D;
        let at = msg
            .windows(8)
            .position(|w| w == D)
            .expect("buy_exact_sol_in discriminator in the message");
        assert!(
            !msg.windows(8)
                .any(|w| w == pump_quant_protocol::ix::BUY_DISCRIMINATOR),
            "the exact-OUT `buy` must never be built for an entry"
        );
        msg[at..at + 25].try_into().unwrap()
    }

    fn args(d: &[u8; 25]) -> (u64, u64, u8) {
        (
            u64::from_le_bytes(d[8..16].try_into().unwrap()),
            u64::from_le_bytes(d[16..24].try_into().unwrap()),
            d[24],
        )
    }

    /// The exact bytes `on_admit` hands the signer for a BUY are the protocol builder's
    /// `buy_exact_sol_in` message with spend = the clip and min_tokens_out = the exact floor.
    #[test]
    fn the_live_buy_is_buy_exact_sol_in_spending_the_clip_with_the_limit_as_a_floor() {
        use pump_quant_protocol::tx_build::build_pump_buy_exact_sol_in_message;
        let (vsol, vtok) = (80_000_000_000u64, 402_375_000_000_000u64);
        let (sink, seen) = capture_sink(vsol, vtok);
        let clip = 500_000_000u64;
        let limit = 0.000_21; // above the FULL all-in average 0.0002025...
        match sink.on_admit(&buy_rec(clip, Some(limit))) {
            OutboundOutcome::Sender(m) => assert!(m.contains("disabled"), "{m}"),
            o => panic!("expected the build to be reached and submission refused: {o:?}"),
        }
        let msgs = seen.lock().unwrap().clone();
        assert_eq!(msgs.len(), 1);
        let d = pump_data(&msgs[0]);
        let floor = crate::price_anchor::min_tokens_from_price_limit(clip, limit).unwrap();
        assert_eq!(args(&d), (clip, floor, 0));
        // Byte-identical to the protocol builder on the same ctx/env/params.
        let st = CurveFetcher { vsol, vtok }
            .fetch_state_hot(&[1u8; 32], &[2u8; 32])
            .unwrap();
        let reg = verified_buy_registry();
        let want = build_pump_buy_exact_sol_in_message(
            &st.curve_ctx,
            BuyExactSolInParams {
                spendable_sol_in: clip,
                min_tokens_out: floor,
            },
            &BuildEnv {
                compute: ComputePlan {
                    unit_limit: 200_000,
                    unit_price_micro_lamports: 100_000,
                },
                tip: None,
                recent_blockhash: test_blockhash().blockhash,
                registry: &reg,
                fee_tail: FeeTail::BuybackVault(bv(1 + (st.curve_ctx.mint[0] % 8))),
            },
        )
        .unwrap();
        assert_eq!(msgs[0], want.bytes);
    }

    /// No limit: still exact-SOL-in, spend = clip, floor = the clip's OWN quote less the slippage
    /// budget (not the old linear `size*vtok/vsol`, which ignored fees and impact).
    #[test]
    fn an_absent_limit_spends_the_clip_with_a_quote_based_slippage_floor() {
        let (vsol, vtok) = (80_000_000_000u64, 402_375_000_000_000u64);
        let (sink, seen) = capture_sink(vsol, vtok);
        let clip = 500_000_000u64;
        let _ = sink.on_admit(&buy_rec(clip, None));
        let d = pump_data(&seen.lock().unwrap()[0]);
        let q = curve_buy_exact_in(vsol, vtok, u64::MAX, clip, 95, 30).unwrap();
        let want = (q.tokens_out * 9_500 / 10_000) as u64;
        assert_eq!(args(&d), (clip, want, 0));
    }

    /// S4: the +187.5 bp case. A limit equal to the marginal (prompt) price cannot be met by the FULL
    /// clip; the sink refuses by name and NOTHING is signed — no smaller clip, no widened limit, no
    /// slippage count, no exact-OUT fallback.
    #[test]
    fn a_limit_the_clip_cannot_meet_is_refused_by_name_and_nothing_is_signed() {
        let (vsol, vtok) = (80_000_000_000u64, 402_375_000_000_000u64);
        let (sink, seen) = capture_sink(vsol, vtok);
        let supplied = 198_819e-9;
        match sink.on_admit(&buy_rec(500_000_000, Some(supplied))) {
            OutboundOutcome::Construction(m) => {
                assert!(m.starts_with("price_limit_not_met"), "{m}");
                assert!(
                    m.contains("2468558269818") && m.contains("2514850190375"),
                    "{m}"
                );
            }
            o => panic!("{o:?}"),
        }
        assert!(seen.lock().unwrap().is_empty(), "nothing may be signed");
        // A missing fee configuration is a named refusal too, never an unquoted build.
        let mut s2 = capture_sink(vsol, vtok).0;
        s2.config.curve_buy_fee_bps = None;
        match s2.on_admit(&buy_rec(500_000_000, None)) {
            OutboundOutcome::Construction(m) => assert!(m.starts_with("quote_unavailable"), "{m}"),
            o => panic!("{o:?}"),
        }
    }

    /// Reserves moving between the decision and the build: the floor is a function of (clip, limit)
    /// only, so it is the same bytes on every state; the FRESH quote decides build vs named refusal.
    /// On chain, a move after the build is caught by the program's own `min_tokens_out` (6042).
    #[test]
    fn reserves_moving_before_the_build_keep_the_floor_and_refuse_by_name() {
        let k = 30_000_000_000u128 * 1_073_000_000_000_000u128;
        let pool = |vsol: u64| (vsol, (k / u128::from(vsol)) as u64);
        let clip = 500_000_000u64;
        let limit = 198_819e-9 * 1.05;
        let floor = crate::price_anchor::min_tokens_from_price_limit(clip, limit).unwrap();
        let mut built = Vec::new();
        for vsol in [80_000_000_000u64, 76_000_000_000] {
            let (vs, vt) = pool(vsol);
            let (sink, seen) = capture_sink(vs, vt);
            let _ = sink.on_admit(&buy_rec(clip, Some(limit)));
            let d = pump_data(&seen.lock().unwrap()[0]);
            built.push(args(&d));
        }
        assert_eq!(built, vec![(clip, floor, 0), (clip, floor, 0)]);
        let (vs, vt) = pool(84_000_000_000);
        let (sink, seen) = capture_sink(vs, vt);
        match sink.on_admit(&buy_rec(clip, Some(limit))) {
            OutboundOutcome::Construction(m) => {
                assert!(m.starts_with("price_limit_not_met"), "{m}")
            }
            o => panic!("{o:?}"),
        }
        assert!(seen.lock().unwrap().is_empty());
    }
}
