//! Leaf `ex_route_health`: observed landing outcomes → the health inputs
//! [`RouteCtx`] needs.
//!
//! ## Why this exists
//! [`crate::ex_route_policy::route_ev_lamports`] charges [`Route::Rpc`] a fee of
//! zero, so RPC's entire disadvantage lives in its latency and failure inputs.
//! With those at their default of `0` — exactly what an unwired feed produces —
//! RPC is unbeatable by any tipped route and
//! [`crate::ex_sender_route::choose_submit_plan`] correctly fails closed, never
//! selecting Sender. That gate is honest, but it is also permanent until
//! something actually measures. This module is that something.
//!
//! ## Design
//! A bounded ring of the last [`WINDOW`] submissions per route. Failure rate is
//! the exact integer count over that window in basis points; latency is an
//! integer EWMA with alpha 1/8 (`ewma += (sample - ewma) >> 3`), matching the
//! shift-based EWMA the stream-capture RPC pool already uses so the two report
//! comparable numbers.
//!
//! Under-sampling is treated as *no measurement*, not as *good health*. A route
//! with two observations reporting `fail_bps == 0` would look perfect, and the
//! EV model would believe it. [`RouteHealth::has_measurement`] requires
//! [`MIN_SAMPLES`], and [`RouteHealthSet::fill_route_ctx`] leaves a route's
//! fields at zero until it clears that bar — which keeps
//! [`crate::ex_route_policy::route_health_is_measured`] returning `false` and the
//! system failing closed rather than acting on three data points.
//!
//! ## Constitution refs
//! - §22: counts and rates are integers; failure rate in basis points; latency
//!   in integer milliseconds. No floats anywhere.
//! - Explicit overflow: `saturating_*` on every accumulator.
//! - Determinism: no clock, no RNG, no allocation. Latency is supplied by the
//!   caller who measured it; identical observation sequences give identical
//!   output.

use crate::ex_route_policy::{Route, RouteCtx};

/// Submissions retained per route. Bounded and stack-allocated: the health of a
/// route is a recent-behaviour question, and an unbounded history would both
/// allocate and dilute a regime change.
pub const WINDOW: usize = 64;

/// Observations required before a route reports health at all.
///
/// Below this the window is reported as unmeasured. Sixteen keeps the basis-point
/// resolution of the failure rate meaningful (one failure in 16 is 625 bps) while
/// staying short enough to react inside a session.
pub const MIN_SAMPLES: usize = 16;

/// EWMA smoothing shift: alpha = 1/8 (`delta >> 3`). Same constant the
/// stream-capture RPC pool uses, so latency figures are comparable across the
/// data and execution planes.
pub const EWMA_ALPHA_SHIFT: u32 = 3;

/// One completed submission attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attempt {
    /// Whether the transaction actually landed on chain. A transaction that was
    /// accepted by the endpoint but never confirmed is a FAILURE — accepting a
    /// submission is not landing it, and conflating the two is how a route with
    /// a good API and bad inclusion looks healthy.
    pub landed: bool,
    /// Wall time from submission to observed landing (or to the point the
    /// attempt was abandoned), in milliseconds.
    pub latency_ms: u64,
}

/// Rolling health for a single route.
#[derive(Debug, Clone, Copy)]
pub struct RouteHealth {
    ring: [Attempt; WINDOW],
    /// Next write position.
    head: usize,
    /// Observations currently in the ring, saturating at [`WINDOW`].
    len: usize,
    /// Integer EWMA of latency in milliseconds.
    ewma_ms: u64,
    /// Total attempts ever recorded, for diagnostics. Saturating.
    total: u64,
}

impl Default for RouteHealth {
    fn default() -> Self {
        Self::new()
    }
}

impl RouteHealth {
    /// An empty window. Reports no measurement until [`MIN_SAMPLES`] attempts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ring: [Attempt {
                landed: false,
                latency_ms: 0,
            }; WINDOW],
            head: 0,
            len: 0,
            ewma_ms: 0,
            total: 0,
        }
    }

    /// Record one completed attempt, evicting the oldest when full.
    pub fn record(&mut self, attempt: Attempt) {
        self.ring[self.head] = attempt;
        self.head = (self.head + 1) % WINDOW;
        if self.len < WINDOW {
            self.len += 1;
        }
        self.total = self.total.saturating_add(1);

        // Seed the EWMA with the first sample so it does not spend the first
        // several observations climbing out of a zero it was never at.
        if self.total == 1 {
            self.ewma_ms = attempt.latency_ms;
        } else {
            let prev = self.ewma_ms as i128;
            let sample = attempt.latency_ms as i128;
            let next = prev + ((sample - prev) >> EWMA_ALPHA_SHIFT);
            self.ewma_ms = if next < 0 { 0 } else { next as u64 };
        }
    }

    /// Observations currently in the window.
    #[must_use]
    pub const fn samples(&self) -> usize {
        self.len
    }

    /// Attempts recorded over the lifetime of this tracker.
    #[must_use]
    pub const fn total_attempts(&self) -> u64 {
        self.total
    }

    /// Whether the window holds enough observations to be believed.
    ///
    /// Returns `false` below [`MIN_SAMPLES`]. Callers must treat that as *no
    /// information*, never as *healthy* — see the module docs.
    #[must_use]
    pub const fn has_measurement(&self) -> bool {
        self.len >= MIN_SAMPLES
    }

    /// Failure rate over the window in basis points, or `None` when
    /// under-sampled.
    ///
    /// Exact integer division, not the EWMA: a rate is a count question and
    /// smoothing it would hide a step change in inclusion.
    #[must_use]
    pub fn fail_bps(&self) -> Option<u32> {
        if !self.has_measurement() {
            return None;
        }
        let failures = self.ring[..self.len].iter().filter(|a| !a.landed).count();
        // len <= WINDOW = 64, so this cannot overflow u32.
        Some(((failures * 10_000) / self.len) as u32)
    }

    /// Smoothed latency in milliseconds, or `None` when under-sampled.
    #[must_use]
    pub const fn latency_ms(&self) -> Option<u64> {
        if !self.has_measurement() {
            return None;
        }
        Some(self.ewma_ms)
    }

    /// Whether the route should be offered to the policy at all.
    ///
    /// Fails closed on an unmeasured window: a route nobody has observed is not
    /// assumed healthy. `max_fail_bps` is the caller's tolerance.
    #[must_use]
    pub fn is_healthy(&self, max_fail_bps: u32) -> bool {
        match self.fail_bps() {
            Some(bps) => bps <= max_fail_bps,
            None => false,
        }
    }
}

/// Health for every route the policy can choose between.
#[derive(Debug, Clone, Copy, Default)]
pub struct RouteHealthSet {
    /// RPC-primary path.
    pub rpc: RouteHealth,
    /// Jito bundle path.
    pub jito: RouteHealth,
    /// Nozomi low-latency path.
    pub nozomi: RouteHealth,
    /// Helius Sender path.
    pub sender: RouteHealth,
}

impl RouteHealthSet {
    /// An empty set. Every route reports no measurement.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rpc: RouteHealth::new(),
            jito: RouteHealth::new(),
            nozomi: RouteHealth::new(),
            sender: RouteHealth::new(),
        }
    }

    /// Record an attempt against one of the legacy routes.
    pub fn record(&mut self, route: Route, attempt: Attempt) {
        match route {
            Route::Rpc => self.rpc.record(attempt),
            Route::JitoBundle => self.jito.record(attempt),
            Route::Nozomi => self.nozomi.record(attempt),
        }
    }

    /// Record an attempt made through Helius Sender.
    pub fn record_sender(&mut self, attempt: Attempt) {
        self.sender.record(attempt);
    }

    /// Write measured health into `ctx`, leaving under-sampled routes untouched.
    ///
    /// Under-sampled routes keep whatever the caller already set — zero, by
    /// default — so [`crate::ex_route_policy::route_health_is_measured`] keeps
    /// returning `false` and the plan keeps failing closed. Writing a
    /// confident-looking `0` for a route with three observations is precisely
    /// the failure this module exists to prevent.
    ///
    /// `max_fail_bps` is the tolerance used for the boolean `*_healthy` flags.
    pub fn fill_route_ctx(&self, ctx: &mut RouteCtx, max_fail_bps: u32) {
        if let (Some(ms), Some(bps)) = (self.rpc.latency_ms(), self.rpc.fail_bps()) {
            ctx.rpc_latency_ms = ms;
            ctx.rpc_fail_bps = bps;
        }
        if let (Some(ms), Some(bps)) = (self.jito.latency_ms(), self.jito.fail_bps()) {
            ctx.jito_latency_ms = ms;
            ctx.jito_fail_bps = bps;
            ctx.jito_healthy = self.jito.is_healthy(max_fail_bps);
        }
        if let (Some(ms), Some(bps)) = (self.nozomi.latency_ms(), self.nozomi.fail_bps()) {
            ctx.nozomi_latency_ms = ms;
            ctx.nozomi_fail_bps = bps;
            ctx.nozomi_healthy = self.nozomi.is_healthy(max_fail_bps);
        }
    }

    /// Whether every route the policy compares has cleared [`MIN_SAMPLES`].
    ///
    /// Distinct from [`crate::ex_route_policy::route_health_is_measured`], which
    /// asks whether *any* input is non-zero. This asks the stronger question the
    /// operator actually cares about: is the comparison fully informed?
    #[must_use]
    pub const fn all_legacy_measured(&self) -> bool {
        self.rpc.has_measurement() && self.jito.has_measurement() && self.nozomi.has_measurement()
    }
}
