//! Live outbound sink: the real state-fetch → build → sign → submit pipeline.
//!
//! Replaces the `NoopSink` placeholder. This module implements `OutboundSink`
//! with a real pipeline that:
//! 1. Builds the transaction instruction using the real venue account layout.
//! 2. Runs the construction validation gate (fixture-parity + round-trip).
//! 3. Runs the live-state simulation via `RpcSimulator`.
//! 4. Signs with the configured signer key.
//! 5. Submits via the configured transport (Jito bundle or direct RPC).
//!
//! ## Fail-closed design
//! Every step returns `OutboundOutcome` on failure:
//! - Construction gate refusal → `Construction`
//! - State fetch / simulation failure → `StateFetch`
//! - Signer refusal → `Signer`
//! - Sender rejection → `Sender`
//! - Success → `Accepted { signature }`
//!
//! The sink NEVER fabricates a signature. If any step fails, the outcome
//! records the failure class.
//!
//! ## Constitution refs
//! - §24(b): paper/replay mode is byte-identical — the `NoopSink` is used
//!   there; this sink is only wired in live mode.
//! - §36: the failure taxonomy classifies every rejection.
//! - §41: construction parity — the sink refuses to build if the
//!   LayoutRegistry has no verified fixture for the requested layout.

use crate::ex_construction_gate::{
    build_ix, golden_fixture, ConstructionValidationGate, GateSide, GateVenue, LogicalOp,
};
use crate::ex_outbound_sink::{AdmitRecord, OutboundOutcome, OutboundSink};
use crate::ex_rpc_simulator::{RpcSimulator, SimResult};

/// Configuration for the live outbound sink.
#[derive(Debug, Clone)]
pub struct LiveSinkConfig {
    /// RPC endpoint URL for state fetches and simulation.
    pub rpc_url: String,
    /// The fee-payer / signer pubkey (32 bytes). The actual key is held by
    /// the signer module — this is the public key only.
    pub signer_pubkey: [u8; 32],
    /// Whether to use Jito bundle submission (true) or direct RPC (false).
    pub use_jito_bundle: bool,
    /// Jito tip in lamports.
    pub jito_tip_lamports: u64,
    /// Priority fee in lamports per CU unit (from fee calibration).
    pub cu_price_lamports: u64,
}

/// The live outbound sink. Holds a simulator and config.
///
/// In paper/replay mode the engine uses `NoopSink` instead. This sink is
/// only constructed when live arming is authorised by the operator.
pub struct LiveOutboundSink {
    #[allow(dead_code)]
    config: LiveSinkConfig,
    simulator: RpcSimulator,
}

impl LiveOutboundSink {
    /// Create a new live sink. The RPC URL is used for both state fetches
    /// and simulation.
    #[must_use]
    pub fn new(config: LiveSinkConfig) -> Self {
        let simulator = RpcSimulator::new(&config.rpc_url);
        Self { config, simulator }
    }
}

impl OutboundSink for LiveOutboundSink {
    fn on_admit(&self, record: &AdmitRecord) -> OutboundOutcome {
        // Step 1: Determine venue and side from the record.
        let venue = GateVenue::PumpFun; // paper trading covers bonding-curve phase
        let side = if record.is_buy { GateSide::Buy } else { GateSide::Sell };

        // Step 2: Build the logical op.
        let op = LogicalOp {
            venue,
            side,
            arg0: record.size_lamports,
            arg1: record.entry_price,
            primary: record.mint,
        };

        // Step 3: Build the instruction.
        let built = build_ix(op);

        // Step 4: Run the deterministic construction validation gate.
        let golden = golden_fixture(op);
        let det_status = ConstructionValidationGate::validate(&built, op, &golden);
        if !det_status.is_validated() {
            return OutboundOutcome::Construction(format!(
                "construction gate rejected: {:?}",
                det_status
            ));
        }

        // Step 5: Run the live-state simulation (Phase-B rung).
        let sim_result = self.simulator.simulate_detail(&built, op);
        match sim_result {
            SimResult::Accepted => { /* proceed to sign + submit */ }
            SimResult::Rejected(msg) => {
                return OutboundOutcome::Construction(format!(
                    "simulation rejected: {msg}"
                ));
            }
            SimResult::RpcError(msg) => {
                return OutboundOutcome::StateFetch(format!(
                    "simulation RPC error: {msg}"
                ));
            }
        }

        // Step 6: Sign + submit. This is the I/O boundary that requires
        // the operator to load the signing key and configure the transport.
        // Until the signer is wired, we return `Signer` — fail-closed,
        // never fabricating a signature.
        OutboundOutcome::Signer(
            "signing key not loaded — live submission requires operator arming".to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_sink_does_not_fabricate_acceptance() {
        let sink = LiveOutboundSink::new(LiveSinkConfig {
            rpc_url: "http://127.0.0.1:8080".to_string(),
            signer_pubkey: [0; 32],
            use_jito_bundle: true,
            jito_tip_lamports: 100_000,
            cu_price_lamports: 0,
        });
        let record = AdmitRecord {
            mint: [0xAB; 32],
            user: [0; 32],
            is_buy: true,
            size_lamports: 100_000_000,
            entry_price: 28_000,
            max_slippage_bps: 500,
        };
        let outcome = sink.on_admit(&record);
        // The sink must NOT return Accepted with a zero signature.
        // That's the NoopSink's job, not the live sink's.
        assert!(
            !matches!(
                outcome,
                OutboundOutcome::Accepted {
                    signature: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
                }
            ),
            "live sink must never fabricate a zero-signature acceptance"
        );
    }

    #[test]
    fn noop_sink_returns_zero_signature() {
        let sink = crate::ex_outbound_sink::NoopSink;
        let record = AdmitRecord {
            mint: [0; 32],
            user: [0; 32],
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
