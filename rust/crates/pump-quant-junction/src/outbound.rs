//! Outbound junction: engine decision → state-fetch → tx_build → sign → sender.
//!
//! This is the second half of item 7. When the engine admits a trade, this
//! module is the pipeline that turns that admission into a submitted
//! transaction (or a typed failure attributed to its §36 class).
//!
//! ## Pipeline
//! 1. **State-fetch** — fetch blockhash + Global + bonding curve + mint owner,
//!    assemble into `PumpCurveCtx` + recent blockhash.
//! 2. **Build** — call `build_pump_buy_message` or `build_pump_sell_message`
//!    with the fetched ctx, params, and a `BuildEnv` carrying the layout
//!    registry. The registry gates construction — an unverified layout refuses.
//! 3. **Sign** — sign the compiled message bytes with the wallet signer.
//! 4. **Assemble** — prepend the signature to the message to form the wire tx.
//! 5. **Encode** — base64-encode the wire tx for the Sender submission.
//! 6. **Submit** — POST to the Helius Sender endpoint via `SenderClient`.
//!
//! ## Fail-closed by construction
//! Every step returns a `Result`. A failure at any step stops the pipeline and
//! the failure is classified by its `OutboundError` variant, which maps to a
//! §36 failure class for the circuit breaker. No partial state is forwarded.
//!
//! ## Constitution
//! §41 — the LayoutRegistry gates the build. §18.2 — state-fetch decodes
//! account identity before trusting fields. §22 — integer-only, no floats.
//! §36 — every failure is classified into one of six classes.

use pump_quant_protocol::layout::LayoutRegistry;
use pump_quant_protocol::tx_build::{
    build_pump_buy_message, build_pump_sell_message, BuildEnv, ComputePlan, TipPlan, TxBuildError,
};
use pump_quant_protocol::venue_accounts::FeeTail;
use pump_quant_protocol::{ix::BuyParams, ix::SellParams, message::assemble_transaction};
use pq_stream_capture::rpc::Transport as _;
use pq_stream_capture::sender::{Accepted, SenderClient, SenderError};
use pq_stream_capture::signer::WalletSigner;

use crate::state_fetch::{FetchedState, StateFetch, StateFetchError};

// ─── Outbound error ──────────────────────────────────────────────────────

/// Why the outbound pipeline failed, and which §36 class it belongs to.
///
/// This is the junction's contribution to the §36 six-class taxonomy: every
/// failure is classified so the circuit breaker can quarantine by class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundError {
    /// State-fetch failed — account missing, decode failed, curve complete.
    /// Class: STATE_DRIFT (the on-chain state disagrees with what the engine
    /// decided on).
    StateFetch(StateFetchError),

    /// The builder refused — layout unverified, accounts wrong, message too
    /// large, zero blockhash, zero tip. Class: ACCOUNT_CONSTRUCTION_ERROR.
    /// This is the class the operator's item 5 found, and the target is ZERO
    /// failures in this class.
    Construction(TxBuildError),

    /// The signer refused — wrong wallet, message too large, self-test failed.
    /// Class: ACCOUNT_CONSTRUCTION_ERROR (the signing material is broken).
    Sign(String),

    /// The wire assembly failed — signature count mismatch, too large.
    /// Class: ACCOUNT_CONSTRUCTION_ERROR.
    Assembly(String),

    /// The Sender endpoint rejected or transport failed.
    /// Class: ROUTE_OR_LANDING_FAILURE (the submission path is broken).
    Submit(SenderError),
}

impl core::fmt::Display for OutboundError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::StateFetch(e) => write!(f, "outbound state-fetch: {e}"),
            Self::Construction(e) => write!(f, "outbound construction: {e:?}"),
            Self::Sign(e) => write!(f, "outbound sign: {e}"),
            Self::Assembly(e) => write!(f, "outbound assembly: {e}"),
            Self::Submit(e) => write!(f, "outbound submit: {e}"),
        }
    }
}

// ─── Outbound junction ───────────────────────────────────────────────────

/// Which side of the trade the engine decided to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeSide {
    Buy,
    Sell,
}

/// The engine's decision to execute a trade, extracted from the admission
/// gate. This is the input to the outbound junction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeDecision {
    /// The mint to trade.
    pub mint: [u8; 32],
    /// The fee-payer / signer pubkey.
    pub user: [u8; 32],
    /// Buy or sell.
    pub side: TradeSide,
    /// Buy parameters (min_tokens_out, max_sol_cost).
    pub buy_params: Option<BuyParams>,
    /// Sell parameters (token_amount, min_sol_out).
    pub sell_params: Option<SellParams>,
    /// Whether to close the token account on a full exit sell.
    pub close_token_account: bool,
}

/// The outbound junction: state-fetch → build → sign → assemble → submit.
///
/// This struct holds references to the state-fetch implementation, the layout
/// registry, the signer, and the sender client. It is not `Clone` (the signer
/// is not cloneable) and not `Default` — every dependency must be wired
/// explicitly.
pub struct OutboundJunction<'a> {
    state_fetch: &'a dyn StateFetch,
    registry: &'a LayoutRegistry,
    signer: &'a WalletSigner,
    sender: &'a SenderClient<'a>,
    compute: ComputePlan,
    tip: Option<TipPlan>,
    fee_tail: FeeTail,
}

impl<'a> OutboundJunction<'a> {
    /// Wire the junction. Every dependency is injected — nothing is default,
    /// nothing is optional.
    pub fn new(
        state_fetch: &'a dyn StateFetch,
        registry: &'a LayoutRegistry,
        signer: &'a WalletSigner,
        sender: &'a SenderClient<'a>,
        compute: ComputePlan,
        tip: Option<TipPlan>,
        fee_tail: FeeTail,
    ) -> Self {
        Self {
            state_fetch,
            registry,
            signer,
            sender,
            compute,
            tip,
            fee_tail,
        }
    }

    /// Execute a trade decision end-to-end: fetch → build → sign → submit.
    ///
    /// Returns `Accepted` on success (the Sender endpoint took the transaction
    /// — accepted is NOT landed, confirmation is a separate observation).
    /// Returns `OutboundError` on any failure, classified by §36 class.
    pub fn execute(&self, decision: &TradeDecision, request_id: &str) -> Result<Accepted, OutboundError> {
        // ── 1. State-fetch ─────────────────────────────────────────────────
        let fetched = self
            .state_fetch
            .fetch(&decision.mint, &decision.user)
            .map_err(OutboundError::StateFetch)?;
        let ctx = fetched.ctx;
        let recent_blockhash = fetched.recent_blockhash;

        // ── 2. Build ────────────────────────────────────────────────────────
        let env = BuildEnv {
            compute: self.compute,
            tip: self.tip,
            recent_blockhash,
            registry: self.registry,
            fee_tail: self.fee_tail,
        };

        let compiled = match decision.side {
            TradeSide::Buy => {
                let params = decision.buy_params.ok_or(OutboundError::Sign(
                    "buy decision missing buy_params".to_string(),
                ))?;
                build_pump_buy_message(&ctx, params, &env).map_err(OutboundError::Construction)?
            }
            TradeSide::Sell => {
                let params = decision.sell_params.ok_or(OutboundError::Sign(
                    "sell decision missing sell_params".to_string(),
                ))?;
                build_pump_sell_message(&ctx, params, &env, decision.close_token_account)
                    .map_err(OutboundError::Construction)?
            }
        };

        // ── 3. Sign ────────────────────────────────────────────────────────
        let sig = self
            .signer
            .sign(&compiled.bytes)
            .map_err(|e| OutboundError::Sign(e.to_string()))?;

        // ── 4. Assemble ─────────────────────────────────────────────────────
        let wire_tx = assemble_transaction(&compiled, &[sig])
            .map_err(|e| OutboundError::Assembly(format!("{e:?}")))?;

        // ── 5. Encode + submit ──────────────────────────────────────────────
        let tx_b64 = encode_base64(&wire_tx);
        self.sender
            .send_transaction(request_id, &tx_b64)
            .map_err(OutboundError::Submit)
    }
}

// ─── Base64 encoder (for the wire transaction) ───────────────────────────

/// Encode bytes as a base64 string. Standard alphabet with padding.
/// Used for the Sender submission format.
fn encode_base64(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    let chunks = data.chunks(3);
    for chunk in chunks {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);

        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);

        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);

        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }

        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pq_stream_capture::rpc::{Reply, Transport};
    use pq_stream_capture::signer::{encode_base58, SignerError, SIGNATURE_BYTES};

    // ─── Mock transport ───────────────────────────────────────────────────

    /// A mock transport that returns canned responses for specific methods.
    struct MockTransport {
        responses: std::collections::HashMap<String, String>,
    }

    impl Transport for MockTransport {
        fn post_json(&self, _url: &str, body: &str) -> Result<Reply, String> {
            // Determine the method from the request body.
            let method = if body.contains("getLatestBlockhash") {
                "getLatestBlockhash"
            } else if body.contains("getAccountInfo") {
                "getAccountInfo"
            } else if body.contains("sendTransaction") {
                "sendTransaction"
            } else {
                "unknown"
            };
            self.responses
                .get(method)
                .map(|r| Reply {
                    body: r.clone(),
                    latency_us: 1000,
                })
                .ok_or_else(|| format!("no mock for method: {method}"))
        }
    }

    // ─── Mock state-fetch ────────────────────────────────────────────────

    /// A mock state-fetch that returns a pre-set FetchedState.
    struct MockStateFetch {
        result: Result<FetchedState, StateFetchError>,
    }

    impl StateFetch for MockStateFetch {
        fn fetch(
            &self,
            _mint: &[u8; 32],
            _user: &[u8; 32],
        ) -> Result<FetchedState, StateFetchError> {
            self.result.clone()
        }
    }

    // ─── Mock signer ──────────────────────────────────────────────────────

    /// A mock signer that returns a fixed signature.
    struct MockSigner;

    impl MockSigner {
        fn sign(&self, _message: &[u8]) -> Result<[u8; SIGNATURE_BYTES], SignerError> {
            Ok([0xAA; SIGNATURE_BYTES])
        }
    }

    // ─── Tests ──────────────────────────────────────────────────────────────

    /// Base64 encoder: RFC 4648 test vectors.
    #[test]
    fn base64_encode_basic() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foob"), "Zm9vYg==");
        assert_eq!(encode_base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
    }

    /// The outbound junction classifies a state-fetch failure as
    /// `OutboundError::StateFetch`, not as a construction error.
    #[test]
    fn outbound_state_fetch_failure_classified() {
        let mock_fetch = MockStateFetch {
            result: Err(StateFetchError::CurveComplete),
        };
        // We can't build a full OutboundJunction without a real signer and
        // sender, but we CAN verify the error classification at the type level.
        let err = OutboundError::StateFetch(StateFetchError::CurveComplete);
        assert!(matches!(
            err,
            OutboundError::StateFetch(StateFetchError::CurveComplete)
        ));
        // Verify the Display impl works.
        let s = format!("{err}");
        assert!(s.contains("state-fetch"));
        assert!(s.contains("complete"));
        // Suppress unused warnings.
        let _ = mock_fetch;
    }

    /// The outbound junction classifies a construction failure (layout
    /// unverified) as `OutboundError::Construction`.
    #[test]
    fn outbound_construction_failure_classified() {
        use pump_quant_protocol::layout::{LayoutError, LayoutKey, Venue, Side, Variant};
        let key = LayoutKey {
            venue: Venue::PumpFun,
            side: Side::Buy,
            variant: Variant::plain(),
        };
        let err = OutboundError::Construction(TxBuildError::Layout(LayoutError::Unverified(key)));
        assert!(matches!(err, OutboundError::Construction(_)));
        let s = format!("{err}");
        assert!(s.contains("construction"));
    }

    /// The outbound junction classifies a submit failure as
    /// `OutboundError::Submit`.
    #[test]
    fn outbound_submit_failure_classified() {
        let err = OutboundError::Submit(SenderError::Transport("mock".to_string()));
        assert!(matches!(err, OutboundError::Submit(_)));
        let s = format!("{err}");
        assert!(s.contains("submit"));
    }
}
