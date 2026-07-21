//! # exit_ladder — the scalp exit execution family
//!
//! Crate `pump-quant-strategy`, import path `pump_quant_strategy::exit_ladder`.
//!
//! This module implements the exit side of the bot's lifecycle:
//!
//! * **Pre-armed exit templates** — the full sell message is serialized once, at
//!   entry, with placeholder bytes for the three values that are only known at the
//!   trigger instant (recent blockhash, sell amount, minimum-out). Their byte
//!   offsets are recorded at arm time and are *stable for the life of the template*.
//!   The trigger hot path is pure `patch-sign-send`: it overwrites those byte
//!   ranges in place, never re-serializes.
//! * **Per-market derived profit targets** — the take-profit level is derived from
//!   the *measured* round-trip cost floor plus a configured margin. There is no
//!   global TP constant path (defect #3): a market whose measured floor exceeds the
//!   available upside evidence is declared inadmissible instead of being handed a
//!   guaranteed net-loss target.
//! * **Whole-lifecycle peak protection** — a trailing reference armed from the
//!   moment of entry (defects #1/#2), monotone in the running peak, never a
//!   TP2-gated dead zone.
//! * **5-level sell escalation** — a pure state machine whose cooldown scales *down*
//!   with measured price-decay urgency (a faster collapse means a shorter wait), the
//!   single permitted dynamic safety constant.
//! * **Cost-priced partial-exit ladders** — rung sizing that prices each rung
//!   against the full fixed cost it must pay, not impact alone (criterion 112).
//! * **Exit-into-strength trigger** — a pure detector that fires the pre-armed exit
//!   into an authentic buy-side burst climax while in profit.
//!
//! ## Constitution
//! §22: no `f32`/`f64` anywhere in outcome-controlling logic. All arithmetic here is
//! integer / fixed-point. Overflow is always explicit (checked / saturating).

#![forbid(unsafe_code)]

use core::fmt;
use core::ops::{Deref, DerefMut};

// ===========================================================================
// Fixed-capacity vector (no heap allocation)
// ===========================================================================

/// Error returned when a fixed-capacity [`ArrayVec`] would overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityError;

/// A minimal, self-contained, heap-free vector with a compile-time capacity `N`.
///
/// The exit templates and rung ladders must never allocate on the hot path, so
/// every buffer in this module is an `ArrayVec`. It derefs to a slice, so all the
/// usual slice operations (`iter`, indexing, `copy_from_slice`, `len`) are
/// available; only the first `len` elements are ever exposed.
#[derive(Clone)]
pub struct ArrayVec<T, const N: usize> {
    buf: [T; N],
    len: usize,
}

impl<T: Copy + Default, const N: usize> ArrayVec<T, N> {
    /// A new, empty buffer of capacity `N`.
    pub fn new() -> Self {
        Self { buf: [T::default(); N], len: 0 }
    }

    /// Number of live elements.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when there are no live elements.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Compile-time capacity.
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Append `v`, panicking on overflow. Used only where the caller has already
    /// proven the count is within capacity.
    pub fn push(&mut self, v: T) {
        assert!(self.len < N, "ArrayVec capacity {N} exceeded");
        self.buf[self.len] = v;
        self.len += 1;
    }

    /// Fallible append: `Err(CapacityError)` instead of a panic on overflow.
    pub fn try_push(&mut self, v: T) -> Result<(), CapacityError> {
        if self.len >= N {
            return Err(CapacityError);
        }
        self.buf[self.len] = v;
        self.len += 1;
        Ok(())
    }

    /// Append every element of `s`, or `Err(CapacityError)` if it would not fit
    /// (the buffer is left unchanged in that case).
    pub fn try_extend_from_slice(&mut self, s: &[T]) -> Result<(), CapacityError> {
        let end = self.len.checked_add(s.len()).ok_or(CapacityError)?;
        if end > N {
            return Err(CapacityError);
        }
        self.buf[self.len..end].copy_from_slice(s);
        self.len = end;
        Ok(())
    }

    /// View the live elements as a slice.
    pub fn as_slice(&self) -> &[T] {
        &self.buf[..self.len]
    }

    /// View the live elements as a mutable slice.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.buf[..self.len]
    }
}

impl<T: Copy + Default, const N: usize> Default for ArrayVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Deref for ArrayVec<T, N> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.buf[..self.len]
    }
}

impl<T, const N: usize> DerefMut for ArrayVec<T, N> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.buf[..self.len]
    }
}

impl<T: PartialEq, const N: usize> PartialEq for ArrayVec<T, N> {
    fn eq(&self, other: &Self) -> bool {
        self.deref() == other.deref()
    }
}

impl<T: Eq, const N: usize> Eq for ArrayVec<T, N> {}

impl<T: fmt::Debug, const N: usize> fmt::Debug for ArrayVec<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.deref().iter()).finish()
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a ArrayVec<T, N> {
    type Item = &'a T;
    type IntoIter = core::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.deref().iter()
    }
}

// ===========================================================================
// Small integer helpers (no floating point anywhere)
// ===========================================================================

/// Ceil division for unsigned integers. `div` must be non-zero.
#[inline]
fn ceil_div(num: u64, div: u64) -> u64 {
    debug_assert!(div != 0);
    if num == 0 {
        0
    } else {
        (num - 1) / div + 1
    }
}

/// `price * bps / 10_000`, floored, computed in `u128` so it cannot overflow for
/// any `u64` price. Used for fixed-point bps scaling of prices.
#[inline]
fn scale_bps_down(price: u64, bps: u32) -> u64 {
    let scaled = (price as u128) * (bps as u128) / 10_000u128;
    // A `bps <= 10_000` factor can never exceed the original price, but clamp for
    // total safety against callers passing bps > 10_000.
    if scaled > u64::MAX as u128 {
        u64::MAX
    } else {
        scaled as u64
    }
}

/// True when byte range `[a, a+al)` overlaps `[b, b+bl)`.
#[inline]
fn ranges_overlap(a: usize, al: usize, b: usize, bl: usize) -> bool {
    a < b.saturating_add(bl) && b < a.saturating_add(al)
}

// ===========================================================================
// Leaf: el_template_arm  — pre-armed exit skeleton
// ===========================================================================

/// Solana packet cap; the real sell message is always smaller, but the buffer is
/// sized to the transport maximum so serialization can never need the heap.
pub const MAX_MSG: usize = 1232;

/// 8-byte instruction discriminator for the sell instruction (placeholder value;
/// the offset discipline is what matters, not this exact tag).
const SELL_DISCRIMINATOR: [u8; 8] = [0xB7, 0x12, 0x46, 0x9C, 0x94, 0x3D, 0xA2, 0x14];

/// Accounts required to build a sell message. All are resolved at entry (arm time)
/// so that nothing is resolved on the sell hot path (pre-armed law).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitAccounts {
    /// Signing wallet pubkey.
    pub wallet: [u8; 32],
    /// Token mint being sold.
    pub mint: [u8; 32],
    /// Wallet's associated token account for `mint`.
    pub token_account: [u8; 32],
    /// The venue program the sell instruction targets.
    pub program: [u8; 32],
}

impl ExitAccounts {
    /// Deterministic fixture used by the property tests.
    pub fn test() -> Self {
        Self {
            wallet: [1u8; 32],
            mint: [2u8; 32],
            token_account: [3u8; 32],
            program: [4u8; 32],
        }
    }
}

/// Parameters baked into the pre-armed message at arm time. `amount` / `min_out`
/// are placeholders here; their live values are patched in at trigger time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitParams {
    /// Placeholder sell amount (patched at trigger).
    pub amount: u64,
    /// Placeholder minimum acceptable out (patched at trigger).
    pub min_out: u64,
    /// Slippage tolerance in bps, fixed at arm time.
    pub slippage_bps: u32,
}

impl ExitParams {
    /// Deterministic fixture used by the property tests.
    pub fn test() -> Self {
        Self { amount: 1_000_000, min_out: 900_000, slippage_bps: 300 }
    }
}

/// A pre-armed, byte-level exit transaction skeleton.
///
/// `msg_bytes` is the fully serialized sell message with placeholder bytes at three
/// recorded offsets. The offsets are computed once, here, and remain valid for the
/// whole life of the template — patching never re-serializes and never changes the
/// length or any unpatched byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitTemplate {
    /// Serialized sell message (fixed-capacity, no heap).
    pub msg_bytes: ArrayVec<u8, MAX_MSG>,
    /// Offset of the 32-byte recent-blockhash field.
    pub blockhash_off: usize,
    /// Offset of the 8-byte little-endian amount field.
    pub amount_off: usize,
    /// Offset of the 8-byte little-endian min-out field.
    pub min_out_off: usize,
}

/// Failure modes of [`arm_exit_template`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmError {
    /// The message did not fit in the fixed-capacity buffer.
    Serialize,
    /// A recorded field offset ran past the end of the message.
    OffsetOutOfBounds,
    /// Two patchable fields overlapped.
    Overlap,
}

/// Build a pre-armed exit skeleton at entry.
///
/// Serializes the full sell message once, writing placeholder bytes for the three
/// trigger-time fields and recording their offsets. The offsets are then verified
/// in-bounds and non-overlapping before the template is returned. No heap
/// allocation and no account resolution happens after this point.
pub fn arm_exit_template(
    accounts: &ExitAccounts,
    params: &ExitParams,
) -> Result<ExitTemplate, ArmError> {
    let mut msg = ArrayVec::<u8, MAX_MSG>::new();

    // Header: instruction discriminator.
    msg.try_extend_from_slice(&SELL_DISCRIMINATOR).map_err(|_| ArmError::Serialize)?;

    // Account keys (all resolved now, at arm time).
    msg.try_extend_from_slice(&accounts.program).map_err(|_| ArmError::Serialize)?;
    msg.try_extend_from_slice(&accounts.wallet).map_err(|_| ArmError::Serialize)?;
    msg.try_extend_from_slice(&accounts.mint).map_err(|_| ArmError::Serialize)?;
    msg.try_extend_from_slice(&accounts.token_account).map_err(|_| ArmError::Serialize)?;

    // Patchable field: amount (placeholder written now, offset recorded).
    let amount_off = msg.len();
    msg.try_extend_from_slice(&params.amount.to_le_bytes()).map_err(|_| ArmError::Serialize)?;

    // Patchable field: min_out (placeholder).
    let min_out_off = msg.len();
    msg.try_extend_from_slice(&params.min_out.to_le_bytes()).map_err(|_| ArmError::Serialize)?;

    // Fixed (non-patchable) slippage tolerance.
    msg.try_extend_from_slice(&params.slippage_bps.to_le_bytes()).map_err(|_| ArmError::Serialize)?;

    // Patchable field: recent blockhash (placeholder zeros).
    let blockhash_off = msg.len();
    msg.try_extend_from_slice(&[0u8; 32]).map_err(|_| ArmError::Serialize)?;

    // Validate offsets in-bounds.
    let fields = [(blockhash_off, 32usize), (amount_off, 8), (min_out_off, 8)];
    for &(off, len) in &fields {
        let end = off.checked_add(len).ok_or(ArmError::OffsetOutOfBounds)?;
        if end > msg.len() {
            return Err(ArmError::OffsetOutOfBounds);
        }
    }

    // Validate the three fields do not overlap.
    if ranges_overlap(blockhash_off, 32, amount_off, 8)
        || ranges_overlap(blockhash_off, 32, min_out_off, 8)
        || ranges_overlap(amount_off, 8, min_out_off, 8)
    {
        return Err(ArmError::Overlap);
    }

    Ok(ExitTemplate { msg_bytes: msg, blockhash_off, amount_off, min_out_off })
}

/// Overwrite the 8 little-endian bytes of a `u64` field at `off` in place.
///
/// A raw byte-patch helper used by the templates and tests. It changes exactly the
/// eight bytes of the field and never the message length. Out-of-range offsets are
/// a caller contract violation and are ignored (the template is left untouched).
pub fn patch_u64(t: &mut ExitTemplate, off: usize, value: u64) {
    let end = match off.checked_add(8) {
        Some(e) => e,
        None => return,
    };
    if end <= t.msg_bytes.len() {
        t.msg_bytes[off..end].copy_from_slice(&value.to_le_bytes());
    }
}

// ===========================================================================
// Leaf: el_patch_sign  — trigger-time in-place patch, the whole sell hot path
// ===========================================================================

/// Failure modes of [`patch_and_finalize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchError {
    /// A recorded field offset ran past the end of the message (never valid for a
    /// template produced by [`arm_exit_template`]).
    OutOfBounds,
}

/// Trigger-time patch of blockhash / amount / min_out, in place.
///
/// This is the entire sell hot path: no allocation, no serialization framework, no
/// syscalls — just three `copy_from_slice`s into pre-recorded byte ranges. Writing
/// the same values twice yields byte-identical output (idempotent).
pub fn patch_and_finalize(
    t: &mut ExitTemplate,
    blockhash: &[u8; 32],
    amount: u64,
    min_out: u64,
) -> Result<(), PatchError> {
    let len = t.msg_bytes.len();
    let bh_end = t.blockhash_off.checked_add(32).ok_or(PatchError::OutOfBounds)?;
    let amt_end = t.amount_off.checked_add(8).ok_or(PatchError::OutOfBounds)?;
    let mo_end = t.min_out_off.checked_add(8).ok_or(PatchError::OutOfBounds)?;
    if bh_end > len || amt_end > len || mo_end > len {
        return Err(PatchError::OutOfBounds);
    }

    t.msg_bytes[t.blockhash_off..bh_end].copy_from_slice(blockhash);
    t.msg_bytes[t.amount_off..amt_end].copy_from_slice(&amount.to_le_bytes());
    t.msg_bytes[t.min_out_off..mo_end].copy_from_slice(&min_out.to_le_bytes());
    Ok(())
}

// ===========================================================================
// Leaf: el_target_derive  — per-market derived profit target (defect #3 fix)
// ===========================================================================

/// Derive the per-market profit target in bps from the measured round-trip cost
/// floor plus a configured margin.
///
/// There is no global-constant path (defect #3): the target is always at least
/// `floor + margin`. Where conditional upside evidence (`mfe_p25_bps`, the 25th
/// percentile of measured favorable excursion for the archetype) is present and is
/// smaller than the target, the market is *inadmissible* — its own evidence says
/// the move cannot even pay the round-trip floor — and `None` is returned. A
/// checked add means arithmetic overflow also returns `None`.
pub fn derive_target_bps(
    floor_bps: u32,
    margin_bps: u32,
    mfe_p25_bps: Option<u32>,
) -> Option<u32> {
    let target = floor_bps.checked_add(margin_bps)?;
    match mfe_p25_bps {
        Some(mfe) if mfe < target => None,
        _ => Some(target),
    }
}

// ===========================================================================
// Leaf: el_peak_protect  — whole-lifecycle protection (defects #1/#2 fix)
// ===========================================================================

/// Compute the fixed-point protection (stop) price, armed from the moment of entry.
///
/// Protection is the *maximum* of two references:
/// * a trail from the true running peak: `peak * (10_000 - trail_bps) / 10_000`
/// * a hard stop from entry: `entry * (10_000 - hard_sl_bps) / 10_000`
///
/// Taking the max means protection exists from entry onward — there is no
/// TP2-gated dead zone (defects #1/#2) — and it is monotone in the peak: a higher
/// peak can never lower the returned level. bps math is integer and saturates at
/// zero for `trail_bps` / `hard_sl_bps` greater than `10_000`.
pub fn protection_level_fp(
    peak_price_fp: u64,
    entry_price_fp: u64,
    trail_bps: u32,
    hard_sl_bps: u32,
) -> u64 {
    let trail_factor = 10_000u32.saturating_sub(trail_bps);
    let sl_factor = 10_000u32.saturating_sub(hard_sl_bps);
    let trail_level = scale_bps_down(peak_price_fp, trail_factor);
    let sl_level = scale_bps_down(entry_price_fp, sl_factor);
    trail_level.max(sl_level)
}

// ===========================================================================
// Leaf: el_escalation  — 5-level sell escalation state machine
// ===========================================================================

/// Highest escalation level (levels are `0..=4`).
pub const MAX_ESCALATION_LEVEL: u8 = 4;
/// Cooldown floor: escalation never waits less than this, however urgent.
pub const MIN_COOLDOWN_MS: u32 = 5;
/// Upper bound on the urgency divisor, so a single extreme decay reading cannot
/// collapse the cooldown below the floor path in one step (retained static-by-design).
pub const MAX_URGENCY_DIV: u32 = 1_000;

/// State of the 5-level sell-escalation machine for one position.
///
/// `level` only ever rises (on a failed attempt), never falls mid-position;
/// `cooldown_ms` is recomputed each transition from measured decay urgency;
/// `emergency_path_required`, once set at the top level, stays set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscalationState {
    /// Current escalation level, `0..=MAX_ESCALATION_LEVEL`. Higher levels mean
    /// wider slippage tolerance and higher priority fee (encoded by the caller).
    pub level: u8,
    /// Cooldown before the next attempt, in ms — shorter under faster decay.
    pub cooldown_ms: u32,
    /// Set once the top level has failed: the caller must take the emergency path.
    pub emergency_path_required: bool,
}

impl EscalationState {
    /// Fresh state at entry: level 0, no emergency, cooldown seeded from the base.
    pub fn new(base_cooldown_ms: u32) -> Self {
        Self {
            level: 0,
            cooldown_ms: base_cooldown_ms.max(MIN_COOLDOWN_MS),
            emergency_path_required: false,
        }
    }
}

/// Slippage tolerance (bps) the caller should apply at a given escalation level.
/// Strictly increasing in level.
pub fn slippage_tolerance_bps(level: u8) -> u32 {
    // 50, 150, 350, 750, 1500 bps across levels 0..=4.
    match level.min(MAX_ESCALATION_LEVEL) {
        0 => 50,
        1 => 150,
        2 => 350,
        3 => 750,
        _ => 1_500,
    }
}

/// Priority fee multiplier (integer, x1) the caller should apply at a level.
/// Strictly increasing in level.
pub fn priority_fee_mult(level: u8) -> u32 {
    (level.min(MAX_ESCALATION_LEVEL) as u32) + 1
}

/// Advance the escalation state machine.
///
/// On a failed attempt the level rises by one (saturating at
/// [`MAX_ESCALATION_LEVEL`]); a success holds the level (escalation never reduces
/// aggressiveness mid-collapse). The cooldown is the base scaled *down* by measured
/// decay urgency — a faster collapse yields a strictly shorter wait — floored at
/// [`MIN_COOLDOWN_MS`]. Reaching the top level on a failure latches
/// `emergency_path_required`, which never clears. Pure and deterministic.
pub fn next_escalation(
    cur: EscalationState,
    attempt_failed: bool,
    decay_bps_per_s: u32,
    base_cooldown_ms: u32,
) -> EscalationState {
    // Urgency-scaled cooldown: faster measured decay => larger divisor => shorter
    // wait, bounded above by MAX_URGENCY_DIV and floored at MIN_COOLDOWN_MS.
    let urgency = decay_bps_per_s.max(1);
    let divisor = urgency.min(MAX_URGENCY_DIV).max(1);
    let cooldown_ms = (base_cooldown_ms / divisor).max(MIN_COOLDOWN_MS);

    let mut level = cur.level;
    let mut emergency = cur.emergency_path_required;

    if attempt_failed {
        level = level.saturating_add(1).min(MAX_ESCALATION_LEVEL);
        if level == MAX_ESCALATION_LEVEL {
            emergency = true;
        }
    }

    EscalationState { level, cooldown_ms, emergency_path_required: emergency }
}

// ===========================================================================
// Leaf: el_partial_ladder  — cost-priced partial-exit ladder (criterion 112)
// ===========================================================================

/// Maximum number of partial-exit rungs.
///
/// Sized so a rung ladder cannot proliferate past the point where each rung's full
/// fixed cost outweighs the marginal impact benefit of splitting (criterion 112).
pub const MAX_RUNGS: usize = 4;

/// A measured market-impact curve: given a clip size in lamports, the modeled price
/// impact in bps, and the inverse (largest clip within an impact budget).
///
/// The test fixture is linear — `impact_bps(size) = size / lamports_per_bps` — but
/// the ladder logic only relies on the two query methods, so a richer measured
/// curve can be substituted without touching [`ladder_rungs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactCurve {
    /// Lamports of clip size per 1 bps of modeled impact (linear model).
    lamports_per_bps: u64,
}

impl ImpactCurve {
    /// A linear test curve: `lamports_per_bps` lamports of size per bps of impact.
    pub fn linear_test(lamports_per_bps: u64) -> Self {
        Self { lamports_per_bps: lamports_per_bps.max(1) }
    }

    /// Modeled impact in bps for a clip of `size` lamports (floored).
    pub fn impact_bps(&self, size: u64) -> u32 {
        let bps = size / self.lamports_per_bps;
        if bps > u32::MAX as u64 {
            u32::MAX
        } else {
            bps as u32
        }
    }

    /// Largest clip size (lamports) whose modeled impact stays within `max_bps`.
    pub fn max_size_at(&self, max_bps: u32) -> u64 {
        self.lamports_per_bps.saturating_mul(max_bps as u64)
    }
}

/// Size a cost-priced partial-exit ladder.
///
/// Every rung pays the *full* fixed cost, while linear impact is only marginally
/// reduced by splitting, so a rung is only worth carving off if its share of the
/// position still clears that fixed cost with `min_rung_margin_bps` of margin
/// (criterion 112). Two constraints bound the rung count:
///
/// * **impact ceiling** — the largest impact-safe rung is `max_size_at(max_rung_impact_bps)`;
///   naively you would want `ceil(position / impact_cap)` rungs.
/// * **cost floor** — each rung must be at least `fixed_cost * 10_000 / margin_bps`
///   lamports, so the position supports at most `position / cost_floor` rungs.
///
/// The final count is the smaller of the two (and at most [`MAX_RUNGS`]). When the
/// cost floor is the binding constraint the rungs are larger than the impact budget
/// would like — that is the deliberate cost-priced trade, and the resulting rungs
/// still clear the fixed-cost floor. A position too small to carry two rungs above
/// that floor (or a zero-margin sentinel, which forbids free splitting) collapses to
/// a single clip. The rung sizes always sum to `position_lamports` exactly — no dust
/// is lost. Deterministic, no allocation.
pub fn ladder_rungs(
    position_lamports: u64,
    max_rung_impact_bps: u32,
    fixed_cost_lamports: u64,
    min_rung_margin_bps: u32,
    impact_curve: &ImpactCurve,
) -> ArrayVec<u64, MAX_RUNGS> {
    let mut rungs = ArrayVec::<u64, MAX_RUNGS>::new();
    if position_lamports == 0 {
        return rungs;
    }

    // (1) Impact ceiling: largest rung whose modeled impact stays within the bound.
    let impact_cap = impact_curve.max_size_at(max_rung_impact_bps).max(1);

    // (2) Cost floor: the smallest rung that pays its fixed cost with the required
    //     margin. A zero margin is a sentinel forbidding free splitting => u64::MAX,
    //     which forces a single clip.
    let cost_floor: u64 = if min_rung_margin_bps == 0 {
        u64::MAX
    } else {
        let cf = (fixed_cost_lamports as u128) * 10_000u128 / (min_rung_margin_bps as u128);
        if cf >= u64::MAX as u128 {
            u64::MAX
        } else {
            cf as u64
        }
    };

    // Rung count permitted by the cost floor (each rung must clear it).
    let cost_limited = if cost_floor == 0 {
        MAX_RUNGS as u64
    } else {
        (position_lamports / cost_floor).max(1)
    };
    // Rung count the impact ceiling would like.
    let impact_needed = ceil_div(position_lamports, impact_cap).max(1);

    let count = impact_needed
        .min(cost_limited)
        .min(MAX_RUNGS as u64)
        .max(1) as usize;

    // Even split with the remainder folded into the final rung: conserves the total
    // exactly and keeps every rung >= the base, hence >= the cost floor when split.
    let base = position_lamports / count as u64;
    let mut assigned = 0u64;
    for i in 0..count {
        let rung = if i + 1 == count { position_lamports - assigned } else { base };
        assigned += rung;
        rungs.push(rung);
    }
    rungs
}

// ===========================================================================
// Leaf: el_exit_into_strength  — burst-climax exit trigger
// ===========================================================================

/// Direction of a detected order-flow burst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Net buy-side pressure (favorable for exiting a long into strength).
    Buy,
    /// Net sell-side pressure.
    Sell,
}

/// Phase of a detected order-flow burst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstPhase {
    /// Burst is building.
    Onset,
    /// Burst has peaked — the exit-into-strength window.
    Climax,
    /// Burst is fading.
    Decay,
    /// No active burst.
    Quiet,
}

/// A detected order-flow burst: its phase, direction, and measured authenticity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurstState {
    /// Current burst phase.
    pub phase: BurstPhase,
    /// Net direction of the burst.
    pub direction: Dir,
}

impl BurstState {
    /// Deterministic fixture used by the property tests.
    pub fn test(phase: BurstPhase, direction: Dir) -> Self {
        Self { phase, direction }
    }
}

/// Decide whether to fire the pre-armed exit into detected buy-pressure.
///
/// Fires only on a buy-side [`BurstPhase::Climax`] while in profit and only when the
/// measured `authenticity_fp` clears `min_authenticity_fp` — fabricated bursts (the
/// product vendors sell) are exactly what this threshold rejects. Pure detector: it
/// never sizes the exit, only signals it.
pub fn exit_into_strength_fires(
    burst: &BurstState,
    in_profit: bool,
    authenticity_fp: u32,
    min_authenticity_fp: u32,
) -> bool {
    matches!(burst.phase, BurstPhase::Climax)
        && burst.direction == Dir::Buy
        && in_profit
        && authenticity_fp >= min_authenticity_fp
}
