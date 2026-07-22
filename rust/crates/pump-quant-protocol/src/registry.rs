//! Versioned protocol-registry identifiers.
//!
//! # Responsibility
//! Map a trading [`Venue`] to a `(version, hash)` pair used by the supervisor
//! to pin which decoder/builder revision a strategy was compiled against. The
//! hash is a **deterministic placeholder** derived purely from the venue's
//! canonical program-id string — it is not a cryptographic digest, but it is
//! stable across processes and machines so that a version mismatch is
//! detectable without any network call.
//!
//! # Constitution
//! * Deterministic — identical `venue` always yields identical output; no RNG,
//!   clock, or network involved.
//! * §22 — integer-only.

/// A supported trading venue whose data layout this crate can handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Venue {
    /// pump.fun bonding-curve venue.
    PumpFun,
    /// PumpSwap constant-product AMM venue.
    PumpSwap,
}

impl Venue {
    /// Canonical on-chain program id for this venue (base58 string).
    pub const fn program_id(self) -> &'static str {
        match self {
            Venue::PumpFun => "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
            Venue::PumpSwap => "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
        }
    }

    /// Registry schema version for this venue's decoders/builders.
    pub const fn version(self) -> u16 {
        match self {
            Venue::PumpFun => 1,
            Venue::PumpSwap => 1,
        }
    }
}

/// Return the versioned protocol-registry id and hash placeholder for `venue`.
///
/// The 32-byte hash is produced by a small deterministic FNV-1a-style fold over
/// the venue's program-id bytes, expanded to fill the array. It is a stable
/// placeholder — swap for a real digest when the registry is finalized.
///
/// # Constitution
/// Deterministic and integer-only; no floats, RNG, clock, or I/O.
pub fn registry_version(venue: Venue) -> (u16, [u8; 32]) {
    (
        venue.version(),
        placeholder_hash(venue.program_id().as_bytes()),
    )
}

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Deterministically expand `seed` bytes into a stable 32-byte placeholder.
///
/// Each output byte is drawn from a running FNV-1a hash that is re-mixed with
/// the byte index, giving good diffusion while remaining fully reproducible.
fn placeholder_hash(seed: &[u8]) -> [u8; 32] {
    let mut base = FNV_OFFSET;
    for &b in seed {
        base ^= b as u64;
        base = base.wrapping_mul(FNV_PRIME);
    }

    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let mut h = base ^ (i as u64).wrapping_mul(FNV_PRIME);
        h = h.wrapping_mul(FNV_PRIME);
        *slot = (h >> ((i % 8) * 8)) as u8;
    }
    out
}

// ---------------------------------------------------------------------------
// §18.2 — version-controlled protocol registry.
//
// Each supported venue is described by a single, immutable [`RegistryEntry`]
// carrying every fact §18.2 mandates: program id, platform/config PDA,
// effective slot range, account-layout version, instruction & account
// discriminators, fee model, curve model, migration target, quote-mint
// behavior, upgrade authority, a golden account fixture, the decoder version,
// and the last-verified slot. The slot fields are **offline-curated recorded
// metadata** — seeded with known historical values and bumped only when a
// human re-verifies against chain; they are never live-computed. A
// deterministic [`RegistryEntry::content_digest`] folds every field into a
// 32-byte tag so a mismatch between a compiled-in entry and an expected pin is
// detectable without any network call.
// ---------------------------------------------------------------------------

/// How trading fees are levied on a venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeKind {
    /// Flat basis-point fee taken by a pump.fun-style bonding curve.
    FixedBondingCurve,
    /// Flat basis-point fee taken by a constant-product AMM swap.
    ConstantProductAmm,
}

impl FeeKind {
    /// Stable single-byte tag used in the content digest.
    const fn tag(self) -> u8 {
        match self {
            FeeKind::FixedBondingCurve => 1,
            FeeKind::ConstantProductAmm => 2,
        }
    }
}

/// Recorded fee model for a venue: a `kind` plus its basis-point rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeModel {
    /// The mechanism by which the fee is applied.
    pub kind: FeeKind,
    /// Fee rate in basis points (1 bp = 0.01%).
    pub fee_bps: u32,
}

/// The price/output curve a venue's math follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveModel {
    /// pump.fun "virtual" constant-product bonding curve.
    VirtualConstantProduct,
    /// Plain `x * y = k` constant-product AMM.
    ConstantProductAmm,
}

impl CurveModel {
    /// Stable single-byte tag used in the content digest.
    const fn tag(self) -> u8 {
        match self {
            CurveModel::VirtualConstantProduct => 1,
            CurveModel::ConstantProductAmm => 2,
        }
    }
}

/// A single version-controlled protocol-registry entry (§18.2).
///
/// Every field is a compiled-in constant or a curated recorded fact. Nothing
/// here is derived from a clock, RNG, or network read — the struct is the
/// authoritative in-repo description a decoder verifies identity against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryEntry {
    /// Which venue this entry describes.
    pub venue: Venue,
    /// Canonical on-chain program id (base58).
    pub program_id: &'static str,
    /// Platform / global-config PDA owned by the program (base58).
    pub config_pda: &'static str,
    /// First slot at which this entry's layout is known to be effective.
    pub effective_slot_start: u64,
    /// Last effective slot, or `None` when the entry is still current.
    pub effective_slot_end: Option<u64>,
    /// Account-layout schema version this decoder targets.
    pub layout_version: u16,
    /// `global:buy` instruction discriminator.
    pub buy_discriminator: [u8; 8],
    /// `global:sell` instruction discriminator.
    pub sell_discriminator: [u8; 8],
    /// Anchor account discriminator (`sha256("account:<Name>")[..8]`) of the
    /// venue's primary decoded account (`BondingCurve` / `Pool`).
    pub account_discriminator: [u8; 8],
    /// Recorded fee model.
    pub fee_model: FeeModel,
    /// Recorded curve model.
    pub curve_model: CurveModel,
    /// Program id this venue's positions migrate to, or `None` when terminal.
    pub migration_target: Option<&'static str>,
    /// Canonical quote-mint (base58); pump.fun/PumpSwap both quote in wSOL.
    pub quote_mint: &'static str,
    /// Recorded on-chain upgrade authority (base58).
    pub upgrade_authority: &'static str,
    /// Stored golden account fixture that must decode cleanly.
    pub golden_fixture: &'static [u8],
    /// Version of the decoder code this entry was validated against.
    pub decoder_version: u16,
    /// Last slot at which a human re-verified this entry against chain.
    pub last_verified_slot: u64,
}

/// Canonical wrapped-SOL mint, the quote asset for both supported venues.
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// PumpFun `BondingCurve` account discriminator (`sha256("account:BondingCurve")[..8]`).
pub const PUMPFUN_ACCOUNT_DISCRIMINATOR: [u8; 8] = [23, 183, 248, 55, 96, 216, 172, 96];

/// PumpSwap `Pool` account discriminator (`sha256("account:Pool")[..8]`).
pub const PUMPSWAP_ACCOUNT_DISCRIMINATOR: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];

/// Golden `BondingCurve` account fixture (49 bytes) — decodes cleanly.
const PUMPFUN_GOLDEN: [u8; 49] = [
    23, 183, 248, 55, 96, 216, 172, 96, 0, 0, 51, 115, 250, 206, 3, 0, 0, 172, 35, 252, 6, 0, 0, 0,
    0, 120, 197, 251, 81, 209, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 198, 164, 126, 141, 3, 0, 0,
];

/// Golden `Pool` account fixture (35 bytes) — decodes cleanly.
const PUMPSWAP_GOLDEN: [u8; 35] = [
    241, 154, 109, 4, 17, 177, 109, 188, 255, 0, 0, 0, 16, 165, 212, 232, 0, 0, 0, 0, 144, 47, 80,
    9, 0, 0, 0, 0, 194, 235, 11, 0, 0, 0, 0,
];

/// The version-controlled entry for the pump.fun bonding-curve venue.
const PUMPFUN_ENTRY: RegistryEntry = RegistryEntry {
    venue: Venue::PumpFun,
    program_id: "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
    config_pda: "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf",
    effective_slot_start: 254_000_000,
    effective_slot_end: None,
    layout_version: 1,
    buy_discriminator: crate::ix::BUY_DISCRIMINATOR,
    sell_discriminator: crate::ix::SELL_DISCRIMINATOR,
    account_discriminator: PUMPFUN_ACCOUNT_DISCRIMINATOR,
    fee_model: FeeModel {
        kind: FeeKind::FixedBondingCurve,
        fee_bps: 100,
    },
    curve_model: CurveModel::VirtualConstantProduct,
    migration_target: Some("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"),
    quote_mint: WSOL_MINT,
    upgrade_authority: "BCdaMs8j2rMcpqU55CBAe8FBQqTNTNPQkhnErvS1P5XY",
    golden_fixture: &PUMPFUN_GOLDEN,
    decoder_version: 1,
    last_verified_slot: 305_000_000,
};

/// The version-controlled entry for the PumpSwap constant-product AMM venue.
const PUMPSWAP_ENTRY: RegistryEntry = RegistryEntry {
    venue: Venue::PumpSwap,
    program_id: "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
    config_pda: "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw",
    effective_slot_start: 291_000_000,
    effective_slot_end: None,
    layout_version: 1,
    buy_discriminator: crate::ix::BUY_DISCRIMINATOR,
    sell_discriminator: crate::ix::SELL_DISCRIMINATOR,
    account_discriminator: PUMPSWAP_ACCOUNT_DISCRIMINATOR,
    fee_model: FeeModel {
        kind: FeeKind::ConstantProductAmm,
        fee_bps: 25,
    },
    curve_model: CurveModel::ConstantProductAmm,
    migration_target: None,
    quote_mint: WSOL_MINT,
    upgrade_authority: "BCdaMs8j2rMcpqU55CBAe8FBQqTNTNPQkhnErvS1P5XY",
    golden_fixture: &PUMPSWAP_GOLDEN,
    decoder_version: 1,
    last_verified_slot: 305_000_000,
};

impl RegistryEntry {
    /// Deterministic 32-byte content digest over every field of this entry.
    ///
    /// Folds each field's bytes through the same FNV-1a mixer used elsewhere in
    /// the crate. Any change to any recorded fact — a bumped slot, an edited
    /// discriminator, a swapped migration target — changes the digest, so a
    /// caller can pin an expected value and detect drift with a byte compare.
    ///
    /// # Constitution
    /// §22 — integer-only, deterministic; no floats, RNG, clock, or I/O.
    pub fn content_digest(&self) -> [u8; 32] {
        let mut acc = FNV_OFFSET;
        let mut mix = |bytes: &[u8]| {
            for &b in bytes {
                acc ^= b as u64;
                acc = acc.wrapping_mul(FNV_PRIME);
            }
        };
        mix(&[venue_tag(self.venue)]);
        mix(self.program_id.as_bytes());
        mix(&[0xff]); // field separator
        mix(self.config_pda.as_bytes());
        mix(&[0xff]);
        mix(&self.effective_slot_start.to_le_bytes());
        mix(&slot_opt_bytes(self.effective_slot_end));
        mix(&self.layout_version.to_le_bytes());
        mix(&self.buy_discriminator);
        mix(&self.sell_discriminator);
        mix(&self.account_discriminator);
        mix(&[self.fee_model.kind.tag()]);
        mix(&self.fee_model.fee_bps.to_le_bytes());
        mix(&[self.curve_model.tag()]);
        match self.migration_target {
            Some(t) => {
                mix(&[1]);
                mix(t.as_bytes());
            }
            None => mix(&[0]),
        }
        mix(&[0xff]);
        mix(self.quote_mint.as_bytes());
        mix(&[0xff]);
        mix(self.upgrade_authority.as_bytes());
        mix(&[0xff]);
        mix(self.golden_fixture);
        mix(&self.decoder_version.to_le_bytes());
        mix(&self.last_verified_slot.to_le_bytes());

        let base = acc;
        let mut out = [0u8; 32];
        for (i, slot) in out.iter_mut().enumerate() {
            let mut h = base ^ (i as u64).wrapping_mul(FNV_PRIME);
            h = h.wrapping_mul(FNV_PRIME);
            *slot = (h >> ((i % 8) * 8)) as u8;
        }
        out
    }

    /// `true` if `slot` falls within this entry's effective slot range.
    ///
    /// An open-ended entry (`effective_slot_end == None`) is effective for
    /// every slot at or after [`Self::effective_slot_start`].
    pub const fn is_effective_at(&self, slot: u64) -> bool {
        if slot < self.effective_slot_start {
            return false;
        }
        match self.effective_slot_end {
            Some(end) => slot <= end,
            None => true,
        }
    }
}

/// Stable single-byte tag for a venue, used in the content digest.
const fn venue_tag(venue: Venue) -> u8 {
    match venue {
        Venue::PumpFun => 1,
        Venue::PumpSwap => 2,
    }
}

/// Serialize an optional slot as a 1-byte presence flag plus 8-byte value.
const fn slot_opt_bytes(slot: Option<u64>) -> [u8; 9] {
    let mut out = [0u8; 9];
    match slot {
        Some(v) => {
            out[0] = 1;
            let b = v.to_le_bytes();
            let mut i = 0;
            while i < 8 {
                out[i + 1] = b[i];
                i += 1;
            }
        }
        None => out[0] = 0,
    }
    out
}

/// Return the version-controlled [`RegistryEntry`] for `venue`.
///
/// # Constitution
/// Deterministic and integer-only; no floats, RNG, clock, or I/O.
pub const fn entry(venue: Venue) -> &'static RegistryEntry {
    match venue {
        Venue::PumpFun => &PUMPFUN_ENTRY,
        Venue::PumpSwap => &PUMPSWAP_ENTRY,
    }
}

/// Return the expected account discriminator for `venue`'s primary account.
///
/// This is the value a decoder must find at byte offset `0..8` of a raw
/// account buffer before it will trust the remaining bytes (fail-closed
/// identity check, §18.2).
///
/// # Constitution
/// Deterministic and integer-only.
pub const fn account_discriminator(venue: Venue) -> [u8; 8] {
    entry(venue).account_discriminator
}
