//! RESTART launch recovery: rebuild the decision cache's per-market evidence (launch identity, creator counts,
//! state tape, enrichment, reserve plane, pool bindings, market registration) from the PREVIOUS process's durable
//! capture, causally and deduplicated, before the first tick.
//!
//! Contract
//! * Only events with receive time `<= restart_ms` (the durable barrier the held ledger and flow checkpoint are bound
//!   to) are applied. Everything after it is re-delivered by the live/overlap feed and deduplicated there.
//! * The flow reducer is NOT touched: it is durable on its own and already holds its copy of every applied print.
//! * Launches: same dedup identity (mint) and count-before-insert as the uninterrupted run, applied in capture order,
//!   so a creator count is reproduced exactly and never manufactured; a frozen count is never revised.
//! * Nothing is dirtied or dispatched: a market rebuilt here is only REGISTERED (as the uninterrupted run had it).
//! * Coverage: `covered_from_ms` is the first receive time of a capture that reaches `restart_ms`. A launch before the
//!   restart that the capture does not cover from launch keeps a NAMED refusal (`join_tape_truncated_at_restart`) and
//!   is reported as lost coverage (mint + launch time); it is never served as parity.

use super::Engine;
use crate::event::AppEvent;

/// What one rehydration pass applied (reporting).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RehydrateStats {
    pub applied: u64,
    pub after_restart_skipped: u64,
    pub launches: u64,
    pub prints: u64,
    pub reserves: u64,
    /// Pump.fun CURVE markets whose curve-account routing the applied prefix PROVES the previous process had: a launch
    /// (every launch source installs the PDA) or a decoded curve account observation (only routed accounts decode).
    /// A curve print alone proves nothing (some print sources never install the PDA), so it is not included: routing
    /// more than the uninterrupted process would admit reserve updates it dropped. The daemon re-installs exactly this
    /// set at restart, so a reserve update after the restart is not dropped merely because the process-local map was lost.
    pub curve_mints: std::collections::BTreeSet<[u8; 32]>,
}

impl Engine {
    /// The `decision.through_ms` the NEXT held-ledger write records (the capture seal binds to it).
    #[must_use]
    pub fn model_held_through_ms_now(&self) -> i64 {
        self.model_clock_ms.max(self.model_replay_through_ms)
    }

    /// Whether a held-ledger FILE was restored into this engine (a restart, as opposed to a clean start).
    #[must_use]
    pub fn model_held_restored_from_file(&self) -> bool {
        self.model_held.restored_from_file
    }

    /// The durable decision barrier of the restored held ledger (0 = no restore).
    #[must_use]
    pub fn model_replay_through_ms(&self) -> i64 {
        self.model_replay_through_ms
    }

    /// Declare the restart instant and the capture coverage start (use `i64::MAX` when no trustworthy capture covers
    /// the interval up to `restart_ms`). Call once, after the held restore, before the first tick.
    pub fn model_rehydrate_begin(&mut self, restart_ms: i64, covered_from_ms: i64) {
        self.model_cache
            .set_restart_floor(restart_ms, covered_from_ms);
    }

    /// Markets launched before the restart whose pre-restart history is not recovered: `(mint, launch_ms)`.
    #[must_use]
    pub fn model_restart_lost_coverage(&self) -> Vec<([u8; 32], i64)> {
        self.model_cache.restart_lost_coverage()
    }

    /// Apply ONE captured event of the previous process (only after [`Self::model_rehydrate_begin`]). Returns whether
    /// it was applied (false: after the restart instant, no clock, or not a market-evidence event).
    pub fn model_rehydrate_event(&mut self, ev: &AppEvent, st: &mut RehydrateStats) -> bool {
        let Some((restart_ms, _)) = self.model_cache.restart_floor() else {
            return false;
        };
        let recv = match ev {
            AppEvent::MarketTrade { recv_unix_ms, .. }
            | AppEvent::CorpusFlowRow { recv_unix_ms, .. }
            | AppEvent::CurveObserved { recv_unix_ms, .. }
            | AppEvent::AmmSwap { recv_unix_ms, .. } => *recv_unix_ms,
            AppEvent::LaunchObserved { launch_unix_ms, .. } => Some(*launch_unix_ms),
            _ => None,
        };
        let Some(t) = recv else {
            return false;
        };
        if t > restart_ms {
            st.after_restart_skipped += 1;
            return false;
        }
        let applied = match *ev {
            AppEvent::LaunchObserved {
                mint,
                creator,
                launch_unix_ms,
            } => {
                st.launches += 1;
                st.curve_mints.insert(*mint.as_bytes());
                self.model_cache
                    .observe_launch(*mint.as_bytes(), creator, launch_unix_ms);
                self.model_registry.insert(*mint.as_bytes());
                true
            }
            AppEvent::MarketTrade {
                mint,
                price_fp,
                quote_lamports,
                signed_base,
                buyer_entity,
                trader_pubkey,
                recv_unix_ms,
                slot,
                fee_lamports,
                cu_consumed,
                venue,
                event_id,
                feature,
                ..
            } => {
                let venue = match venue {
                    Some(crate::event::TradeVenue::PumpFun) => {
                        crate::state_ledger::VenueLabel::Pumpfun
                    }
                    Some(crate::event::TradeVenue::PumpSwap) => {
                        crate::state_ledger::VenueLabel::Pumpswap
                    }
                    None => crate::state_ledger::VenueLabel::Unknown,
                };
                if price_fp > 0 && matches!(venue, crate::state_ledger::VenueLabel::Pumpfun) {
                    self.model_registry.insert(*mint.as_bytes());
                }
                st.prints += 1;
                self.model_cache
                    .rehydrate_trade(&crate::decision_join::TradeObs {
                        mint: *mint.as_bytes(),
                        price_fp,
                        quote_lamports,
                        signed_base,
                        buyer_entity,
                        trader: trader_pubkey,
                        recv_unix_ms,
                        slot,
                        fee_lamports,
                        cu_consumed,
                        venue,
                        event_id,
                        feature,
                    })
                    .is_some()
            }
            AppEvent::CorpusFlowRow {
                mint,
                venue,
                feature,
                recv_unix_ms,
                slot,
                fee_lamports,
                cu_consumed,
                event_id,
            } => {
                let venue = match venue {
                    crate::event::TradeVenue::PumpFun => crate::state_ledger::VenueLabel::Pumpfun,
                    crate::event::TradeVenue::PumpSwap => crate::state_ledger::VenueLabel::Pumpswap,
                };
                st.prints += 1;
                self.model_cache
                    .rehydrate_trade(&crate::decision_join::TradeObs {
                        mint: *mint.as_bytes(),
                        price_fp: 0,
                        quote_lamports: 0,
                        signed_base: 0,
                        buyer_entity: 0,
                        trader: Some(feature.trader),
                        recv_unix_ms,
                        slot,
                        fee_lamports,
                        cu_consumed,
                        venue,
                        event_id: Some(event_id),
                        feature: Some(feature),
                    })
                    .is_some()
            }
            AppEvent::CurveObserved {
                mint,
                v_sol_lamports,
                v_tokens,
                real_sol_lamports,
                real_tokens,
                slot,
                ..
            } => {
                st.reserves += 1;
                st.curve_mints.insert(*mint.as_bytes());
                self.model_registry.insert(*mint.as_bytes());
                self.model_cache.observe_curve(
                    *mint.as_bytes(),
                    crate::curve_annotation::CurveObservation {
                        v_sol_lamports,
                        v_tokens,
                        real_sol_lamports,
                        real_tokens,
                        ts_ms: t,
                        slot,
                    },
                );
                true
            }
            AppEvent::AmmSwap {
                mint,
                pool,
                pool_is_canonical,
                quote_is_wsol,
                token_reserve_pre,
                quote_reserve_pre,
                fee_bps,
                fee_parts,
                virtual_quote,
                cashback,
                is_buy,
                token_amount,
                quote_lamports,
                trader,
                fee_lamports,
                cu_consumed,
                slot,
                ..
            } => {
                st.reserves += 1;
                let m = *mint.as_bytes();
                if !quote_is_wsol {
                    return false;
                }
                if !pool_is_canonical {
                    self.model_other_pools.entry(m).or_default().insert(pool);
                    return true;
                }
                // Same registration as the live path (`model_on_amm_swap` -> `model_register`): a canonical WSOL pool
                // swap registers its market whether or not the observation is applied.
                self.model_registry.insert(m);
                let pool_s = pool.iter().map(|b| format!("{b:02x}")).collect::<String>();
                self.model_cache.bind_pool(m, &pool_s);
                let others = self.model_other_pools.get(&m).map_or(0, |s| s.len());
                self.model_cache.observe_amm(
                    m,
                    crate::curve_annotation::AmmObservation {
                        pool: pool_s.clone(),
                        base_reserves_raw: token_reserve_pre,
                        quote_reserves_lamports: quote_reserve_pre,
                        quote_is_wsol: true,
                        ts_ms: t,
                        slot,
                    },
                    crate::curve_annotation::AmmAttribution {
                        wsol_pools: vec![pool_s],
                        pools_total: 1 + others,
                        graduated: true,
                    },
                );
                self.model_amm_fee.insert(m, (fee_bps, t));
                self.model_amm_econ
                    .insert(m, (fee_parts, virtual_quote, t, cashback));
                if token_amount > 0 && quote_lamports > 0 && !self.corpus_flow_rows {
                    let price_fp =
                        (i128::from(quote_lamports) * 1_000_000_000) / i128::from(token_amount);
                    let signed = i64::try_from(token_amount).unwrap_or(i64::MAX);
                    self.model_cache
                        .rehydrate_trade(&crate::decision_join::TradeObs {
                            mint: m,
                            price_fp,
                            quote_lamports,
                            signed_base: if is_buy { signed } else { -signed },
                            buyer_entity:
                                pump_quant_wallet_graph::tracked_wallet_matcher::wallet_entity_id(
                                    &trader,
                                ),
                            trader: Some(trader),
                            recv_unix_ms: Some(t),
                            slot: Some(slot),
                            fee_lamports,
                            cu_consumed,
                            venue: crate::state_ledger::VenueLabel::Pumpswap,
                            event_id: None,
                            feature: None,
                        });
                }
                true
            }
            _ => false,
        };
        if applied {
            st.applied += 1;
            self.model_note_clock(t);
        }
        applied
    }
}
