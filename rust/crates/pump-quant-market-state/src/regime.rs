//! `MarketRegimeState` — deterministic, time-safe, independently observable
//! market regime.
//!
//! ## Responsibility
//! Reduce market-wide observations into a **multi-dimensional** regime state
//! whose components are each an ordinal level, and which is **never collapsed
//! into a composite score** (§21.3: "a deterministic, time-safe, independently
//! observable regime state. Components may include: SOL price shock,
//! market-wide launch velocity, aggregate graduation rate, market-wide buy/sell
//! imbalance, aggregate rug/collapse rate, network congestion, fee regime,
//! route degradation, liquidity regime. Never collapse it invisibly into a
//! composite score."). Used for strategy eligibility, exposure throttling,
//! walk-forward stratification, etc. — it passes feature admission like any
//! other feature family.
//!
//! ## Determinism & bounds
//! Two parts, both pure integer (§22):
//! * [`MarketRegimeReducer`] — accumulates integer market-wide counters over a
//!   caller-defined window into a [`RegimeObservation`]. O(1) state.
//! * [`classify`] — a pure function mapping a [`RegimeObservation`] plus
//!   versioned [`RegimeThresholds`] to a [`MarketRegimeState`]. No clock, no
//!   float, no RNG.

use crate::common::signed_ratio_bps;

/// A generic four-step ordinal regime level.
///
/// ## Responsibility
/// The shared, inspectable scale for regime components whose severity is
/// one-directional (higher = more stressed / more active). Kept ordinal (not a
/// number) so no component is silently arithmetically combined with another
/// (§21.3 no-composite rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RegimeLevel {
    /// Below the normal band.
    Low,
    /// Normal band.
    Normal,
    /// Above normal.
    Elevated,
    /// Extreme.
    High,
}

/// A signed three-step ordinal for symmetric components (imbalance, price
/// shock) where both directions matter.
///
/// Constitution: §21.3 (market-wide buy/sell imbalance; SOL price shock).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Skew {
    /// Strongly negative (sell-skewed / price crash).
    StrongDown,
    /// Mildly negative.
    Down,
    /// Balanced.
    Neutral,
    /// Mildly positive.
    Up,
    /// Strongly positive (buy-skewed / price spike).
    StrongUp,
}

/// Raw, integer, market-wide observation over one window.
///
/// ## Responsibility
/// The deterministic input to [`classify`]. Every field is an integer counter
/// or a signed bps delta computed by the caller / [`MarketRegimeReducer`]; no
/// floats (§22). Missing dimensions are represented by `None` and classified as
/// UNKNOWN (§6.4), never as a fabricated default.
#[derive(Clone, Copy, Debug, Default)]
pub struct RegimeObservation {
    /// SOL price change over the window in signed bps (e.g. -1500 = -15%).
    /// `None` when the external SOL price feed is unavailable.
    pub sol_price_change_bps: Option<i64>,
    /// Count of new launches observed in the window.
    pub launches: u64,
    /// Count of graduations observed in the window.
    pub graduations: u64,
    /// Count of market-wide buys in the window.
    pub buys: u64,
    /// Count of market-wide sells in the window.
    pub sells: u64,
    /// Count of markets that rugged / collapsed in the window.
    pub rugs: u64,
    /// Count of markets that were live/eligible in the window (denominator for
    /// rug/collapse rate). `None` when unknown.
    pub live_markets: Option<u64>,
    /// Median priority fee (micro-lamports per compute unit) in the window.
    /// `None` when unobserved.
    pub median_priority_fee: Option<u64>,
    /// Fraction of recent slots that were full/congested, in bps. `None` when
    /// unobserved.
    pub slot_fullness_bps: Option<u64>,
    /// Count of route/submission attempts in the window.
    pub route_attempts: u64,
    /// Count of route/submission failures in the window.
    pub route_failures: u64,
    /// Aggregate executable liquidity index (caller-defined integer units,
    /// higher = deeper). `None` when unobserved.
    pub liquidity_index: Option<u64>,
}

/// Versioned threshold set for [`classify`].
///
/// ## Responsibility
/// Every cut point is explicit and versioned (§102 no silent magic numbers).
/// Bands are half-open `[low, high)`: `< low` => below-normal, `>= high` =>
/// above-normal.
#[derive(Clone, Copy, Debug)]
pub struct RegimeThresholds {
    /// Taxonomy/threshold version stamped onto the produced state.
    pub version: u32,
    /// SOL price-change bps bands: (strong_down, down, up, strong_up).
    /// e.g. (-1000, -300, 300, 1000).
    pub sol_shock_bps: (i64, i64, i64, i64),
    /// Launch-velocity bands (launches per window): (low, elevated, high).
    pub launch_velocity: (u64, u64, u64),
    /// Graduation-rate bps bands (graduations*1e4/launches): (low, elevated, high).
    pub graduation_rate_bps: (u64, u64, u64),
    /// Buy/sell imbalance bps bands: (strong_down, down, up, strong_up).
    pub imbalance_bps: (i64, i64, i64, i64),
    /// Rug/collapse-rate bps bands (rugs*1e4/live_markets): (low, elevated, high).
    pub rug_rate_bps: (u64, u64, u64),
    /// Congestion bands on slot_fullness_bps: (low, elevated, high).
    pub congestion_bps: (u64, u64, u64),
    /// Fee-regime bands on median_priority_fee: (low, elevated, high).
    pub fee_regime: (u64, u64, u64),
    /// Route-degradation bps bands (failures*1e4/attempts): (low, elevated, high).
    pub route_degradation_bps: (u64, u64, u64),
    /// Liquidity-regime bands on liquidity_index: (low, elevated, high). Note
    /// liquidity is *inverted* in stress terms — see [`classify`].
    pub liquidity_index: (u64, u64, u64),
}

impl Default for RegimeThresholds {
    /// Illustrative default v0 thresholds for tests / bootstrapping. Production
    /// supplies calibrated, versioned values.
    fn default() -> Self {
        RegimeThresholds {
            version: 0,
            sol_shock_bps: (-1000, -300, 300, 1000),
            launch_velocity: (5, 50, 200),
            graduation_rate_bps: (100, 500, 1500),
            imbalance_bps: (-3000, -1000, 1000, 3000),
            rug_rate_bps: (500, 2000, 5000),
            congestion_bps: (3000, 6000, 8500),
            fee_regime: (10_000, 100_000, 1_000_000),
            route_degradation_bps: (500, 2000, 5000),
            liquidity_index: (100, 1_000, 10_000),
        }
    }
}

/// The deterministic, multi-dimensional market regime state.
///
/// ## Responsibility
/// Each component is stored separately and independently inspectable
/// (§21.3, criterion 47). There is deliberately **no** `overall` /composite
/// field. Components are `Option` so that missing observations classify as
/// UNKNOWN rather than a fabricated level (§6.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarketRegimeState {
    /// Threshold/taxonomy version that produced this state.
    pub version: u32,
    /// SOL price-shock skew. `None` when the SOL feed is unavailable.
    pub sol_price_shock: Option<Skew>,
    /// Market-wide launch velocity.
    pub launch_velocity: RegimeLevel,
    /// Aggregate graduation rate. `None` when there were zero launches.
    pub graduation_rate: Option<RegimeLevel>,
    /// Market-wide buy/sell imbalance skew. `None` when there was zero flow.
    pub buy_sell_imbalance: Option<Skew>,
    /// Aggregate rug/collapse rate. `None` when live-market count is unknown or
    /// zero.
    pub rug_collapse_rate: Option<RegimeLevel>,
    /// Network congestion. `None` when slot fullness is unobserved.
    pub network_congestion: Option<RegimeLevel>,
    /// Fee regime. `None` when the fee feed is unobserved.
    pub fee_regime: Option<RegimeLevel>,
    /// Route degradation. `None` when there were zero route attempts.
    pub route_degradation: Option<RegimeLevel>,
    /// Liquidity regime (in *stress* orientation: `High` == liquidity crisis).
    /// `None` when liquidity is unobserved.
    pub liquidity_regime: Option<RegimeLevel>,
}

/// Bucket an unsigned value into a [`RegimeLevel`] with `(low, elevated, high)`
/// cut points. `< low` => Low, `[low, elevated)` => Normal,
/// `[elevated, high)` => Elevated, `>= high` => High.
#[must_use]
fn bucket(value: u64, bands: (u64, u64, u64)) -> RegimeLevel {
    let (low, elevated, high) = bands;
    if value >= high {
        RegimeLevel::High
    } else if value >= elevated {
        RegimeLevel::Elevated
    } else if value >= low {
        RegimeLevel::Normal
    } else {
        RegimeLevel::Low
    }
}

/// Bucket an unsigned value into an *inverted*-stress [`RegimeLevel`], for
/// liquidity where *lower* raw depth means *higher* stress. `>= high` => Low
/// stress, ... `< low` => High stress.
#[must_use]
fn bucket_inverted(value: u64, bands: (u64, u64, u64)) -> RegimeLevel {
    let (low, elevated, high) = bands;
    if value >= high {
        RegimeLevel::Low
    } else if value >= elevated {
        RegimeLevel::Normal
    } else if value >= low {
        RegimeLevel::Elevated
    } else {
        RegimeLevel::High
    }
}

/// Bucket a signed bps value into a [`Skew`] with
/// `(strong_down, down, up, strong_up)` cut points.
#[must_use]
fn bucket_skew(value: i64, bands: (i64, i64, i64, i64)) -> Skew {
    let (strong_down, down, up, strong_up) = bands;
    if value <= strong_down {
        Skew::StrongDown
    } else if value <= down {
        Skew::Down
    } else if value >= strong_up {
        Skew::StrongUp
    } else if value >= up {
        Skew::Up
    } else {
        Skew::Neutral
    }
}

/// Classify a [`RegimeObservation`] into a [`MarketRegimeState`] using versioned
/// [`RegimeThresholds`].
///
/// ## Responsibility
/// The pure, deterministic, per-dimension classifier (§21.3, §22). Each
/// component is derived independently; nothing is summed into a scalar. Missing
/// inputs propagate as `None` (UNKNOWN, §6.4) rather than being defaulted.
///
/// Ratios use the crate's checked fixed-point helpers; e.g. graduation rate is
/// `graduations * 10_000 / launches` in bps.
#[must_use]
pub fn classify(obs: &RegimeObservation, th: &RegimeThresholds) -> MarketRegimeState {
    let sol_price_shock = obs
        .sol_price_change_bps
        .map(|bps| bucket_skew(bps, th.sol_shock_bps));

    let launch_velocity = {
        let (low, elevated, high) = th.launch_velocity;
        bucket(obs.launches, (low, elevated, high))
    };

    let graduation_rate = if obs.launches == 0 {
        None
    } else {
        // graduations can exceed launches across windows (a token launched in a
        // prior window graduates now); the ratio still classifies fine.
        let bps = crate::common::ratio_bps(u128::from(obs.graduations), u128::from(obs.launches))
            .unwrap_or(0);
        Some(bucket(bps, th.graduation_rate_bps))
    };

    let buy_sell_imbalance = {
        let total = obs.buys.saturating_add(obs.sells);
        if total == 0 {
            None
        } else {
            let net = i128::from(obs.buys) - i128::from(obs.sells);
            let bps = signed_ratio_bps(net, i128::from(total)).unwrap_or(0);
            Some(bucket_skew(bps, th.imbalance_bps))
        }
    };

    let rug_collapse_rate = match obs.live_markets {
        Some(live) if live > 0 => {
            let bps = crate::common::ratio_bps(u128::from(obs.rugs), u128::from(live)).unwrap_or(0);
            Some(bucket(bps, th.rug_rate_bps))
        }
        _ => None,
    };

    let network_congestion = obs.slot_fullness_bps.map(|f| bucket(f, th.congestion_bps));

    let fee_regime = obs.median_priority_fee.map(|f| bucket(f, th.fee_regime));

    let route_degradation = if obs.route_attempts == 0 {
        None
    } else {
        let bps = crate::common::ratio_bps(
            u128::from(obs.route_failures),
            u128::from(obs.route_attempts),
        )
        .unwrap_or(0);
        Some(bucket(bps, th.route_degradation_bps))
    };

    let liquidity_regime = obs
        .liquidity_index
        .map(|l| bucket_inverted(l, th.liquidity_index));

    MarketRegimeState {
        version: th.version,
        sol_price_shock,
        launch_velocity,
        graduation_rate,
        buy_sell_imbalance,
        rug_collapse_rate,
        network_congestion,
        fee_regime,
        route_degradation,
        liquidity_regime,
    }
}

/// Market-wide event feeding the [`MarketRegimeReducer`].
///
/// Constitution: §21.3 component list.
#[derive(Clone, Copy, Debug)]
pub enum MarketEvent {
    /// A new token launch.
    Launch,
    /// A graduation/migration event.
    Graduation,
    /// A market-wide buy.
    Buy,
    /// A market-wide sell.
    Sell,
    /// A rug/collapse terminal event for some market.
    Rug,
    /// A route/submission attempt with its success outcome.
    RouteAttempt {
        /// Whether the attempt landed successfully.
        succeeded: bool,
    },
}

/// Accumulates [`MarketEvent`]s plus externally-sampled scalars into a
/// [`RegimeObservation`].
///
/// ## Responsibility
/// The event-stream half of the regime feature (§21.3). O(1) state, saturating
/// counters (§22, §99). External scalars (SOL price change, fee, congestion,
/// liquidity, live-market count) are *set* by the caller from their own
/// time-safe feeds rather than being invented here.
#[derive(Clone, Debug, Default)]
pub struct MarketRegimeReducer {
    launches: u64,
    graduations: u64,
    buys: u64,
    sells: u64,
    rugs: u64,
    route_attempts: u64,
    route_failures: u64,
    sol_price_change_bps: Option<i64>,
    live_markets: Option<u64>,
    median_priority_fee: Option<u64>,
    slot_fullness_bps: Option<u64>,
    liquidity_index: Option<u64>,
}

impl MarketRegimeReducer {
    /// Create an empty reducer.
    #[must_use]
    pub fn new() -> Self {
        MarketRegimeReducer::default()
    }

    /// Ingest one market-wide event (saturating counters).
    pub fn ingest(&mut self, ev: &MarketEvent) {
        match *ev {
            MarketEvent::Launch => self.launches = self.launches.saturating_add(1),
            MarketEvent::Graduation => self.graduations = self.graduations.saturating_add(1),
            MarketEvent::Buy => self.buys = self.buys.saturating_add(1),
            MarketEvent::Sell => self.sells = self.sells.saturating_add(1),
            MarketEvent::Rug => self.rugs = self.rugs.saturating_add(1),
            MarketEvent::RouteAttempt { succeeded } => {
                self.route_attempts = self.route_attempts.saturating_add(1);
                if !succeeded {
                    self.route_failures = self.route_failures.saturating_add(1);
                }
            }
        }
    }

    /// Set the externally-sampled SOL price change (signed bps) for the window.
    pub fn set_sol_price_change_bps(&mut self, bps: i64) {
        self.sol_price_change_bps = Some(bps);
    }

    /// Set the live/eligible market count (rug-rate denominator).
    pub fn set_live_markets(&mut self, live: u64) {
        self.live_markets = Some(live);
    }

    /// Set the median priority fee for the window.
    pub fn set_median_priority_fee(&mut self, fee: u64) {
        self.median_priority_fee = Some(fee);
    }

    /// Set the slot-fullness congestion measure (bps) for the window.
    pub fn set_slot_fullness_bps(&mut self, bps: u64) {
        self.slot_fullness_bps = Some(bps);
    }

    /// Set the aggregate liquidity index for the window.
    pub fn set_liquidity_index(&mut self, index: u64) {
        self.liquidity_index = Some(index);
    }

    /// Produce the accumulated observation.
    #[must_use]
    pub fn observation(&self) -> RegimeObservation {
        RegimeObservation {
            sol_price_change_bps: self.sol_price_change_bps,
            launches: self.launches,
            graduations: self.graduations,
            buys: self.buys,
            sells: self.sells,
            rugs: self.rugs,
            live_markets: self.live_markets,
            median_priority_fee: self.median_priority_fee,
            slot_fullness_bps: self.slot_fullness_bps,
            route_attempts: self.route_attempts,
            route_failures: self.route_failures,
            liquidity_index: self.liquidity_index,
        }
    }

    /// Classify the accumulated observation with the given thresholds.
    #[must_use]
    pub fn classify(&self, th: &RegimeThresholds) -> MarketRegimeState {
        classify(&self.observation(), th)
    }
}
