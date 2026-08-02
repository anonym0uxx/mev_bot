//! Program-derived-address (PDA) derivation from first principles.
//!
//! ## Responsibility
//! Implement Solana's `find_program_address` locally — SHA-256 over
//! `seeds ++ [bump] ++ program_id ++ b"ProgramDerivedAddress"`, rejecting any
//! candidate that decompresses to a valid ed25519 curve point — so every venue
//! account the transaction builder needs (`bonding_curve`, `creator_vault`,
//! `user_volume_accumulator`, `fee_config`, associated token accounts, …) is
//! **derived** at build time rather than accepted from a document or a model.
//!
//! This is `docs/VENUE_TX_LAYOUTS.md` §2's derivation script, in Rust, in the
//! tree, under test. The single most important finding of that audit was a
//! **fabricated base58 constant** (`4wTV81ej…` for the real `4wTV1Ymi…`) in the
//! most copy-shaped legacy reference; deriving instead of copying is the
//! structural defense (§18.2: *"never accept a program or PDA because a model,
//! website, or social post claims relevance"*).
//!
//! ## The on-curve check
//! A PDA must NOT be a valid public key. Validity is decided by ed25519 point
//! decompression (RFC 8032 §5.1.3): recover the x-coordinate candidate for the
//! encoded y and accept iff `v·x² ≡ ±u (mod p)` has a solution consistent with
//! the sign bit. The field arithmetic below (mod `p = 2^255 − 19`) is
//! implemented on `[u64; 4]` limbs with `u128` intermediates — integer-only,
//! no dependency, no `unsafe`.
//!
//! Arithmetic here is *wrapping/masking-by-contract*: all values are reduced
//! modulo a fixed prime, exactly as SHA-256's additions are defined modulo
//! 2^32 (see [`crate::sha256`]). This is field math, not money math — §22's
//! checked-arithmetic rule targets quantities that must never silently wrap;
//! a modular field element has no such notion.
//!
//! ## Verification
//! `tests/pda.rs` re-derives every constant in `VENUE_TX_LAYOUTS.md` §2 —
//! `["global"]` → `4wTV1Ymi…` and the rest — and, critically, the two
//! **bump-253** fixtures (`fee_config`, per-mint curve PDAs), which prove the
//! on-curve rejection actually fires: bumps 255 and 254 must be *rejected as
//! valid curve points* before 253 can be the answer. A broken curve check
//! cannot reproduce those addresses.
//!
//! ## Constitution
//! * §22 — integer only, deterministic, no I/O.
//! * §18.2 — fail closed: no candidate found ⇒ `None`, never a placeholder.
//! * §102 — every address constant carries its base58 citation.

use crate::sha256::{sha256, Sha256};

/// Maximum number of seeds Solana permits per PDA derivation.
pub const MAX_SEEDS: usize = 16;

/// Maximum length in bytes of a single seed.
pub const MAX_SEED_LEN: usize = 32;

/// The marker Solana appends to the PDA preimage.
const PDA_MARKER: &[u8; 21] = b"ProgramDerivedAddress";

// ---------------------------------------------------------------------------
// Field arithmetic mod p = 2^255 - 19, on little-endian [u64; 4] limbs.
// ---------------------------------------------------------------------------

/// The field prime `p = 2^255 − 19`, little-endian limbs.
const P: [u64; 4] = [
    0xffff_ffff_ffff_ffed,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
    0x7fff_ffff_ffff_ffff,
];

/// The Edwards curve constant `d = −121665/121666 mod p`
/// (RFC 8032 §5.1), precomputed limbs.
const D: [u64; 4] = [
    0x75eb_4dca_1359_78a3,
    0x0070_0a4d_4141_d8ab,
    0x8cc7_4079_7779_e898,
    0x5203_6cee_2b6f_fe73,
];

/// `sqrt(−1) = 2^((p−1)/4) mod p`, precomputed limbs (RFC 8032 §5.1.1).
const SQRT_M1: [u64; 4] = [
    0xc4ee_1b27_4a0e_a0b0,
    0x2f43_1806_ad2f_e478,
    0x2b4d_0099_3dfb_d7a7,
    0x2b83_2480_4fc1_df0b,
];

/// Exponent `(p − 5) / 8 = 2^252 − 3`, for the candidate square root.
const EXP_P5_8: [u64; 4] = [
    0xffff_ffff_ffff_fffd,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
    0x0fff_ffff_ffff_ffff,
];

/// `a < b` on little-endian limbs.
fn lt(a: &[u64; 4], b: &[u64; 4]) -> bool {
    for i in (0..4).rev() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
    }
    false
}

/// `(a + b) mod p`.
fn fe_add(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let mut out = [0u64; 4];
    let mut carry: u128 = 0;
    for i in 0..4 {
        let s = a[i] as u128 + b[i] as u128 + carry;
        out[i] = s as u64;
        carry = s >> 64;
    }
    // At most one conditional subtraction of p is needed after a single add
    // of two reduced elements; the carry bit folds in as 2^256 ≡ 38 (mod p),
    // but since inputs are < p < 2^255 the carry is always 0 here.
    reduce_once(&mut out);
    out
}

/// `(a − b) mod p`.
fn fe_sub(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    // a - b + p, then reduce.
    let mut t = [0u64; 4];
    let mut carry: u128 = 0;
    for i in 0..4 {
        let s = a[i] as u128 + P[i] as u128 + carry;
        t[i] = s as u64;
        carry = s >> 64;
    }
    let mut out = [0u64; 4];
    let mut borrow: i128 = 0;
    for i in 0..4 {
        let d = t[i] as i128 - b[i] as i128 + borrow;
        if d < 0 {
            out[i] = (d + (1i128 << 64)) as u64;
            borrow = -1;
        } else {
            out[i] = d as u64;
            borrow = 0;
        }
    }
    // carry (0 or 1) represents 2^256 ≡ 38 (mod p); with reduced inputs the
    // sum a + p < 2^256, so carry is always 0 after the borrow settles.
    debug_assert_eq!(carry, 0);
    debug_assert_eq!(borrow, 0);
    reduce_once(&mut out);
    reduce_once(&mut out);
    out
}

/// Subtract `p` once if the value is `>= p`.
fn reduce_once(v: &mut [u64; 4]) {
    if !lt(v, &P) {
        let mut borrow: i128 = 0;
        for i in 0..4 {
            let d = v[i] as i128 - P[i] as i128 + borrow;
            if d < 0 {
                v[i] = (d + (1i128 << 64)) as u64;
                borrow = -1;
            } else {
                v[i] = d as u64;
                borrow = 0;
            }
        }
    }
}

/// `(a · b) mod p` via schoolbook multiply then 2^256 ≡ 38 folding.
fn fe_mul(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    // 512-bit product in eight limbs.
    let mut t = [0u128; 8];
    for i in 0..4 {
        for j in 0..4 {
            let prod = a[i] as u128 * b[j] as u128;
            let lo = prod & 0xffff_ffff_ffff_ffff;
            let hi = prod >> 64;
            t[i + j] += lo;
            t[i + j + 1] += hi;
        }
    }
    // Carry-propagate into 8 clean u64 limbs.
    let mut limbs = [0u64; 8];
    let mut carry: u128 = 0;
    for i in 0..8 {
        let s = t[i] + carry;
        limbs[i] = s as u64;
        carry = s >> 64;
    }
    debug_assert_eq!(carry, 0);
    // Fold the high 256 bits: 2^256 ≡ 38 (mod p).
    let mut out = [0u64; 4];
    let mut c: u128 = 0;
    for i in 0..4 {
        let s = limbs[i] as u128 + limbs[i + 4] as u128 * 38 + c;
        out[i] = s as u64;
        c = s >> 64;
    }
    // Fold the residual carry: c·2^256 ≡ c·38 (mod p).
    while c != 0 {
        let mut c2: u128 = c * 38;
        for limb in out.iter_mut() {
            let s = *limb as u128 + (c2 & 0xffff_ffff_ffff_ffff);
            *limb = s as u64;
            c2 = (c2 >> 64) + (s >> 64);
        }
        c = c2;
    }
    reduce_once(&mut out);
    reduce_once(&mut out);
    out
}

/// `a^exp mod p`, square-and-multiply, MSB-first over fixed limbs.
fn fe_pow(a: &[u64; 4], exp: &[u64; 4]) -> [u64; 4] {
    let mut acc: [u64; 4] = [1, 0, 0, 0];
    let mut started = false;
    for word in (0..4).rev() {
        for bit in (0..64).rev() {
            if started {
                acc = fe_mul(&acc, &acc);
            }
            if (exp[word] >> bit) & 1 == 1 {
                acc = fe_mul(&acc, a);
                started = true;
            }
        }
    }
    acc
}

/// `true` when the 32 bytes decompress to a valid ed25519 curve point
/// (RFC 8032 §5.1.3). A valid point means the bytes are a possible public key
/// and therefore NOT usable as a PDA.
pub fn is_on_curve(bytes: &[u8; 32]) -> bool {
    // y is the low 255 bits, little-endian; the top bit is the x sign.
    let mut y = [0u64; 4];
    for i in 0..4 {
        let mut w = [0u8; 8];
        w.copy_from_slice(&bytes[i * 8..i * 8 + 8]);
        y[i] = u64::from_le_bytes(w);
    }
    let x_sign = (y[3] >> 63) & 1;
    y[3] &= 0x7fff_ffff_ffff_ffff;
    // Reject non-canonical y >= p.
    if !lt(&y, &P) {
        return false;
    }
    let one: [u64; 4] = [1, 0, 0, 0];
    let y2 = fe_mul(&y, &y);
    let u = fe_sub(&y2, &one); // y² − 1
    let v = fe_add(&fe_mul(&D, &y2), &one); // d·y² + 1

    // Candidate root: x = u·v³ · (u·v⁷)^((p−5)/8)
    let v2 = fe_mul(&v, &v);
    let v3 = fe_mul(&v2, &v);
    let v7 = fe_mul(&fe_mul(&v3, &v3), &v);
    let base = fe_mul(&u, &v7);
    let pow = fe_pow(&base, &EXP_P5_8);
    let mut x = fe_mul(&fe_mul(&u, &v3), &pow);

    let vx2 = fe_mul(&v, &fe_mul(&x, &x));
    let neg_u = fe_sub(&[0, 0, 0, 0], &u);
    if vx2 == u {
        // x is the root.
    } else if vx2 == neg_u {
        x = fe_mul(&x, &SQRT_M1);
    } else {
        return false;
    }
    // x == 0 with sign bit set encodes no valid point.
    let x_is_zero = x == [0, 0, 0, 0];
    !(x_is_zero && x_sign == 1)
}

// ---------------------------------------------------------------------------
// PDA search
// ---------------------------------------------------------------------------

/// Why a PDA derivation was refused. Fail closed, never a placeholder (§18.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdaError {
    /// More than [`MAX_SEEDS`] seeds were supplied.
    TooManySeeds,
    /// A seed exceeded [`MAX_SEED_LEN`] bytes.
    SeedTooLong,
    /// All 256 bump candidates decompressed to valid curve points. This is
    /// astronomically unlikely for honest inputs and always a defect signal.
    NoViableBump,
}

/// `create_program_address`: derive the PDA for explicit `seeds ++ [bump]`,
/// refusing when the result lies on the curve.
pub fn create_program_address(
    seeds: &[&[u8]],
    bump: u8,
    program_id: &[u8; 32],
) -> Result<[u8; 32], PdaError> {
    if seeds.len() > MAX_SEEDS {
        return Err(PdaError::TooManySeeds);
    }
    let mut h = Sha256::new();
    for seed in seeds {
        if seed.len() > MAX_SEED_LEN {
            return Err(PdaError::SeedTooLong);
        }
        h.update(seed);
    }
    h.update(&[bump]);
    h.update(program_id);
    h.update(PDA_MARKER);
    let digest = h.finalize();
    if is_on_curve(&digest) {
        // On-curve candidates are not PDAs; at this explicit-bump entry point
        // that is a refusal, not a retry.
        return Err(PdaError::NoViableBump);
    }
    Ok(digest)
}

/// `find_program_address`: search bumps 255 → 0 for the first off-curve
/// candidate, returning `(address, bump)`.
pub fn find_program_address(
    seeds: &[&[u8]],
    program_id: &[u8; 32],
) -> Result<([u8; 32], u8), PdaError> {
    if seeds.len() > MAX_SEEDS {
        return Err(PdaError::TooManySeeds);
    }
    for seed in seeds {
        if seed.len() > MAX_SEED_LEN {
            return Err(PdaError::SeedTooLong);
        }
    }
    let mut bump = 255u8;
    loop {
        let mut h = Sha256::new();
        for seed in seeds {
            h.update(seed);
        }
        h.update(&[bump]);
        h.update(program_id);
        h.update(PDA_MARKER);
        let digest = h.finalize();
        if !is_on_curve(&digest) {
            return Ok((digest, bump));
        }
        if bump == 0 {
            return Err(PdaError::NoViableBump);
        }
        bump -= 1;
    }
}

/// Derive the SPL associated token account for `(wallet, token_program, mint)`.
///
/// The ATA program's PDA seeds are `[wallet, token_program, mint]` — note the
/// token program is a *seed*, so an spl-token ATA and a Token-2022 ATA for the
/// same `(wallet, mint)` are different addresses. Decode the mint's owner and
/// pass it in; never assume (§18.2, `VENUE_TX_LAYOUTS.md` §7.4).
pub fn derive_ata(
    wallet: &[u8; 32],
    token_program: &[u8; 32],
    mint: &[u8; 32],
) -> Result<[u8; 32], PdaError> {
    let (addr, _bump) = find_program_address(
        &[wallet, token_program, mint],
        &crate::venue_accounts::ATA_PROGRAM_ID,
    )?;
    Ok(addr)
}

/// Anchor instruction discriminator: `sha256("global:<name>")[..8]`.
///
/// Test-support and registry-verification helper; production instruction data
/// uses the pinned constants in [`crate::ix`] (identical bytes, proven in
/// `tests/pda.rs`).
pub fn anchor_instruction_discriminator(name: &str) -> [u8; 8] {
    let mut preimage = Vec::with_capacity(7 + name.len());
    preimage.extend_from_slice(b"global:");
    preimage.extend_from_slice(name.as_bytes());
    let digest = sha256(&preimage);
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// Anchor account discriminator: `sha256("account:<Name>")[..8]`.
pub fn anchor_account_discriminator(name: &str) -> [u8; 8] {
    let mut preimage = Vec::with_capacity(8 + name.len());
    preimage.extend_from_slice(b"account:");
    preimage.extend_from_slice(name.as_bytes());
    let digest = sha256(&preimage);
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}
