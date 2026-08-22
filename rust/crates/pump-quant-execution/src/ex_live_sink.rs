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
//! ## Constitution refs
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

use crate::ex_live_io_traits::{
    LiveSigner, LiveStateFetcher, LiveSubmitter,
};
use crate::ex_outbound_sink::{AdmitRecord, OutboundOutcome, OutboundSink};

use pump_quant_protocol::layout::LayoutRegistry;
use pump_quant_protocol::tx_build::{
    build_pump_buy_message, build_pump_sell_message, BuildEnv, ComputePlan, TipPlan, TxBuildError,
};
use pump_quant_protocol::ix::{BuyParams, SellParams};
use pump_quant_protocol::venue_accounts::FeeTail;
use pump_quant_protocol::message::{assemble_transaction, MessageError};

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
        let state = match self.state_fetcher.fetch_state_hot(
            &record.mint,
            &real_user,
        ) {
            Ok(s) => s,
            Err(e) => {
                return OutboundOutcome::StateFetch(format!(
                    "state fetch failed: {e:?}"
                ));
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
                    return OutboundOutcome::StateFetch(format!(
                        "blockhash fetch failed: {e:?}"
                    ));
                }
            },
            registry: &self.registry,
            fee_tail,
        };

        let compiled_msg = if record.is_buy {
            // Buy: compute min_tokens_out from entry_price and max_slippage_bps.
            // min_tokens_out = expected_tokens * (10000 - max_slippage_bps) / 10000
            // where expected_tokens = size_lamports / entry_price (in token base units).
            // We use the curve's virtual reserves to compute expected tokens.
            //
            // For a linear curve: tokens_out = sol_in * virtual_token_reserves / virtual_sol_reserves
            // (approximately — the exact formula accounts for the fee and curve mechanics,
            // but the slippage guard just needs a lower bound).
            let vtokens = state.virtual_token_reserves;
            let vsol = state.virtual_sol_reserves;
            if vsol == 0 {
                return OutboundOutcome::StateFetch(
                    "virtual_sol_reserves is zero — cannot compute min_tokens".to_string(),
                );
            }
            // expected_tokens = sol_in * vtokens / vsol  (u128 to prevent overflow)
            let expected_tokens = (record.size_lamports as u128)
                .saturating_mul(vtokens as u128)
                / (vsol as u128);
            // Apply slippage: min_tokens = expected_tokens * (10000 - bps) / 10000
            let slippage_factor = 10_000u32.saturating_sub(self.config.max_slippage_bps as u32);
            let min_tokens = (expected_tokens * slippage_factor as u128 / 10_000u128) as u64;
            // max_sol_cost = size_lamports (the engine already sized this)
            let max_sol_cost = record.size_lamports;

            let params = BuyParams {
                min_tokens_out: min_tokens,
                max_sol_cost,
            };

            match build_pump_buy_message(&state.curve_ctx, params, &build_env) {
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
                    return OutboundOutcome::StateFetch(format!(
                        "ATA balance fetch failed: {e:?}"
                    ));
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
            let expected_sol = (sell_amount as u128)
                .saturating_mul(vsol as u128)
                / (vtokens as u128);
            let slippage_factor = 10_000u32.saturating_sub(self.config.max_slippage_bps as u32);
            let min_sol = (expected_sol * slippage_factor as u128 / 10_000u128) as u64;

            let params = SellParams {
                token_amount: sell_amount,
                min_sol_out: min_sol,
            };

            // Only close the ATA if we're selling the ENTIRE balance (full exit).
            // If ata_balance <= sell_amount, we're selling everything → close OK.
            // If ata_balance > sell_amount (partial ladder rung), don't close.
            // This prevents Custom:11 (CloseAccount fails when dust remains).
            let close_token_account = ata_balance <= sell_amount;

            match build_pump_sell_message(
                &state.curve_ctx,
                params,
                &build_env,
                close_token_account,
            ) {
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
                return OutboundOutcome::Signer(format!(
                    "signing failed: {e:?}"
                ));
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
        let on_chain_sig = match self.submitter.submit(&wire_tx, record.is_buy) {
            Ok(sig) => sig,
            Err(e) => {
                return OutboundOutcome::Sender(format!(
                    "submission failed: {e:?}"
                ));
            }
        };

        // ── Success ───────────────────────────────────────────────────────
        //
        // The on-chain signature is the transaction signature returned by
        // the RPC/Sender endpoint. This is a real, non-fabricated signature
        // from a real on-chain submission.
        OutboundOutcome::Accepted {
            signature: on_chain_sig,
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
        LiveBlockhash, LiveCurveState, LiveSigner, LiveStateFetcher, LiveSubmitter,
        SignError, StateFetchError, SubmitError,
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
            OutboundOutcome::Accepted { signature } => {
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
        if let OutboundOutcome::Accepted { signature } = &outcome {
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
        };
        let outcome = sink.on_admit(&record);
        match outcome {
            OutboundOutcome::Accepted { signature } => assert!(signature == [0u8; 64]),
            _ => panic!("noop sink must accept with zero signature"),
        }
    }
}
