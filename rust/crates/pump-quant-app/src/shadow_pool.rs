//! `paper_fill_v2_shadow` — the SHADOW POOL paper fill model (spec:
//! `docs/missing_history_causal/SHADOW_POOL_SPEC.md`).
//!
//! A paper fill never reached the chain, so the observed reserves do not contain it. The v1 executor
//! (`exec_quote.rs` on the observed state) therefore prices our SELL against a curve that never received our
//! BUY's SOL. The shadow pool keeps, PER MARKET, only OUR cumulative simulated net effect (SOL into / out of
//! the reserves, tokens out of / into the reserves) and prices OUR next fill on
//! `latest observed reserves + our delta`. Nothing here ever writes an observed reserve record.
//!
//! RULES (each is tested in `tests/shadow_pool.rs`):
//! * Observed state is immutable evidence. Its size-specific quote stays the EXTERNAL-LIQUIDITY BENCHMARK and
//!   is always reported next to the shadow quote ([`ShadowSellView`]).
//! * Our delta is applied idempotently, keyed by fill identity `(leg, order id)` and the order's CUMULATIVE
//!   (tokens, lamports): a duplicate report changes nothing, a backwards report is a named fault.
//! * A fresh observation REPLACES the base; our delta is carried on top only while it reconciles:
//!   - curve: the curve's trade invariants (`vsol - real_sol`, `vtok - real_tok`) must be unchanged and the curve
//!     not complete; a change is a non-trade liquidity change -> `adverse_liquidity_change`, delta DROPPED;
//!   - pool: the constant product never decreases under swaps; a decrease is a liquidity withdrawal ->
//!     `adverse_liquidity_change`, delta DROPPED;
//!   - observed real SOL + our conserved SOL contribution below zero, or observed real tokens below the tokens
//!     we took out -> `unreconcilable_snapshot`, delta DROPPED.
//!   A dropped delta never comes back: sale capacity falls back to what the observed state supports.
//! * SALE CAPACITY is conserved: a shadow sell's gross never exceeds `observed real SOL (pool: quote vault) +
//!   our conserved net SOL contribution` (`max(0, sol_in - sol_out)` of OUR fills in this venue segment).
//!   Never invented liquidity.
//! * Graduation ends the curve shadow (`curve_shadow_ended:graduation`); the pool shadow starts from the observed
//!   pool state with an EMPTY delta. Our tokens are inventory, not liquidity; our curve SOL is not carried.
//! * EXPLICIT ASSUMPTION: other participants behave exactly as observed. Replay cannot establish the
//!   counterfactual (they might have traded differently against reserves that contained our fills).
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::exec_quote::{QuoteRefusal, SellQuote};
use pump_quant_protocol::pumpswap_event::CashbackField;

/// The fill-model version string this module implements (reported with every shadow result).
pub const PAPER_FILL_V2_SHADOW: &str = "paper_fill_v2_shadow";
/// The v1 (observed-state-only) fill model's version string.
pub const PAPER_FILL_V1: &str = "paper_fill_v1_observed";

/// Which paper fill model prices OUR fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaperFillVersion {
    /// Observed reserves only (the external-liquidity quote is the execution price).
    #[default]
    V1Observed,
    /// Observed reserves + our conserved delta (this module).
    V2Shadow,
}

impl PaperFillVersion {
    /// Stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::V1Observed => PAPER_FILL_V1,
            Self::V2Shadow => PAPER_FILL_V2_SHADOW,
        }
    }
    /// Parse the label (env / config). Unknown -> `None` (never a default).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            PAPER_FILL_V1 => Some(Self::V1Observed),
            PAPER_FILL_V2_SHADOW => Some(Self::V2Shadow),
            _ => None,
        }
    }
}

/// Which leg kind a fill identity belongs to (entry and management orders have separate id namespaces).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LegKind {
    /// Entry BUY (entry-order namespace).
    Entry,
    /// ADD (management namespace).
    Add,
    /// REDUCE / EXIT / protective sell (management namespace).
    Sell,
}

impl LegKind {
    /// Stable durable code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Entry => "entry",
            Self::Add => "add",
            Self::Sell => "sell",
        }
    }
    /// Parse a durable code (unknown -> `None`).
    #[must_use]
    pub fn from_code(s: &str) -> Option<Self> {
        match s {
            "entry" => Some(Self::Entry),
            "add" => Some(Self::Add),
            "sell" => Some(Self::Sell),
            _ => None,
        }
    }
    const fn is_buy(self) -> bool {
        !matches!(self, Self::Sell)
    }
}

/// The venue segment a shadow belongs to. A graduation starts a NEW segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowVenue {
    /// Bonding curve.
    Curve,
    /// Canonical WSOL pool.
    Pool,
}

/// An observed curve state (evidence; never modified here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurveBase {
    pub vsol: u64,
    pub vtok: u64,
    pub real_sol: u64,
    pub real_tok: u64,
    pub slot: u64,
}

/// An observed pool state (evidence; never modified here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolBase {
    /// Token reserve.
    pub base: u64,
    /// Real WSOL quote vault.
    pub quote: u64,
    /// Virtual quote reserve on the landing event.
    pub vq: u64,
    pub slot: u64,
}

/// The observed state OUR fill was priced on (the reconciliation basis recorded with the fill).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillBasis {
    /// Curve landing state.
    Curve(CurveBase),
    /// Pool landing state.
    Pool(PoolBase),
}

impl FillBasis {
    const fn venue(self) -> ShadowVenue {
        match self {
            Self::Curve(_) => ShadowVenue::Curve,
            Self::Pool(_) => ShadowVenue::Pool,
        }
    }
}

fn pool_k(b: &PoolBase) -> u128 {
    u128::from(b.base) * (u128::from(b.quote) + u128::from(b.vq))
}

/// Why a market's delta stopped being carried (named; reported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Divergence {
    /// A non-trade change in observed liquidity (curve token offset `vtok - real_tok` moved / curve completed
    /// without a graduation call / pool constant product decreased).
    AdverseLiquidityChange,
    /// The curve's virtual SOL offset `vsol - real_sol` moved between snapshots: the observed transition is not
    /// explained by constant-product trades, so the additive price basis of our delta is falsified by evidence.
    CurveVirtualOffsetChanged,
    /// The observed state cannot contain what the shadow says we withdrew (real SOL or real tokens too low).
    UnreconcilableSnapshot,
}

impl Divergence {
    /// Stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::AdverseLiquidityChange => "shadow_divergence:adverse_liquidity_change",
            Self::UnreconcilableSnapshot => "shadow_divergence:unreconcilable_snapshot",
            Self::CurveVirtualOffsetChanged => "shadow_divergence:curve_virtual_offset_changed",
        }
    }
    /// Inverse of [`Self::label`].
    #[must_use]
    pub fn from_label(s: &str) -> Option<Self> {
        [
            Self::AdverseLiquidityChange,
            Self::UnreconcilableSnapshot,
            Self::CurveVirtualOffsetChanged,
        ]
        .into_iter()
        .find(|d| d.label() == s)
    }
    const fn code(self) -> &'static str {
        match self {
            Self::AdverseLiquidityChange => "adverse",
            Self::UnreconcilableSnapshot => "unreconcilable",
            Self::CurveVirtualOffsetChanged => "voffset",
        }
    }
    fn from_code(s: &str) -> Option<Self> {
        match s {
            "adverse" => Some(Self::AdverseLiquidityChange),
            "unreconcilable" => Some(Self::UnreconcilableSnapshot),
            "voffset" => Some(Self::CurveVirtualOffsetChanged),
            _ => None,
        }
    }
}

/// Result of applying one cumulative fill report to the shadow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The increment over the recorded cumulative was applied.
    Applied { tokens: u64, lamports: u64 },
    /// Equal to the recorded cumulative: nothing changed.
    Duplicate,
    /// Cumulative went backwards (or tokens/lamports disagree in direction): named fault, nothing changed.
    Backwards,
    /// The market's delta is no longer carried (diverged or venue ended); the fill is recorded for identity but
    /// does not enter the delta.
    NotCarried,
}

/// Per-market shadow: our cumulative net effect in the CURRENT venue segment, plus fill identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowMarket {
    pub venue: ShadowVenue,
    /// SOL our fills put INTO the reserves (buy net-in, venue fees excluded: fees leave to fee recipients).
    pub sol_in: u128,
    /// SOL our fills took OUT of the reserves (sell gross).
    pub sol_out: u128,
    /// Tokens our fills took OUT of the reserves (buys).
    pub tok_out: u128,
    /// Tokens our fills put INTO the reserves (sells).
    pub tok_in: u128,
    /// Fill identity -> cumulative (tokens, lamports) already applied. Survives restarts and segment changes.
    pub applied: BTreeMap<(LegKind, u64), (u64, u64)>,
    /// Set once the delta stopped being carried in this segment.
    pub diverged: Option<Divergence>,
    /// Curve invariants (`vsol - real_sol`, `vtok - real_tok`) of the base the delta was opened on.
    pub curve_offsets: Option<(u64, u64)>,
    /// Last pool constant product the delta was reconciled against.
    pub pool_k: Option<u128>,
}

impl ShadowMarket {
    fn new(venue: ShadowVenue) -> Self {
        Self {
            venue,
            sol_in: 0,
            sol_out: 0,
            tok_out: 0,
            tok_in: 0,
            applied: BTreeMap::new(),
            diverged: None,
            curve_offsets: None,
            pool_k: None,
        }
    }

    /// Our conserved net SOL contribution to the reserves (never negative for capacity purposes).
    #[must_use]
    pub fn conserved_sol(&self) -> u128 {
        self.sol_in.saturating_sub(self.sol_out)
    }

    /// Signed net SOL delta (in - out).
    fn dsol(&self) -> i128 {
        self.sol_in as i128 - self.sol_out as i128 // LINT-ALLOW(money_float_cast): integer widening
    }

    /// Signed net token delta (out of reserves - into reserves).
    fn dtok(&self) -> i128 {
        self.tok_out as i128 - self.tok_in as i128 // LINT-ALLOW(money_float_cast): integer widening
    }

    fn carried(&self) -> bool {
        self.diverged.is_none()
    }

    fn drop_delta(&mut self, why: Divergence) {
        self.sol_in = 0;
        self.sol_out = 0;
        self.tok_out = 0;
        self.tok_in = 0;
        self.diverged = Some(why);
    }
}

/// A shadow sell evaluation, always next to the external (observed-state) benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShadowSellView {
    /// Observed-state quote (exec_quote) — the external-liquidity benchmark. Always present.
    pub external: Result<SellQuote, QuoteRefusal>,
    /// Shadow quote (observed + our carried delta, capped by conserved capacity).
    pub shadow: Result<SellQuote, ShadowRefusal>,
    /// The cap applied: observed real SOL / quote vault + our conserved contribution.
    pub capacity_lamports: u128,
    /// Whether the delta was carried (false = diverged/none -> shadow == observed state).
    pub delta_carried: bool,
}

/// Why a shadow quote is unavailable (named).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowRefusal {
    /// The quote on shadow reserves was refused by the venue arithmetic.
    Quote(QuoteRefusal),
    /// The gross exceeds the conserved sale capacity.
    CapacityExceeded { capacity: u128 },
    /// Shadow reserves are not representable (would be negative / overflow).
    StateInvalid,
}

impl ShadowRefusal {
    /// Stable label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Quote(q) => q.label(),
            Self::CapacityExceeded { .. } => "quote_unavailable:shadow_capacity_exceeded",
            Self::StateInvalid => "quote_unavailable:shadow_state_invalid",
        }
    }
}

/// All markets' shadows. Persisted with the financial ledger.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShadowBook {
    pub markets: BTreeMap<[u8; 32], ShadowMarket>,
}

fn add_i(base: u64, d: i128) -> Option<u64> {
    u64::try_from(i128::from(base).checked_add(d)?).ok()
}

impl ShadowBook {
    /// The shadow of `mint`, if any.
    #[must_use]
    pub fn market(&self, mint: &[u8; 32]) -> Option<&ShadowMarket> {
        self.markets.get(mint)
    }

    /// Apply one CUMULATIVE fill report of ours. `tokens`/`lamports` are cumulative for the order: buys report
    /// net SOL that entered the reserves (venue fees excluded) and tokens delivered; sells report tokens sold and
    /// GROSS SOL taken out (fees are paid from gross to fee recipients, so the reserves lose the gross).
    ///
    /// `basis` is the observed state the fill was priced on; the first carried fill of a segment records the
    /// reconciliation invariants from it (curve offsets / pool k), so the very next snapshot is already checked.
    pub fn apply_own_fill(
        &mut self,
        mint: [u8; 32],
        basis: FillBasis,
        leg: LegKind,
        order_id: u64,
        cum_tokens: u64,
        cum_lamports: u64,
    ) -> ApplyOutcome {
        let venue = basis.venue();
        let m = self
            .markets
            .entry(mint)
            .or_insert_with(|| ShadowMarket::new(venue));
        let key = (leg, order_id);
        let (pt, pl) = m.applied.get(&key).copied().unwrap_or((0, 0));
        if cum_tokens == pt && cum_lamports == pl {
            return ApplyOutcome::Duplicate;
        }
        if cum_tokens < pt || cum_lamports < pl {
            return ApplyOutcome::Backwards;
        }
        let (dt, dl) = (cum_tokens - pt, cum_lamports - pl);
        m.applied.insert(key, (cum_tokens, cum_lamports));
        if m.venue != venue || !m.carried() {
            // A fill on a venue this segment is not (or a diverged delta): identity recorded, not carried.
            return ApplyOutcome::NotCarried;
        }
        match basis {
            FillBasis::Curve(b) => {
                if m.curve_offsets.is_none() {
                    m.curve_offsets = b
                        .vsol
                        .checked_sub(b.real_sol)
                        .zip(b.vtok.checked_sub(b.real_tok));
                }
            }
            FillBasis::Pool(b) => {
                if m.pool_k.is_none() {
                    m.pool_k = Some(pool_k(&b));
                }
            }
        }
        if leg.is_buy() {
            m.sol_in += u128::from(dl);
            m.tok_out += u128::from(dt);
        } else {
            m.sol_out += u128::from(dl);
            m.tok_in += u128::from(dt);
        }
        ApplyOutcome::Applied {
            tokens: dt,
            lamports: dl,
        }
    }

    /// Reconcile a FRESH curve observation (it replaces the base; the record itself is not touched). Returns the
    /// divergence declared by THIS observation, if any.
    pub fn on_curve_observation(&mut self, mint: &[u8; 32], obs: &CurveBase) -> Option<Divergence> {
        let m = self.markets.get_mut(mint)?;
        if m.venue != ShadowVenue::Curve || !m.carried() {
            return None;
        }
        let offs = (
            obs.vsol.checked_sub(obs.real_sol)?,
            obs.vtok.checked_sub(obs.real_tok)?,
        );
        if obs.vtok == 0 || m.curve_offsets.is_some_and(|o| o.1 != offs.1) {
            m.drop_delta(Divergence::AdverseLiquidityChange);
            return m.diverged;
        }
        if m.curve_offsets.is_some_and(|o| o.0 != offs.0) {
            m.drop_delta(Divergence::CurveVirtualOffsetChanged);
            return m.diverged;
        }
        m.curve_offsets.get_or_insert(offs);
        let sol_ok = i128::from(obs.real_sol) + m.dsol() >= 0;
        let tok_ok = i128::from(obs.real_tok) - m.dtok() >= 0;
        if !(sol_ok && tok_ok) {
            m.drop_delta(Divergence::UnreconcilableSnapshot);
            return m.diverged;
        }
        None
    }

    /// Reconcile a FRESH pool observation.
    pub fn on_pool_observation(&mut self, mint: &[u8; 32], obs: &PoolBase) -> Option<Divergence> {
        let m = self.markets.get_mut(mint)?;
        if m.venue != ShadowVenue::Pool || !m.carried() {
            return None;
        }
        let k = pool_k(obs);
        if m.pool_k.is_some_and(|k0| k < k0) {
            m.drop_delta(Divergence::AdverseLiquidityChange);
            return m.diverged;
        }
        m.pool_k = Some(k);
        let sol_ok = i128::from(obs.quote) + m.dsol() >= 0;
        let tok_ok = i128::from(obs.base) - m.dtok() >= 0;
        if !(sol_ok && tok_ok) {
            m.drop_delta(Divergence::UnreconcilableSnapshot);
            return m.diverged;
        }
        None
    }

    /// Graduation: the curve shadow ENDS; a pool segment starts with an empty delta from the observed pool state.
    /// Fill identities are kept (a late duplicate report of a curve fill stays a duplicate). Returns whether a
    /// curve delta was dropped.
    pub fn on_graduation(&mut self, mint: &[u8; 32]) -> bool {
        let Some(m) = self.markets.get_mut(mint) else {
            return false;
        };
        if m.venue == ShadowVenue::Pool {
            return false;
        }
        let had = m.sol_in + m.sol_out + m.tok_in + m.tok_out > 0;
        let applied = std::mem::take(&mut m.applied);
        *m = ShadowMarket::new(ShadowVenue::Pool);
        m.applied = applied;
        had
    }

    /// Forget a market (position fully closed and no pending orders).
    pub fn forget(&mut self, mint: &[u8; 32]) {
        self.markets.remove(mint);
    }

    /// The curve reserves OUR next fill is priced on (`observed + carried delta`), or `None` when not
    /// representable. With no carried delta this IS the observed state.
    #[must_use]
    pub fn shadow_curve(&self, mint: &[u8; 32], obs: &CurveBase) -> Option<(CurveBase, u128)> {
        let (dsol, dtok, conserved) = match self.markets.get(mint) {
            Some(m) if m.venue == ShadowVenue::Curve && m.carried() => {
                (m.dsol(), m.dtok(), m.conserved_sol())
            }
            _ => (0, 0, 0),
        };
        let s = CurveBase {
            vsol: add_i(obs.vsol, dsol)?,
            vtok: add_i(obs.vtok, -dtok)?,
            real_sol: add_i(obs.real_sol, dsol)?,
            real_tok: add_i(obs.real_tok, -dtok)?,
            slot: obs.slot,
        };
        Some((s, u128::from(obs.real_sol) + conserved))
    }

    /// Pool reserves for our next fill (`observed + carried delta`) and the conserved capacity.
    #[must_use]
    pub fn shadow_pool(&self, mint: &[u8; 32], obs: &PoolBase) -> Option<(PoolBase, u128)> {
        let (dsol, dtok, conserved) = match self.markets.get(mint) {
            Some(m) if m.venue == ShadowVenue::Pool && m.carried() => {
                (m.dsol(), m.dtok(), m.conserved_sol())
            }
            _ => (0, 0, 0),
        };
        let s = PoolBase {
            base: add_i(obs.base, -dtok)?,
            quote: add_i(obs.quote, dsol)?,
            vq: obs.vq,
            slot: obs.slot,
        };
        Some((s, u128::from(obs.quote) + conserved))
    }

    /// Curve SELL of `tokens`: the external benchmark AND the shadow quote, separately.
    #[must_use]
    pub fn curve_sell_view(&self, mint: &[u8; 32], obs: &CurveBase, tokens: u64) -> ShadowSellView {
        let external = crate::exec_quote::curve_sell(obs.vsol, obs.vtok, obs.real_sol, tokens);
        let carried = self
            .markets
            .get(mint)
            .is_some_and(|m| m.venue == ShadowVenue::Curve && m.carried());
        let Some((s, cap)) = self.shadow_curve(mint, obs) else {
            return ShadowSellView {
                external,
                shadow: Err(ShadowRefusal::StateInvalid),
                capacity_lamports: 0,
                delta_carried: carried,
            };
        };
        let shadow = match crate::exec_quote::curve_sell(s.vsol, s.vtok, s.real_sol, tokens) {
            Ok(q) if u128::from(q.gross) > cap => {
                Err(ShadowRefusal::CapacityExceeded { capacity: cap })
            }
            Ok(q) => Ok(q),
            Err(r) => Err(ShadowRefusal::Quote(r)),
        };
        ShadowSellView {
            external,
            shadow,
            capacity_lamports: cap,
            delta_carried: carried,
        }
    }

    /// Pool SELL of `tokens`: external benchmark and shadow quote.
    #[must_use]
    pub fn pool_sell_view(
        &self,
        mint: &[u8; 32],
        obs: &PoolBase,
        parts: Option<(u32, u32, u32)>,
        cashback: CashbackField,
        tokens: u64,
    ) -> ShadowSellView {
        let external =
            crate::exec_quote::amm_sell(obs.base, obs.quote, Some(obs.vq), parts, cashback, tokens);
        let carried = self
            .markets
            .get(mint)
            .is_some_and(|m| m.venue == ShadowVenue::Pool && m.carried());
        let Some((s, cap)) = self.shadow_pool(mint, obs) else {
            return ShadowSellView {
                external,
                shadow: Err(ShadowRefusal::StateInvalid),
                capacity_lamports: 0,
                delta_carried: carried,
            };
        };
        let shadow =
            match crate::exec_quote::amm_sell(s.base, s.quote, Some(s.vq), parts, cashback, tokens)
            {
                Ok(q) if u128::from(q.gross) > cap => {
                    Err(ShadowRefusal::CapacityExceeded { capacity: cap })
                }
                Ok(q) => Ok(q),
                Err(r) => Err(ShadowRefusal::Quote(r)),
            };
        ShadowSellView {
            external,
            shadow,
            capacity_lamports: cap,
            delta_carried: carried,
        }
    }

    /// Durable form (persisted inside the held ledger under `shadow_pool`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mk: Vec<Value> = self
            .markets
            .iter()
            .map(|(mint, m)| {
                let applied: Vec<Value> = m
                    .applied
                    .iter()
                    .map(|((l, id), (t, s))| json!([l.code(), id, t, s]))
                    .collect();
                json!({
                    "mint": mint.iter().map(|x| format!("{x:02x}")).collect::<String>(),
                    "venue": if m.venue == ShadowVenue::Curve { "curve" } else { "pool" },
                    "sol_in": m.sol_in.to_string(),
                    "sol_out": m.sol_out.to_string(),
                    "tok_out": m.tok_out.to_string(),
                    "tok_in": m.tok_in.to_string(),
                    "applied": applied,
                    "diverged": m.diverged.map(Divergence::code),
                    "curve_offsets": m.curve_offsets.map(|(a, b)| json!([a, b])),
                    "pool_k": m.pool_k.map(|k| k.to_string()),
                })
            })
            .collect();
        json!({"version": PAPER_FILL_V2_SHADOW, "markets": mk})
    }

    /// Read the durable form. Any malformed field -> `Err` (the caller refuses the ledger; never a silent empty).
    ///
    /// # Errors
    /// Static reason.
    pub fn from_json(v: &Value) -> Result<Self, &'static str> {
        if v["version"].as_str() != Some(PAPER_FILL_V2_SHADOW) {
            return Err("shadow_pool.version");
        }
        let big = |x: &Value, k: &'static str| -> Result<u128, &'static str> {
            x[k].as_str().and_then(|s| s.parse::<u128>().ok()).ok_or(k)
        };
        let mut out = Self::default();
        for m in v["markets"].as_array().ok_or("shadow_pool.markets")? {
            let hexs = m["mint"].as_str().ok_or("shadow_pool.mint")?;
            if hexs.len() != 64 {
                return Err("shadow_pool.mint");
            }
            let mut mint = [0u8; 32];
            for (i, o) in mint.iter_mut().enumerate() {
                *o = u8::from_str_radix(&hexs[i * 2..i * 2 + 2], 16)
                    .map_err(|_| "shadow_pool.mint")?;
            }
            let venue = match m["venue"].as_str() {
                Some("curve") => ShadowVenue::Curve,
                Some("pool") => ShadowVenue::Pool,
                _ => return Err("shadow_pool.venue"),
            };
            let mut sm = ShadowMarket::new(venue);
            sm.sol_in = big(m, "sol_in")?;
            sm.sol_out = big(m, "sol_out")?;
            sm.tok_out = big(m, "tok_out")?;
            sm.tok_in = big(m, "tok_in")?;
            for a in m["applied"].as_array().ok_or("shadow_pool.applied")? {
                let l = a[0]
                    .as_str()
                    .and_then(LegKind::from_code)
                    .ok_or("shadow_pool.applied")?;
                let id = a[1].as_u64().ok_or("shadow_pool.applied")?;
                let t = a[2].as_u64().ok_or("shadow_pool.applied")?;
                let s = a[3].as_u64().ok_or("shadow_pool.applied")?;
                sm.applied.insert((l, id), (t, s));
            }
            sm.diverged = match &m["diverged"] {
                Value::Null => None,
                x => Some(
                    x.as_str()
                        .and_then(Divergence::from_code)
                        .ok_or("shadow_pool.diverged")?,
                ),
            };
            sm.curve_offsets = match &m["curve_offsets"] {
                Value::Null => None,
                x => Some((
                    x[0].as_u64().ok_or("shadow_pool.curve_offsets")?,
                    x[1].as_u64().ok_or("shadow_pool.curve_offsets")?,
                )),
            };
            sm.pool_k = match &m["pool_k"] {
                Value::Null => None,
                x => Some(
                    x.as_str()
                        .and_then(|s| s.parse().ok())
                        .ok_or("shadow_pool.pool_k")?,
                ),
            };
            out.markets.insert(mint, sm);
        }
        Ok(out)
    }
}

/// LABELLED ESTIMATE (not observed truth): network fee per landed leg, lamports. Measured p50 on the captured
/// tape (p90 45,000; p99 1,005,000). Used only for the net liquidation ESTIMATE and paper settlement.
pub const NETWORK_FEE_PER_LANDED_LEG_ESTIMATE: u64 = 10_000;

/// Latest observed venue state for a held mint, as handed to the engine hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedVenue {
    /// Curve state.
    Curve(CurveBase),
    /// Pool state + the landing event's fee parts + its cashback field (unknown cashback refuses, never zero).
    Pool(PoolBase, Option<(u32, u32, u32)>, CashbackField),
}

impl ShadowBook {
    /// ENGINE HOOK 1 (stop table): the named shadow divergence of `mint`, if its delta stopped being carried.
    #[must_use]
    pub fn divergence(&self, mint: &[u8; 32]) -> Option<Divergence> {
        self.markets.get(mint).and_then(|m| m.diverged)
    }

    /// ENGINE HOOK 2 (stop table): shadow NET liquidation estimate of selling `tokens` now, lamports, signed
    /// (venue-net proceeds minus [`NETWORK_FEE_PER_LANDED_LEG_ESTIMATE`]; may be negative for dust).
    /// `None` = UNKNOWN: diverged shadow, no observation, venue mismatch, or the shadow quote is refused.
    /// Never zero and never the cost basis as a stand-in.
    #[must_use]
    pub fn net_liquidation_estimate(
        &self,
        mint: &[u8; 32],
        obs: Option<&ObservedVenue>,
        tokens: u64,
    ) -> Option<i128> {
        if self.divergence(mint).is_some() {
            return None;
        }
        let view = match obs? {
            ObservedVenue::Curve(c) => {
                if self
                    .markets
                    .get(mint)
                    .is_some_and(|m| m.venue != ShadowVenue::Curve)
                {
                    return None;
                }
                self.curve_sell_view(mint, c, tokens)
            }
            ObservedVenue::Pool(p, parts, cashback) => {
                if self
                    .markets
                    .get(mint)
                    .is_some_and(|m| m.venue != ShadowVenue::Pool)
                {
                    return None;
                }
                self.pool_sell_view(mint, p, *parts, *cashback, tokens)
            }
        };
        let q = view.shadow.ok()?;
        Some(i128::from(q.net) - i128::from(NETWORK_FEE_PER_LANDED_LEG_ESTIMATE))
    }
}
