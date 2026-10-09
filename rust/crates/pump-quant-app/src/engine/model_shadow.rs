//! Engine wiring of the paper fill model `paper_fill_v2_shadow` and the shared BUY/ADD/SELL settlement.
//!
//! ARMED ONLY. With the default `paper_fill_v1_observed` nothing here runs: no field is read, no ledger section is
//! written, so the legacy/golden path and every v1 result stay byte-identical.
//!
//! With `paper_fill_v2_shadow`:
//! * OUR simulated fills (entry BUY, ADD, REDUCE/EXIT/protective SELL) are priced on `observed reserves + our carried
//!   delta` ([`crate::shadow_pool::ShadowBook`]); the observed-state quote is still computed and reported next to it
//!   as the external-liquidity benchmark (never replaced by it).
//! * Every fill increment that changes our books (paper or reconciled) is also settled through the ONE
//!   [`crate::settlement::SettlementLedger::settle`] path; its invariant `cash + committed == seed + realized` holds
//!   after every step and its per-mint token holding must equal the engine's reconciled inventory EXACTLY. A refused
//!   settlement or an inventory mismatch is a named fault that latches SAFETY_OFF (`settlement_fault`); nothing is
//!   closed and protection continues.
//! * Fresh observations reconcile the shadow; a divergence is named, permanent per segment, reported to the stop table
//!   (`ShadowDivergence` -> SAFETY_OFF `shadow_divergence`), and the delta is dropped (sale capacity falls back to the
//!   observed state alone).
//! * Shadow book + settlement ledger persist inside the held ledger (`paper_fill` section) and restore exactly.

use super::*;
use crate::settlement::{SettleError, Settled, SettlementLedger};
use crate::shadow_pool::{
    CurveBase, Divergence, FillBasis, LegKind, ObservedVenue, PaperFillVersion, PoolBase,
    ShadowBook,
};

/// SAFETY_OFF reason latched by a settlement refusal or a settlement/engine inventory mismatch.
pub const REASON_SETTLEMENT_FAULT: &str = "settlement_fault";

/// Paper-fill state carried by the engine (inert under v1).
#[derive(Debug, Clone, Default)]
pub struct PaperFillState {
    /// Which paper fill model prices our simulated fills.
    pub version: PaperFillVersion,
    /// The shadow book (v2 only).
    pub shadow: ShadowBook,
    /// The shared settlement ledger (v2 only; `None` under v1).
    pub settle: Option<SettlementLedger>,
    /// Named settlement faults seen (refusals + inventory mismatches), most recent last (bounded).
    pub faults: Vec<String>,
    /// Price-only reconciled reports that carried no proceeds (cannot be settled without inventing proceeds).
    pub unsettleable: u64,
}

const FAULT_LOG_CAP: usize = 64;

impl Engine {
    /// Select the paper fill model. Must be called before restore and before the first tick. Arming v2 creates
    /// the settlement ledger at the configured seed.
    pub fn model_set_paper_fill(&mut self, v: PaperFillVersion) {
        self.model_pf.version = v;
        self.model_pf.shadow = ShadowBook::default();
        self.model_pf.settle = match v {
            PaperFillVersion::V2Shadow => {
                Some(SettlementLedger::new(self.bankroll_origin.seed_lamports()))
            }
            PaperFillVersion::V1Observed => None,
        };
    }

    /// The paper fill model in use.
    #[must_use]
    pub fn model_paper_fill(&self) -> PaperFillVersion {
        self.model_pf.version
    }

    pub(super) fn model_v2(&self) -> bool {
        self.model_pf.version == PaperFillVersion::V2Shadow
    }

    /// The shadow book (read-only view).
    #[must_use]
    pub fn model_shadow_book(&self) -> &ShadowBook {
        &self.model_pf.shadow
    }

    /// The settlement ledger (v2 only).
    #[must_use]
    pub fn model_settlement(&self) -> Option<&SettlementLedger> {
        self.model_pf.settle.as_ref()
    }

    /// Named settlement faults seen so far.
    #[must_use]
    pub fn model_settlement_faults(&self) -> &[String] {
        &self.model_pf.faults
    }

    /// The first named shadow divergence among HELD markets (the stop table's `ShadowDivergence` input).
    #[must_use]
    pub fn model_shadow_held_divergence(&self) -> Option<([u8; 32], Divergence)> {
        if !self.model_v2() {
            return None;
        }
        self.positions.held_records().iter().find_map(|h| {
            self.model_pf
                .shadow
                .divergence(&h.mint)
                .map(|d| (h.mint, d))
        })
    }

    fn model_settle_fault(&mut self, what: String) {
        self.mrep(format!("settle:fault:{what}"));
        if self.model_pf.faults.len() >= FAULT_LOG_CAP {
            self.model_pf.faults.remove(0);
        }
        self.model_pf.faults.push(what);
        self.model_safety_trip(REASON_SETTLEMENT_FAULT);
    }

    /// THE settlement call of the engine: one reconciled increment of one order. No-op under v1.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn model_settle(
        &mut self,
        mint: [u8; 32],
        leg: LegKind,
        order_id: u64,
        tokens: u64,
        venue: u64,
        network: u64,
        fixed: u64,
    ) {
        let Some(l) = self.model_pf.settle.as_mut() else {
            return;
        };
        let r = l.settle_increment(mint, leg, order_id, tokens, venue, network, fixed);
        let held = l.holdings.get(&mint).map_or(0, |h| h.tokens);
        match r {
            Ok(Settled::Applied { .. }) => self.mrep(format!("settle:applied:{}", leg.code())),
            Ok(Settled::Duplicate) => self.mrep("settle:duplicate"),
            Err(e) => {
                self.model_settle_fault(format!("{}:{}:{order_id}", e.label(), leg.code()));
                return;
            }
        }
        // The settlement holding and the engine's reconciled inventory are the same quantity, exactly.
        let inv = self.positions.inventory_tokens(&mint).unwrap_or(0);
        if inv != held {
            self.model_settle_fault(format!(
                "settle_refused:inventory_mismatch:{}:{order_id}:engine={inv}:settlement={held}",
                leg.code()
            ));
        }
    }

    /// Record a price-only reconciled report that carried no proceeds: it changed the engine's books but cannot be
    /// settled without inventing proceeds. Named, counted, and a settlement fault (the ledgers no longer agree).
    pub(super) fn model_settle_unsettleable(&mut self, order_id: u64) {
        if self.model_pf.settle.is_none() {
            return;
        }
        self.model_pf.unsettleable += 1;
        self.model_settle_fault(format!(
            "settle_refused:price_only_report_has_no_proceeds:{order_id}"
        ));
    }

    /// Apply one of OUR simulated fills to the shadow (cumulative per order). No-op under v1.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn model_shadow_apply(
        &mut self,
        mint: [u8; 32],
        basis: FillBasis,
        leg: LegKind,
        order_id: u64,
        cum_tokens: u64,
        cum_lamports: u64,
    ) {
        if !self.model_v2() {
            return;
        }
        let out = self.model_pf.shadow.apply_own_fill(
            mint,
            basis,
            leg,
            order_id,
            cum_tokens,
            cum_lamports,
        );
        self.mrep(match out {
            crate::shadow_pool::ApplyOutcome::Applied { .. } => "shadow:fill_applied",
            crate::shadow_pool::ApplyOutcome::Duplicate => "shadow:fill_duplicate",
            crate::shadow_pool::ApplyOutcome::Backwards => "shadow:fill_backwards",
            crate::shadow_pool::ApplyOutcome::NotCarried => "shadow:fill_not_carried",
        });
    }

    /// Cumulative (tokens, lamports) the shadow already holds for `(leg, order_id)` on `mint`.
    pub(super) fn model_shadow_cum(
        &self,
        mint: &[u8; 32],
        leg: LegKind,
        order_id: u64,
    ) -> (u64, u64) {
        self.model_pf
            .shadow
            .market(mint)
            .and_then(|m| m.applied.get(&(leg, order_id)).copied())
            .unwrap_or((0, 0))
    }

    /// A fresh curve observation: reconcile the shadow (v2). A divergence is named and counted.
    pub(super) fn model_shadow_on_curve(
        &mut self,
        mint: &[u8; 32],
        o: &crate::curve_annotation::CurveObservation,
    ) {
        if !self.model_v2() {
            return;
        }
        let b = curve_base(o);
        if let Some(d) = self.model_pf.shadow.on_curve_observation(mint, &b) {
            self.mrep(d.label());
            self.mrep(format!("shadow:divergence_at_slot:{}", o.slot));
        }
    }

    /// A canonical pool swap's pre-trade state: graduation of a curve shadow, then pool reconciliation (v2).
    pub(super) fn model_shadow_on_pool(
        &mut self,
        mint: &[u8; 32],
        base: u64,
        quote: u64,
        vq: Option<u64>,
        slot: u64,
    ) {
        if !self.model_v2() {
            return;
        }
        if self
            .model_pf
            .shadow
            .market(mint)
            .is_some_and(|m| m.venue == crate::shadow_pool::ShadowVenue::Curve)
        {
            let dropped = self.model_pf.shadow.on_graduation(mint);
            self.mrep(if dropped {
                "shadow:graduation_curve_delta_ended"
            } else {
                "shadow:graduation_empty"
            });
        }
        let Some(vq) = vq else {
            self.mrep("shadow:pool_observation_without_virtual_quote");
            return;
        };
        let b = PoolBase {
            base,
            quote,
            vq,
            slot,
        };
        if let Some(d) = self.model_pf.shadow.on_pool_observation(mint, &b) {
            self.mrep(d.label());
        }
    }

    /// The curve reserves OUR fill is priced on: v1 = observed; v2 = observed + carried delta (`None` = shadow
    /// state not representable, named by the caller).
    pub(super) fn model_fill_curve(
        &self,
        mint: &[u8; 32],
        o: &crate::curve_annotation::CurveObservation,
    ) -> Option<CurveBase> {
        let b = curve_base(o);
        if !self.model_v2() {
            return Some(b);
        }
        self.model_pf.shadow.shadow_curve(mint, &b).map(|(s, _)| s)
    }

    /// Pool reserves (base, quote) OUR fill is priced on (v2: observed + carried delta).
    pub(super) fn model_fill_pool(
        &self,
        mint: &[u8; 32],
        base: u64,
        quote: u64,
        vq: u64,
        slot: u64,
    ) -> Option<(u64, u64)> {
        if !self.model_v2() {
            return Some((base, quote));
        }
        self.model_pf
            .shadow
            .shadow_pool(
                mint,
                &PoolBase {
                    base,
                    quote,
                    vq,
                    slot,
                },
            )
            .map(|(s, _)| (s.base, s.quote))
    }

    /// Shadow capacity guard for a sell gross (v2): `observed real SOL / vault + conserved contribution`.
    pub(super) fn model_shadow_capacity(&self, mint: &[u8; 32], observed_sol: u64) -> u128 {
        let conserved = self
            .model_pf
            .shadow
            .market(mint)
            .filter(|m| m.diverged.is_none())
            .map_or(0, crate::shadow_pool::ShadowMarket::conserved_sol);
        u128::from(observed_sol) + conserved
    }

    /// Shadow NET liquidation estimate per held model-managed mint (v2 stop-table estimator): shadow venue-net
    /// minus the network estimate (hook) minus the exit tip. Unknown is a named `Err`, never zero.
    #[must_use]
    pub fn model_shadow_liquidation_estimates(
        &self,
    ) -> Vec<crate::stop_policy::LiquidationEstimate> {
        self.positions
            .held_records()
            .iter()
            .filter(|h| h.model_managed)
            .map(|h| crate::stop_policy::LiquidationEstimate {
                mint: h.mint,
                value: self.model_shadow_liquidation_estimate(&h.mint),
            })
            .collect()
    }

    fn model_shadow_liquidation_estimate(&self, mint: &[u8; 32]) -> Result<i128, &'static str> {
        let inv = self
            .positions
            .inventory_tokens(mint)
            .ok_or("inventory_unknown")?;
        if let Some(d) = self.model_pf.shadow.divergence(mint) {
            return Err(d.label());
        }
        let clock = self.model_clock_ms;
        let budget = crate::curve_annotation::PRICING_BUDGET_MS;
        let obs = if self.model_cache.snapshot_venue_is_amm(mint) {
            if self.model_cache.pool_conflicting(mint) {
                return Err("pool_binding_conflict");
            }
            let o = self
                .model_cache
                .amm_obs(mint)
                .ok_or("no_reserve_observation")?;
            if clock.saturating_sub(o.ts_ms) > budget {
                return Err("mark_stale");
            }
            match self.model_amm_econ.get(mint).copied() {
                Some((parts, Some(vq), t, cashback)) if t == o.ts_ms => ObservedVenue::Pool(
                    PoolBase {
                        base: o.base_reserves_raw,
                        quote: o.quote_reserves_lamports,
                        vq,
                        slot: o.slot,
                    },
                    parts,
                    cashback,
                ),
                _ => return Err("quote_unavailable:amm_economics_missing"),
            }
        } else {
            let o = self
                .model_cache
                .curve_obs(mint)
                .ok_or("no_reserve_observation")?;
            if clock.saturating_sub(o.ts_ms) > budget {
                return Err("mark_stale");
            }
            ObservedVenue::Curve(curve_base(&o))
        };
        let net = self
            .model_pf
            .shadow
            .net_liquidation_estimate(mint, Some(&obs), inv)
            .ok_or("shadow_quote_unavailable")?;
        Ok(net - i128::from(self.cfg.exit_tip_lamports))
    }

    /// Forget a closed market's shadow (no pending order can still fill on it).
    pub(super) fn model_shadow_forget(&mut self, mint: &[u8; 32]) {
        if self.model_v2() {
            self.model_pf.shadow.forget(mint);
        }
    }

    /// Durable `paper_fill` section of the held ledger (`None` under v1: the v1 ledger is unchanged).
    pub(super) fn model_pf_section(&self) -> Option<serde_json::Value> {
        let l = self.model_pf.settle.as_ref()?;
        Some(serde_json::json!({
            "version": self.model_pf.version.label(),
            "shadow_pool": self.model_pf.shadow.to_json(),
            "settlement": l.to_json(),
            "unsettleable": self.model_pf.unsettleable,
        }))
    }

    /// Parse + validate a `paper_fill` section against the engine's selected model and the ledger's held inventory.
    pub(super) fn model_pf_validate(
        &self,
        section: Option<&serde_json::Value>,
        l: &crate::held_state::HeldLedger,
    ) -> Result<Option<(ShadowBook, SettlementLedger, u64)>, crate::held_state::RestoreRefusal>
    {
        use crate::held_state::RestoreRefusal as R;
        match (self.model_v2(), section) {
            (false, None) => Ok(None),
            (false, Some(_)) => Err(R::PaperFillVersionMismatch),
            (true, None) => {
                if l.held.is_empty() && l.pending.is_empty() {
                    // Flat books from a v1 ledger: a fresh settlement ledger at the restored realized total.
                    let seed = self.bankroll_origin.seed_lamports();
                    let mut s = SettlementLedger::new(seed);
                    s.realized = l.realized_lamports;
                    s.cash = i128::from(seed) + l.realized_lamports;
                    Ok(Some((ShadowBook::default(), s, 0)))
                } else {
                    Err(R::PaperFillSectionMissing)
                }
            }
            (true, Some(v)) => {
                if v["version"].as_str() != Some(PaperFillVersion::V2Shadow.label()) {
                    return Err(R::PaperFillVersionMismatch);
                }
                let book =
                    ShadowBook::from_json(&v["shadow_pool"]).map_err(|_| R::PaperFillUntrusted)?;
                let s = SettlementLedger::from_json(&v["settlement"])
                    .map_err(|_| R::PaperFillUntrusted)?;
                let un = v["unsettleable"].as_u64().ok_or(R::PaperFillUntrusted)?;
                // Realized may legitimately differ from the engine's (the engine releases basis in rounded bps,
                // settlement in exact tokens): that difference is REPORTED (`settlement_minus_engine`), not refused.
                // Seed and per-mint inventory are the same quantity in both books and must match exactly.
                if s.seed != i128::from(l.seed_lamports) {
                    return Err(R::SettlementBooksMismatch);
                }
                for h in &l.held {
                    let st = s.holdings.get(&h.mint).map_or(0, |x| x.tokens);
                    if h.inventory_tokens.unwrap_or(0) != st {
                        return Err(R::SettlementBooksMismatch);
                    }
                }
                if s.holdings
                    .keys()
                    .any(|m| !l.held.iter().any(|h| h.mint == *m))
                {
                    return Err(R::SettlementBooksMismatch);
                }
                Ok(Some((book, s, un)))
            }
        }
    }

    pub(super) fn model_pf_apply(&mut self, parsed: Option<(ShadowBook, SettlementLedger, u64)>) {
        if let Some((b, s, un)) = parsed {
            self.model_pf.shadow = b;
            self.model_pf.settle = Some(s);
            self.model_pf.unsettleable = un;
        }
    }

    /// Read-only paper-fill block for the harness checkpoint / status (v1: just the version).
    #[must_use]
    pub fn model_paper_fill_status(&self) -> serde_json::Value {
        let hx = |m: &[u8; 32]| m.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let Some(l) = self.model_pf.settle.as_ref() else {
            return serde_json::json!({"version": self.model_pf.version.label()});
        };
        let engine_cash = i128::from(self.bankroll_origin.seed_lamports()) + self.bankroll_realized
            - i128::try_from(self.bankroll_committed).unwrap_or(i128::MAX);
        let markets: Vec<serde_json::Value> = self
            .model_pf
            .shadow
            .markets
            .iter()
            .map(|(m, s)| {
                serde_json::json!({
                    "mint": hx(m),
                    "venue": format!("{:?}", s.venue),
                    "sol_in": s.sol_in.to_string(), "sol_out": s.sol_out.to_string(),
                    "tok_out": s.tok_out.to_string(), "tok_in": s.tok_in.to_string(),
                    "diverged": s.diverged.map(Divergence::label),
                })
            })
            .collect();
        serde_json::json!({
            "version": self.model_pf.version.label(),
            "settlement": {
                "seed": l.seed.to_string(), "cash": l.cash.to_string(), "committed": l.committed.to_string(),
                "realized": l.realized.to_string(), "network_estimate": l.network_estimate.to_string(),
                "network_label": crate::settlement::NETWORK_FEE_LABEL,
                "fixed_costs": l.fixed_costs.to_string(), "invariant_holds": l.invariant_holds(),
                "holdings": l.holdings.iter().map(|(m, h)| serde_json::json!([hx(m), h.tokens, h.cost.to_string()])).collect::<Vec<_>>(),
            },
            "engine_books": {
                "realized": self.bankroll_realized.to_string(),
                "committed": self.bankroll_committed.to_string(),
                "cash": engine_cash.to_string(),
            },
            "settlement_minus_engine": {
                "realized": (l.realized - self.bankroll_realized).to_string(),
                "cash": (l.cash - engine_cash).to_string(),
            },
            "faults": self.model_pf.faults,
            "unsettleable": self.model_pf.unsettleable,
            "shadow_markets": markets,
            "held_divergence": self.model_shadow_held_divergence().map(|(m, d)| serde_json::json!([hx(&m), d.label()])),
        })
    }
}

/// The settlement error type is re-exported for tests that name refusals.
pub type PaperSettleError = SettleError;

pub(super) fn curve_base(o: &crate::curve_annotation::CurveObservation) -> CurveBase {
    CurveBase {
        vsol: o.v_sol_lamports,
        vtok: o.v_tokens,
        real_sol: o.real_sol_lamports,
        real_tok: o.real_tokens,
        slot: o.slot,
    }
}
