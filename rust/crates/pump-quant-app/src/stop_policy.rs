//! The ONE stop trigger -> action table for the paper model lane (operator binding, 2026-10-09).
//!
//! Every stop/restriction condition the lane knows is a named [`StopTrigger`]; [`action_for`] is the single
//! place that says what it does. Rules the table encodes (and the tests in `tests/stop_policy_e2e.rs` pin):
//! * Risk-off blocks NEW BUY and ADD only. Reconciliation, REDUCE/EXIT management (wherever a valid verdict
//!   path and execution identity exist) and protection ALWAYS continue. No row stops them.
//! * Inference failure is separate from entry restriction: a hung endpoint stops entry asks; management keeps
//!   asking on its own table and acts on any valid verdict; protection never depends on inference.
//! * Operational warnings (disk, RAM, feed gap, RPC budget) restrict entries while the condition holds and
//!   clear when it clears; they never disable management or protection.
//! * Risk conditions (loss stop, unknown liquidation value, reconciliation fault, durable-write failure,
//!   shadow divergence, hung endpoint) LATCH the durable SAFETY_OFF with a named reason: no auto re-arm.
//! * The run deadline stops entries, starts a bounded DRAIN, then requests the existing protective handoff.
//!   Nothing in this table force-closes a book.
//! * A live signing/submission capability at start refuses the start.
//!
//! Pure: no I/O, no engine state. The engine applies it in `engine/model_stop.rs`.

/// SOL = 1e9 lamports.
pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;
/// Paper loss stop: loss from starting equity at or above this trips (0.5 SOL).
pub const PAPER_LOSS_STOP_LAMPORTS: u64 = LAMPORTS_PER_SOL / 2;
/// Run deadline (6 h) after which no new entries are taken.
pub const RUN_DEADLINE_MS: i64 = 6 * 3_600 * 1_000;
/// Bounded DRAIN after the deadline (30 min): management + protection continue, then the protective handoff
/// is requested. The process still terminates only on flat+reconciled or an acknowledged handoff.
pub const DRAIN_BOUND_MS: i64 = 30 * 60 * 1_000;
/// Label (valuation METRIC ID) of the liquidation estimator in use until the shadow slice lands. PROVISIONAL: the
/// size-specific executable quote at the latest reserve state minus one landed exit leg is a stand-in for the shadow
/// model's net liquidation estimate, not the agreed loss-stop metric. The metric id is pinned at run start
/// ([`crate::engine::model_stop::StopState::valuation_metric`]) and cannot switch mid-run.
pub const ESTIMATOR_EXEC_QUOTE: &str =
    "provisional:exec_quote_size_specific_v1(placeholder_for_shadow_model)";
/// Label when the shadow model supplies the liquidation estimates.
pub const ESTIMATOR_SHADOW: &str = "shadow_model_net_liquidation";

/// Every named stop condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StopTrigger {
    /// Consecutive abandoned/failed model requests (existing SAFETY_OFF rule).
    EndpointHung,
    /// An unresolved entry reconciliation fault or management-sell fault exists.
    ReconciliationFault,
    /// A durable held-state / safety write failed.
    DurableWriteFailure,
    /// Free bytes on the durable-state filesystem below the SOFT floor (or unmeasurable).
    DiskHeadroomLow,
    /// Free bytes on the durable-state filesystem measured below the HARD floor: risk-off latch.
    DiskHardFloor,
    /// Host available memory below the floor (or unmeasurable).
    RamHeadroomLow,
    /// No feed (slot) progress for longer than the existing stale bound.
    FeedGap,
    /// The discovery share of the RPC/data budget is exhausted (held-position reserve untouched).
    RpcBudgetLow,
    /// Shadow model diverged from the books (placeholder hook; the shadow slice is separate).
    ShadowDivergence,
    /// Paper loss from starting equity >= 0.5 SOL by the model liquidation estimate incl. costs.
    PaperLossStop,
    /// A held position has no liquidation value (unknown/stale mark, no route): risk unknown.
    UnknownLiquidationValue,
    /// The 6 h run deadline passed: stop entries, drain, hand off.
    RunDeadline,
    /// A live signing/submission capability is present at start.
    LiveCapabilityPresent,
}

impl StopTrigger {
    /// Every trigger, in table order.
    pub const ALL: [StopTrigger; 13] = [
        Self::EndpointHung,
        Self::ReconciliationFault,
        Self::DurableWriteFailure,
        Self::DiskHeadroomLow,
        Self::DiskHardFloor,
        Self::RamHeadroomLow,
        Self::FeedGap,
        Self::RpcBudgetLow,
        Self::ShadowDivergence,
        Self::PaperLossStop,
        Self::UnknownLiquidationValue,
        Self::RunDeadline,
        Self::LiveCapabilityPresent,
    ];
}

/// What a trigger does to the entry (BUY) and ADD lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Unaffected.
    Allow,
    /// New risk refused by name.
    Block,
}

/// What a trigger does to REDUCE/EXIT management, protection and reconciliation. There is deliberately no
/// `Stop` variant: no trigger may disable them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continue {
    /// Always continues.
    Always,
    /// Continues wherever a valid verdict path and execution identity exist (management).
    IfValid,
}

/// How the restriction ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Latch {
    /// Durable SAFETY_OFF with this reason; only an explicit operator re-arm lifts it.
    SafetyOff(&'static str),
    /// Holds while the measured condition holds; clears when it clears.
    WhileCondition,
    /// Run phase: entries stay stopped for the rest of the run (drain then handoff).
    RunPhase,
    /// The process refuses to start.
    RefuseStart,
}

/// One row of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopAction {
    /// Named action (stable label used in alerts and the report).
    pub name: &'static str,
    /// New BUY entries.
    pub entry: Gate,
    /// ADD to an existing position.
    pub add: Gate,
    /// REDUCE/EXIT management.
    pub manage: Continue,
    /// Protective triggers on held positions.
    pub protect: Continue,
    /// Reconciliation of pending/uncertain orders and reports.
    pub reconcile: Continue,
    /// How it ends.
    pub latch: Latch,
    /// After the drain bound, request the existing protective handoff.
    pub handoff_after_drain: bool,
    /// Named alert surfaced when raised.
    pub alert: &'static str,
}

/// THE table.
#[must_use]
pub const fn action_for(t: StopTrigger) -> StopAction {
    use Continue::{Always, IfValid};
    use Gate::Block;
    const fn row(
        name: &'static str,
        entry: Gate,
        add: Gate,
        latch: Latch,
        alert: &'static str,
    ) -> StopAction {
        StopAction {
            name,
            entry,
            add,
            manage: IfValid,
            protect: Always,
            reconcile: Always,
            latch,
            handoff_after_drain: false,
            alert,
        }
    }
    match t {
        StopTrigger::EndpointHung => row(
            "entry_asks_stopped_management_continues",
            Block,
            Block,
            Latch::SafetyOff(crate::safety_off::REASON_ENDPOINT_HUNG),
            "ALERT_MODEL_ENDPOINT_HUNG",
        ),
        StopTrigger::ReconciliationFault => row(
            "risk_off_reconcile_continues",
            Block,
            Block,
            Latch::SafetyOff("reconciliation_fault"),
            "ALERT_RECONCILIATION_FAULT",
        ),
        StopTrigger::DurableWriteFailure => row(
            "risk_off_durable_write_failed",
            Block,
            Block,
            Latch::SafetyOff("durable_write_failure"),
            "ALERT_DURABLE_WRITE_FAILURE",
        ),
        StopTrigger::DiskHeadroomLow => row(
            "entry_restricted_disk_headroom",
            Block,
            Block,
            Latch::WhileCondition,
            "ALERT_DISK_HEADROOM_LOW",
        ),
        StopTrigger::DiskHardFloor => row(
            "risk_off_disk_hard_floor_evidence_kept",
            Block,
            Block,
            Latch::SafetyOff("disk_hard_floor"),
            "ALERT_DISK_HARD_FLOOR",
        ),
        StopTrigger::RamHeadroomLow => row(
            "entry_restricted_ram_headroom",
            Block,
            Block,
            Latch::WhileCondition,
            "ALERT_RAM_HEADROOM_LOW",
        ),
        StopTrigger::FeedGap => row(
            "entry_restricted_feed_gap",
            Block,
            Block,
            Latch::WhileCondition,
            "ALERT_FEED_GAP",
        ),
        StopTrigger::RpcBudgetLow => row(
            "entry_restricted_rpc_budget_held_reserve_kept",
            Block,
            Block,
            Latch::WhileCondition,
            "ALERT_RPC_BUDGET_LOW",
        ),
        StopTrigger::ShadowDivergence => row(
            "risk_off_shadow_divergence",
            Block,
            Block,
            Latch::SafetyOff("shadow_divergence"),
            "ALERT_SHADOW_DIVERGENCE",
        ),
        StopTrigger::PaperLossStop => row(
            "risk_off_paper_loss_stop",
            Block,
            Block,
            Latch::SafetyOff("paper_loss_stop"),
            "ALERT_PAPER_LOSS_STOP",
        ),
        StopTrigger::UnknownLiquidationValue => row(
            "risk_unknown_stop_new_exposure",
            Block,
            Block,
            Latch::SafetyOff("risk_unknown_liquidation_value"),
            "ALERT_RISK_UNKNOWN_LIQUIDATION_VALUE",
        ),
        StopTrigger::RunDeadline => StopAction {
            handoff_after_drain: true,
            ..row(
                "deadline_stop_entries_drain_then_handoff",
                Block,
                Block,
                Latch::RunPhase,
                "ALERT_RUN_DEADLINE_DRAIN",
            )
        },
        StopTrigger::LiveCapabilityPresent => row(
            "refuse_start_live_capability",
            Block,
            Block,
            Latch::RefuseStart,
            "FATAL_LIVE_CAPABILITY_PRESENT",
        ),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Paper loss stop / risk-unknown valuation (inputs exposed; the estimator is a parameter).
// ---------------------------------------------------------------------------------------------------------------

/// One held position's liquidation value under ONE estimator: lamports net of every cost, or a named reason
/// it is unknown. Never zero-filled, never replaced by cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiquidationEstimate {
    /// Market.
    pub mint: [u8; 32],
    /// Net liquidation value (lamports; may be negative when costs exceed proceeds) or the named unavailable reason.
    pub value: Result<i128, &'static str>,
}

/// The valuation verdict for one estimator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskValuation {
    /// Every held position valued: equity and loss from start.
    Known {
        /// Cash + sum of liquidation values, lamports.
        equity: i128,
        /// starting - equity (positive = loss), lamports.
        loss_from_start: i128,
    },
    /// At least one held position has no value: the first such mint and reason. Risk is UNKNOWN.
    Unknown {
        /// The first unvalued position.
        mint: [u8; 32],
        /// Why.
        reason: &'static str,
    },
}

/// Value the book under one estimator. `cash` is cash on hand (seed + realized - committed entry spend).
#[must_use]
pub fn value_book(starting: u64, cash: i128, positions: &[LiquidationEstimate]) -> RiskValuation {
    let mut equity = cash;
    for p in positions {
        match p.value {
            Ok(v) => equity += v,
            Err(reason) => {
                return RiskValuation::Unknown {
                    mint: p.mint,
                    reason,
                }
            }
        }
    }
    RiskValuation::Known {
        equity,
        loss_from_start: i128::from(starting) - equity,
    }
}

/// The trigger a MODEL-ESTIMATE valuation raises, if any. The external-state valuation is reported beside it
/// and never consulted here (never pick the better of the two).
#[must_use]
pub fn valuation_trigger(model: &RiskValuation) -> Option<StopTrigger> {
    match model {
        RiskValuation::Unknown { .. } => Some(StopTrigger::UnknownLiquidationValue),
        RiskValuation::Known {
            loss_from_start, ..
        } if *loss_from_start >= i128::from(PAPER_LOSS_STOP_LAMPORTS) => {
            Some(StopTrigger::PaperLossStop)
        }
        RiskValuation::Known { .. } => None,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Run deadline phases.
// ---------------------------------------------------------------------------------------------------------------

/// Where the run is relative to its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhase {
    /// Before the deadline.
    Running,
    /// Deadline passed, inside the drain bound: no entries; management and protection continue.
    Drain {
        /// Elapsed run time at which the drain ends.
        ends_at_ms: i64,
    },
    /// Drain bound passed: request the protective handoff (never a force close).
    HandoffDue,
}

/// Phase at `elapsed_ms` since start for a run with `deadline_ms` and `drain_ms`.
#[must_use]
pub fn run_phase(elapsed_ms: i64, deadline_ms: i64, drain_ms: i64) -> RunPhase {
    if elapsed_ms < deadline_ms {
        RunPhase::Running
    } else if elapsed_ms < deadline_ms.saturating_add(drain_ms) {
        RunPhase::Drain {
            ends_at_ms: deadline_ms.saturating_add(drain_ms),
        }
    } else {
        RunPhase::HandoffDue
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Startup paper-only check.
// ---------------------------------------------------------------------------------------------------------------

/// What the process can do at start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartCapability {
    /// `--live` was passed.
    pub live_flag: bool,
    /// A signing keypair has been loaded.
    pub keypair_loaded: bool,
    /// An outbound submission sink is installed (paper mode has none).
    pub submission_sink_installed: bool,
    /// The engine runs in paper mode.
    pub paper_mode: bool,
}

/// Refuse to start unless no live signing/submission capability exists.
///
/// # Errors
/// The first capability found, by name.
pub fn require_paper_only(c: StartCapability) -> Result<(), &'static str> {
    if c.live_flag {
        return Err("live_flag_present");
    }
    if c.keypair_loaded {
        return Err("keypair_loaded");
    }
    if c.submission_sink_installed {
        return Err("submission_sink_not_paper");
    }
    if !c.paper_mode {
        return Err("engine_not_paper_mode");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// Capacity reservation for held-position support (RPC/data budget, subscription slots).
// ---------------------------------------------------------------------------------------------------------------

/// A shared capacity in which held-position support is reserved before discovery may spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldReserve {
    /// Total capacity in the window (requests, pages or slots).
    pub capacity: u64,
    /// Units reserved per held position.
    pub per_held: u64,
}

impl HeldReserve {
    /// Units held positions are entitled to right now.
    #[must_use]
    pub fn reserved(&self, held: u64) -> u64 {
        self.per_held.saturating_mul(held).min(self.capacity)
    }

    /// Whether DISCOVERY may spend `n` more units given `used` already spent in the window: never into the
    /// held reserve.
    #[must_use]
    pub fn discovery_may_spend(&self, used: u64, held: u64, n: u64) -> bool {
        used.saturating_add(n) <= self.capacity.saturating_sub(self.reserved(held))
    }

    /// Whether HELD-POSITION support may spend `n` more units: up to the full capacity.
    #[must_use]
    pub fn held_may_spend(&self, used: u64, n: u64) -> bool {
        used.saturating_add(n) <= self.capacity
    }
}

/// A fixed-window budget meter (e.g. RPC pages per hour) with a held-position reservation.
#[derive(Debug, Clone)]
pub struct WindowBudget {
    /// The reservation rule.
    pub rule: HeldReserve,
    /// Window length, ms.
    pub window_ms: i64,
    window_start_ms: i64,
    used: u64,
    /// Discovery requests refused because they would eat the reserve (this window).
    pub discovery_refused: u64,
}

impl WindowBudget {
    /// A fresh meter.
    #[must_use]
    pub fn new(rule: HeldReserve, window_ms: i64) -> Self {
        Self {
            rule,
            window_ms,
            window_start_ms: i64::MIN,
            used: 0,
            discovery_refused: 0,
        }
    }

    fn roll(&mut self, now_ms: i64) {
        if self.window_start_ms == i64::MIN || now_ms - self.window_start_ms >= self.window_ms {
            self.window_start_ms = now_ms;
            self.used = 0;
            self.discovery_refused = 0;
        }
    }

    /// Spend `n` for discovery if it leaves the held reserve intact.
    pub fn try_discovery(&mut self, now_ms: i64, held: u64, n: u64) -> bool {
        self.roll(now_ms);
        if self.rule.discovery_may_spend(self.used, held, n) {
            self.used += n;
            true
        } else {
            self.discovery_refused += 1;
            false
        }
    }

    /// Spend `n` for held-position support (may use the reserve).
    pub fn try_held(&mut self, now_ms: i64, n: u64) -> bool {
        self.roll(now_ms);
        if self.rule.held_may_spend(self.used, n) {
            self.used += n;
            true
        } else {
            false
        }
    }

    /// The discovery share is exhausted in the current window (the `RpcBudgetLow` condition).
    #[must_use]
    pub fn discovery_exhausted(&self, held: u64) -> bool {
        self.discovery_refused > 0 || !self.rule.discovery_may_spend(self.used, held, 1)
    }

    /// Units spent in the current window.
    #[must_use]
    pub fn used(&self) -> u64 {
        self.used
    }
}

/// Measured operational conditions handed to the engine each evaluation (`None` = could not be measured,
/// which restricts entries by name like a low reading - never assumed fine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpsInputs {
    /// Disk headroom at or above the SOFT floor on the durable-state filesystem (`None` = unmeasurable).
    pub disk_ok: Option<bool>,
    /// Disk headroom at or above the HARD floor. Only a MEASURED `Some(false)` latches; `None` is covered by the
    /// soft row (unmeasurable restricts new risk).
    pub disk_hard_ok: Option<bool>,
    /// RAM headroom ok on the host.
    pub ram_ok: Option<bool>,
    /// Feed (slot) progress is within the stale bound.
    pub feed_ok: bool,
    /// The discovery share of the RPC budget is not exhausted.
    pub rpc_budget_ok: bool,
    /// Shadow divergence reported by the shadow slice (placeholder: always false until it lands).
    pub shadow_divergence: bool,
    /// The run deadline has passed (drain or handoff phase).
    pub deadline_passed: bool,
}

impl OpsInputs {
    /// Every condition healthy.
    #[must_use]
    pub const fn healthy() -> Self {
        Self {
            disk_ok: Some(true),
            disk_hard_ok: Some(true),
            ram_ok: Some(true),
            feed_ok: true,
            rpc_budget_ok: true,
            shadow_divergence: false,
            deadline_passed: false,
        }
    }
}

/// RAM floor from `/proc/meminfo`-style numbers: available must be >= `floor_bps` of total.
#[must_use]
pub fn ram_ok(
    mem_total_kb: Option<u64>,
    mem_available_kb: Option<u64>,
    floor_bps: u64,
) -> Option<bool> {
    let (t, a) = (mem_total_kb?, mem_available_kb?);
    if t == 0 {
        return None;
    }
    Some(u128::from(a) * 10_000 >= u128::from(t) * u128::from(floor_bps))
}

/// Parse `MemTotal` / `MemAvailable` (kB) out of `/proc/meminfo` text.
#[must_use]
pub fn parse_meminfo(text: &str) -> (Option<u64>, Option<u64>) {
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k))
            .and_then(|r| r.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
    };
    (get("MemTotal:"), get("MemAvailable:"))
}

/// Host RAM floor: the operator's generic "keep >= 12% RAM free" build rule. SUPERSEDED for the run's stop input
/// by [`RAM_FLOOR_BYTES`] (workload-measured); kept for the parse/threshold helper it pins.
pub const RAM_FLOOR_BPS: u64 = 1_200;

// ---------------------------------------------------------------------------------------------------------------
// Resource floors (measured basis: /training/mh_build/proc/RESOURCE_BUDGET.md).
// ---------------------------------------------------------------------------------------------------------------

/// GiB.
pub const GIB: u64 = 1 << 30;
/// Disk SOFT floor on the durable-state filesystem: below it new risk is restricted and ALERT_DISK_HEADROOM_LOW is
/// raised (clears when space returns). 20 GiB >= the whole 6 h + 30 min run's measured requirement at the max
/// 1-minute capture rate (event stream 16.0 GB + checkpoints/logs ~1.2 GB), so a run that crosses it still has room
/// to finish its drain at burst rate without deleting anything.
pub const DISK_SOFT_FLOOR_BYTES: u64 = 20 * GIB;
/// Disk HARD floor: measured free bytes below it latch SAFETY_OFF (`disk_hard_floor`). 4 GiB keeps > 30 min of
/// event stream at the max measured 1-minute rate (41.1 MB/min -> 1.23 GB) + two flow checkpoints + journals, so
/// required evidence keeps being written while held positions are managed/protected. Nothing is ever deleted.
pub const DISK_HARD_FLOOR_BYTES: u64 = 4 * GIB;
/// RAM floor (bytes of memory still AVAILABLE to the daemon, i.e. min(host MemAvailable, cgroup memory.max -
/// memory.current)): 12 GiB = 2x the daemon's projected 6.5 h VmHWM (5.95 GB, linear fit of a measured offline
/// replay of 670 s of captured wire). Not host-wide 12%, not the tool's 4 GiB.
pub const RAM_FLOOR_BYTES: u64 = 12 * GIB;

/// The cgroup memory CEILING that actually applies to this process: the lower of `memory.max` (OOM kill) and
/// `memory.high` (reclaim throttling, which stalls the event loop before any kill). `None` = both unlimited.
#[must_use]
pub fn cgroup_mem_ceiling(max: Option<u64>, high: Option<u64>) -> Option<u64> {
    match (max, high) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Memory available to this process: host `MemAvailable` (kB) capped by its cgroup's remaining room
/// (`memory.max` - `memory.current`, bytes; `None` max = unlimited). `None` when MemAvailable is unknown.
#[must_use]
pub fn mem_available_bytes(
    mem_available_kb: Option<u64>,
    cgroup_max: Option<u64>,
    cgroup_current: Option<u64>,
) -> Option<u64> {
    let host = mem_available_kb?.saturating_mul(1024);
    Some(match cgroup_max {
        None => host,
        Some(max) => host.min(max.saturating_sub(cgroup_current?)),
    })
}

/// Parse a cgroup v2 `memory.max`-style value: `"max"` -> `Ok(None)` (unlimited), a number -> `Ok(Some(n))`.
///
/// # Errors
/// Unparseable text (the caller treats the reading as unmeasurable).
#[allow(clippy::result_unit_err)] // public API kept as-is: changing the error type would change callers
pub fn parse_cgroup_limit(text: &str) -> Result<Option<u64>, ()> {
    let t = text.trim();
    if t == "max" {
        return Ok(None);
    }
    t.parse::<u64>().map(Some).map_err(|_| ())
}

/// Whether `available` meets `floor` (`None` = unmeasurable, never assumed fine).
#[must_use]
pub fn bytes_ok(available: Option<u64>, floor: u64) -> Option<bool> {
    available.map(|a| a >= floor)
}
