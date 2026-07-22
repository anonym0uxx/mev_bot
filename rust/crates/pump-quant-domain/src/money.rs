//! Integer money and fixed-point rate types.
//!
//! ## Responsibility
//! The canonical unsigned lamport amount, its signed delta counterpart, the
//! token base-unit amount, and the basis-point rate used to price fees, floors,
//! and slippage. Every arithmetic helper here has **explicit** overflow
//! semantics — `checked_*` returns `Option`, `saturating_*` clamps at the type
//! bound — because silent wraparound in money math is a build defect.
//!
//! ## Constitution alignment
//! * **Section 22:** no floating point in outcome-controlling logic; money is
//!   lamports / token base units / basis points, integer only.
//! * **Section 57(a) hardcoded-parameter/perf law:** "all money/fixed-point
//!   arithmetic uses explicit checked/saturating/widening operations regardless
//!   of profile" — this module is that discipline in code form (rate application
//!   widens to `u128`, so `overflow-checks=false` cannot corrupt it).

use core::fmt;

/// An **unsigned** amount denominated in lamports (1 SOL = 1_000_000_000
/// lamports). Also used for any u64 base-unit quote amount when the quote mint
/// is SOL; USDC-quoted markets carry their own base-unit amount in the same
/// representation (quote-mint-parametric per Section 18.2).
///
/// Constitution Section 22: the canonical integer money type.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Lamports(pub u64);

impl Lamports {
    /// Zero lamports.
    pub const ZERO: Lamports = Lamports(0);
    /// The largest representable amount.
    pub const MAX: Lamports = Lamports(u64::MAX);

    /// Checked addition: `None` on overflow.
    #[inline]
    pub const fn checked_add(self, rhs: Lamports) -> Option<Lamports> {
        match self.0.checked_add(rhs.0) {
            Some(v) => Some(Lamports(v)),
            None => None,
        }
    }

    /// Checked subtraction: `None` if `rhs > self` (no underflow into wraparound).
    #[inline]
    pub const fn checked_sub(self, rhs: Lamports) -> Option<Lamports> {
        match self.0.checked_sub(rhs.0) {
            Some(v) => Some(Lamports(v)),
            None => None,
        }
    }

    /// Saturating addition: clamps at [`Lamports::MAX`].
    #[inline]
    pub const fn saturating_add(self, rhs: Lamports) -> Lamports {
        Lamports(self.0.saturating_add(rhs.0))
    }

    /// Saturating subtraction: clamps at zero.
    #[inline]
    pub const fn saturating_sub(self, rhs: Lamports) -> Lamports {
        Lamports(self.0.saturating_sub(rhs.0))
    }

    /// Checked multiplication by an integer scalar (e.g. per-attempt fixed cost
    /// times expected attempts). `None` on overflow.
    #[inline]
    pub const fn checked_mul(self, scalar: u64) -> Option<Lamports> {
        match self.0.checked_mul(scalar) {
            Some(v) => Some(Lamports(v)),
            None => None,
        }
    }

    /// Signed difference `self - other`, exact (both operands are `u64` and fit
    /// losslessly in `i128`). Used to express balance deltas and PnL.
    #[inline]
    pub const fn signed_diff(self, other: Lamports) -> SignedLamports {
        SignedLamports(self.0 as i128 - other.0 as i128)
    }

    /// Apply a basis-point rate, **saturating** at [`Lamports::MAX`], using a
    /// `u128` intermediate so the multiply cannot overflow before the divide.
    /// Truncates toward zero (floor for non-negative rate), the conservative
    /// direction when pricing a cost against the trader.
    #[inline]
    pub const fn apply_bps_saturating(self, rate: BasisPoints) -> Lamports {
        let product = (self.0 as u128) * (rate.0 as u128);
        let scaled = product / (BasisPoints::ONE_HUNDRED_PERCENT as u128);
        if scaled > u64::MAX as u128 {
            Lamports::MAX
        } else {
            Lamports(scaled as u64)
        }
    }

    /// Apply a basis-point rate with **checked** semantics: `None` only if the
    /// truncated result exceeds `u64` (the `u128` intermediate makes the multiply
    /// itself infallible for any `u64 × u32`).
    #[inline]
    pub const fn checked_apply_bps(self, rate: BasisPoints) -> Option<Lamports> {
        let product = (self.0 as u128) * (rate.0 as u128);
        let scaled = product / (BasisPoints::ONE_HUNDRED_PERCENT as u128);
        if scaled > u64::MAX as u128 {
            None
        } else {
            Some(Lamports(scaled as u64))
        }
    }
}

impl fmt::Display for Lamports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} lamports", self.0)
    }
}

/// A **signed** lamport delta, held in `i128` so that sums of many `u64`-scale
/// amounts (aggregate PnL, running balance deltas) cannot overflow in practice.
///
/// Constitution Section 22: signed money is still integer money.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct SignedLamports(pub i128);

impl SignedLamports {
    /// Zero.
    pub const ZERO: SignedLamports = SignedLamports(0);

    /// Checked addition: `None` on `i128` overflow (unreachable at realistic
    /// scale, but never silently wrapped).
    #[inline]
    pub const fn checked_add(self, rhs: SignedLamports) -> Option<SignedLamports> {
        match self.0.checked_add(rhs.0) {
            Some(v) => Some(SignedLamports(v)),
            None => None,
        }
    }

    /// Saturating addition at the `i128` bounds.
    #[inline]
    pub const fn saturating_add(self, rhs: SignedLamports) -> SignedLamports {
        SignedLamports(self.0.saturating_add(rhs.0))
    }

    /// Negation, saturating at [`i128::MAX`] for the [`i128::MIN`] edge case.
    #[inline]
    pub const fn saturating_neg(self) -> SignedLamports {
        if self.0 == i128::MIN {
            SignedLamports(i128::MAX)
        } else {
            SignedLamports(-self.0)
        }
    }

    /// `true` when this delta represents a loss (strictly negative).
    #[inline]
    pub const fn is_loss(self) -> bool {
        self.0 < 0
    }

    /// Absolute magnitude as unsigned [`Lamports`], saturating if it exceeds
    /// `u64` (so `i128::MIN` and very large deltas stay total).
    #[inline]
    pub const fn magnitude_saturating(self) -> Lamports {
        let m = self.0.unsigned_abs(); // u128
        if m > u64::MAX as u128 {
            Lamports::MAX
        } else {
            Lamports(m as u64)
        }
    }
}

impl fmt::Display for SignedLamports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:+} lamports", self.0)
    }
}

/// An amount denominated in a token's smallest base unit (respecting the mint's
/// decimals; the strategy core never carries fractional/`f64` token amounts).
/// Constitution Section 22 ("token base units").
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct TokenAmount(pub u64);

impl TokenAmount {
    /// Zero base units.
    pub const ZERO: TokenAmount = TokenAmount(0);

    /// Checked addition: `None` on overflow.
    #[inline]
    pub const fn checked_add(self, rhs: TokenAmount) -> Option<TokenAmount> {
        match self.0.checked_add(rhs.0) {
            Some(v) => Some(TokenAmount(v)),
            None => None,
        }
    }

    /// Saturating subtraction, clamped at zero.
    #[inline]
    pub const fn saturating_sub(self, rhs: TokenAmount) -> TokenAmount {
        TokenAmount(self.0.saturating_sub(rhs.0))
    }
}

/// A fixed-point rate in **basis points** (1 bp = 1/10_000). Fees, cost floors,
/// slippage bounds, and profit targets are expressed here rather than as `f64`
/// fractions. Held as `u32`, so rates well above 100% (e.g. a 3× move = 30_000
/// bps) are representable.
///
/// Constitution Section 22 (basis points, not floats) and Section 34.4 (economic
/// gate arithmetic).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct BasisPoints(pub u32);

impl BasisPoints {
    /// Zero rate.
    pub const ZERO: BasisPoints = BasisPoints(0);
    /// 100% expressed in basis points (the fixed-point scale denominator).
    pub const ONE_HUNDRED_PERCENT: u32 = 10_000;
    /// Convenience constant for exactly 100%.
    pub const FULL: BasisPoints = BasisPoints(Self::ONE_HUNDRED_PERCENT);

    /// Construct from a whole-percent value, saturating the `u32` (e.g.
    /// `from_percent(5)` == 500 bp). `checked` variant avoids surprise clamping.
    #[inline]
    pub const fn from_percent(percent: u32) -> BasisPoints {
        BasisPoints(percent.saturating_mul(100))
    }

    /// Checked addition of two rates (e.g. summing fee components): `None` on
    /// `u32` overflow.
    #[inline]
    pub const fn checked_add(self, rhs: BasisPoints) -> Option<BasisPoints> {
        match self.0.checked_add(rhs.0) {
            Some(v) => Some(BasisPoints(v)),
            None => None,
        }
    }

    /// Saturating addition of two rates.
    #[inline]
    pub const fn saturating_add(self, rhs: BasisPoints) -> BasisPoints {
        BasisPoints(self.0.saturating_add(rhs.0))
    }
}

impl fmt::Display for BasisPoints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}bp", self.0)
    }
}
