//! Pre-serialized transaction skeletons for zero-allocation exit path.
//!
//! ## Design
//!
//! At position open we build a complete Pump.fun sell transaction message using
//! placeholder values for the 4 fields that change at exit time:
//!
//! 1. **vSOL reserves** (u64 LE) — in the sell instruction data (`min_sol_out`)
//! 2. **vToken reserves** (u64 LE) — not directly in the sell ix, but we keep
//!    an offset so the caller can recalculate `min_sol_out` externally and patch it.
//!    In practice, we patch `min_sol_out` (the second u64 after discriminator+tokens_to_sell).
//! 3. **Recent blockhash** (32 bytes) — in the message header
//! 4. **Jito tip amount** (u64 LE) — in the system transfer instruction
//!
//! At exit we copy the skeleton, overwrite 56 bytes at known offsets, and hand
//! the patched message to the signer. Total hot-path cost: one memcpy + 4 small
//! writes ≈ 200–400 ns.
//!
//! ## Why store the message, not the full transaction?
//!
//! The Ed25519 signature covers the serialized message. Since patching changes the
//! message, the signature must be recomputed anyway. We store only the message bytes
//! so the caller can: `patch → sign → prepend signature → submit`.

use std::str::FromStr;

use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::{v0, VersionedMessage},
    pubkey::Pubkey,
    system_instruction,
    system_program,
    sysvar,
};

use super::builder::{
    JITO_TIP_ACCOUNTS, PUMP_EVENT_AUTHORITY, PUMP_FEE_RECIPIENT, PUMP_FUN_PROGRAM, PUMP_GLOBAL,
    SELL_DISCRIMINATOR,
};

// ── Constants ────────────────────────────────────────────────────────────────

/// SPL Token program ID (same as builder.rs but we need it locally).
const SPL_TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// SPL Associated Token Account program ID.
const SPL_ATA_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

/// Maximum serialized *message* size. Solana tx limit is 1232 bytes total
/// (signatures + message). A V0 sell message without ALTs is ~560 bytes due to
/// 12 account keys for the pump.fun sell + compute budget + tip instructions.
/// We use 768 to leave headroom.
pub const MAX_SKELETON_SIZE: usize = 768;

/// Sentinel values used as placeholders to locate patchable offsets.
/// Chosen to be astronomically unlikely in real transaction data.
const PLACEHOLDER_VSOL: u64 = 0xDEAD_BEEF_CAFE_0001;
const PLACEHOLDER_VTOKENS: u64 = 0xDEAD_BEEF_CAFE_0002;
const PLACEHOLDER_TIP: u64 = 0xDEAD_BEEF_CAFE_0003;
/// A recognizable 32-byte blockhash placeholder.
const PLACEHOLDER_BLOCKHASH: [u8; 32] = [
    0xBB, 0xBB, 0xBB, 0xBB, 0xBB, 0xBB, 0xBB, 0xBB, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC,
    0xCC, 0xDD, 0xDD, 0xDD, 0xDD, 0xDD, 0xDD, 0xDD, 0xDD, 0xEE, 0xEE, 0xEE, 0xEE, 0xEE, 0xEE,
    0xEE, 0xEE,
];

// ── Public types ─────────────────────────────────────────────────────────────

/// Offsets of patchable fields within the serialized **message** bytes.
/// Determined once at skeleton build time.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PatchOffsets {
    /// Offset of `min_sol_out` (u64 LE) in the sell instruction data.
    /// Caller computes min_sol_out from reserves externally, then patches here.
    pub vsol_reserves: u16,
    /// Offset of a second patchable u64 — we store the vToken placeholder
    /// right after min_sol_out in a dedicated extension field.
    /// In practice this patches the `tokens_to_sell` field if the position
    /// size changes, or a custom data region. See note below.
    pub vtoken_reserves: u16,
    /// Offset of the recent blockhash (32 bytes) in the message header.
    pub blockhash: u16,
    /// Offset of the tip amount (u64 LE) in the Jito system_transfer ix.
    pub tip_amount: u16,
    /// Total serialized message length.
    pub total_len: u16,
}

/// A pre-built transaction message skeleton stored entirely on the stack.
///
/// The skeleton contains the serialized V0 message with placeholder values
/// in the 4 patchable fields. At exit time, call [`TxSkeleton::patch`] to
/// produce a ready-to-sign message in <1μs.
pub struct TxSkeleton {
    /// Raw serialized message bytes (stack-allocated).
    pub data: [u8; MAX_SKELETON_SIZE],
    /// Patch offsets for dynamic fields.
    pub offsets: PatchOffsets,
}

/// Errors during skeleton construction.
#[derive(Debug)]
pub enum SkeletonError {
    /// Failed to compile or serialize the V0 message.
    SerializationFailed,
    /// Serialized message exceeds [`MAX_SKELETON_SIZE`].
    TooLarge,
    /// Could not locate a placeholder sentinel in the serialized bytes.
    PlaceholderNotFound(&'static str),
}

impl core::fmt::Display for SkeletonError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SerializationFailed => write!(f, "skeleton: V0 message serialization failed"),
            Self::TooLarge => write!(
                f,
                "skeleton: serialized message exceeds {} bytes",
                MAX_SKELETON_SIZE
            ),
            Self::PlaceholderNotFound(field) => {
                write!(f, "skeleton: placeholder not found for `{}`", field)
            }
        }
    }
}

// ── ATA derivation (same as builder.rs — avoids the spl dep) ────────────────

fn get_associated_token_address(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM).unwrap();
    let ata_program = Pubkey::from_str(SPL_ATA_PROGRAM).unwrap();
    let (addr, _bump) = Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    );
    addr
}

// ── Implementation ───────────────────────────────────────────────────────────

impl TxSkeleton {
    /// Build a sell-transaction skeleton from known position parameters.
    ///
    /// Called **once** when a position is opened (cold path, ~5-10μs is fine).
    /// The resulting skeleton can then be patched in <1μs at exit time.
    ///
    /// # Arguments
    ///
    /// * `mint` — SPL token mint address (raw 32 bytes)
    /// * `bonding_curve` — Pump.fun bonding curve account
    /// * `assoc_bonding_curve` — Bonding curve's associated token account
    /// * `wallet_pubkey` — Our wallet public key (signer)
    /// * `tokens_held` — Number of tokens we hold (used as `tokens_to_sell`)
    /// * `placeholder_vsol` — Ignored; we use internal sentinel.
    ///   Pass 0 — the field is patched at exit.
    /// * `placeholder_vtokens` — Ignored; we use internal sentinel.
    ///   Pass 0 — the field is patched at exit.
    ///
    /// # Sell instruction layout (Pump.fun Anchor)
    ///
    /// ```text
    /// [8 bytes discriminator][8 bytes tokens_to_sell (u64 LE)][8 bytes min_sol_out (u64 LE)]
    /// ```
    ///
    /// We place sentinels in `min_sol_out` (→ vsol_reserves offset) and
    /// `tokens_to_sell` (→ vtoken_reserves offset) so both are patchable.
    pub fn build_sell_skeleton(
        mint: &[u8; 32],
        bonding_curve: &[u8; 32],
        assoc_bonding_curve: &[u8; 32],
        wallet_pubkey: &[u8; 32],
        _tokens_held: u64,
        _placeholder_vsol: u64,
        _placeholder_vtokens: u64,
    ) -> Result<Self, SkeletonError> {
        // Parse pubkeys
        let mint_pk = Pubkey::new_from_array(*mint);
        let bc_pk = Pubkey::new_from_array(*bonding_curve);
        let abc_pk = Pubkey::new_from_array(*assoc_bonding_curve);
        let wallet_pk = Pubkey::new_from_array(*wallet_pubkey);
        let pump_program = Pubkey::from_str(PUMP_FUN_PROGRAM).unwrap();
        let global = Pubkey::from_str(PUMP_GLOBAL).unwrap();
        let fee_recipient = Pubkey::from_str(PUMP_FEE_RECIPIENT).unwrap();
        let event_authority = Pubkey::from_str(PUMP_EVENT_AUTHORITY).unwrap();
        let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM).unwrap();

        // Build sell instruction data with sentinels:
        //   [discriminator 8B][tokens_to_sell 8B = PLACEHOLDER_VTOKENS][min_sol_out 8B = PLACEHOLDER_VSOL]
        //
        // We use tokens_held as real value but store PLACEHOLDER_VTOKENS instead so
        // we can patch it later if position size changes (partial exit). The caller
        // can also just re-patch it to the same tokens_held value.
        let mut sell_data = Vec::with_capacity(24);
        sell_data.extend_from_slice(&SELL_DISCRIMINATOR);
        sell_data.extend_from_slice(&PLACEHOLDER_VTOKENS.to_le_bytes()); // tokens_to_sell slot
        sell_data.extend_from_slice(&PLACEHOLDER_VSOL.to_le_bytes()); // min_sol_out slot

        // ATA for our wallet + mint
        let associated_user = get_associated_token_address(&wallet_pk, &mint_pk);

        // Account metas — identical to builder.rs sell layout
        let accounts = vec![
            AccountMeta::new_readonly(global, false),
            AccountMeta::new(fee_recipient, false),
            AccountMeta::new_readonly(mint_pk, false),
            AccountMeta::new(bc_pk, false),
            AccountMeta::new(abc_pk, false),
            AccountMeta::new(associated_user, false),
            AccountMeta::new(wallet_pk, true),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(token_program, false),
            AccountMeta::new_readonly(sysvar::rent::id(), false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(pump_program, false),
        ];

        let sell_ix = Instruction {
            program_id: pump_program,
            accounts,
            data: sell_data,
        };

        // Jito tip — use first tip account (skeleton doesn't rotate; the offset
        // is the same regardless of which tip pubkey is used, only the account
        // key differs and that doesn't change per-position).
        let tip_account = Pubkey::from_str(JITO_TIP_ACCOUNTS[0]).unwrap();
        let tip_ix = system_instruction::transfer(&wallet_pk, &tip_account, PLACEHOLDER_TIP);

        // Full instruction set: CU limit, CU price, sell, tip
        let ixs = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(200_000),
            ComputeBudgetInstruction::set_compute_unit_price(0), // price doesn't need patching
            sell_ix,
            tip_ix,
        ];

        // Compile V0 message with placeholder blockhash
        let blockhash = Hash::new_from_array(PLACEHOLDER_BLOCKHASH);
        let msg = v0::Message::try_compile(&wallet_pk, &ixs, &[], blockhash)
            .map_err(|_| SkeletonError::SerializationFailed)?;

        // Serialize the message
        let versioned_msg = VersionedMessage::V0(msg);
        let serialized =
            versioned_msg.serialize();

        if serialized.len() > MAX_SKELETON_SIZE {
            return Err(SkeletonError::TooLarge);
        }

        // Copy into stack buffer
        let mut data = [0u8; MAX_SKELETON_SIZE];
        data[..serialized.len()].copy_from_slice(&serialized);

        // ── Locate patch offsets by scanning for sentinels ────────────────

        let vsol_offset = find_u64_offset(&serialized, PLACEHOLDER_VSOL)
            .ok_or(SkeletonError::PlaceholderNotFound("vsol_reserves"))?;

        let vtoken_offset = find_u64_offset(&serialized, PLACEHOLDER_VTOKENS)
            .ok_or(SkeletonError::PlaceholderNotFound("vtoken_reserves"))?;

        let blockhash_offset = find_bytes_offset(&serialized, &PLACEHOLDER_BLOCKHASH)
            .ok_or(SkeletonError::PlaceholderNotFound("blockhash"))?;

        let tip_offset = find_u64_offset(&serialized, PLACEHOLDER_TIP)
            .ok_or(SkeletonError::PlaceholderNotFound("tip_amount"))?;

        Ok(Self {
            data,
            offsets: PatchOffsets {
                vsol_reserves: vsol_offset as u16,
                vtoken_reserves: vtoken_offset as u16,
                blockhash: blockhash_offset as u16,
                tip_amount: tip_offset as u16,
                total_len: serialized.len() as u16,
            },
        })
    }

    /// Patch dynamic fields and return the number of bytes written to `out`.
    ///
    /// This is the **HOT PATH** — designed to complete in <1μs.
    ///
    /// The caller receives a patched, unsigned V0 message in `out[..returned_len]`.
    /// To produce a submittable transaction:
    ///
    /// 1. Sign the message bytes with the wallet keypair
    /// 2. Prepend the compact signature array + signature
    /// 3. Submit via Jito / RPC
    ///
    /// # Arguments
    ///
    /// * `vsol_reserves` — Current virtual SOL reserves (used to compute `min_sol_out`;
    ///   the caller should pre-compute `min_sol_out` and pass it here).
    /// * `vtoken_reserves` — Current virtual token reserves (patches `tokens_to_sell`;
    ///   pass the actual tokens_to_sell value here).
    /// * `recent_blockhash` — Fresh blockhash (32 bytes).
    /// * `tip_lamports` — Jito tip amount in lamports.
    /// * `out` — Output buffer (stack-allocated by caller).
    ///
    /// # Returns
    ///
    /// Number of valid bytes in `out`.
    #[inline(always)]
    pub fn patch(
        &self,
        vsol_reserves: u64,
        vtoken_reserves: u64,
        recent_blockhash: &[u8; 32],
        tip_lamports: u64,
        out: &mut [u8; MAX_SKELETON_SIZE],
    ) -> usize {
        let len = self.offsets.total_len as usize;

        // 1. Bulk copy skeleton → output
        out[..len].copy_from_slice(&self.data[..len]);

        let o = &self.offsets;

        // 2. Patch min_sol_out (vsol_reserves offset)
        out[o.vsol_reserves as usize..o.vsol_reserves as usize + 8]
            .copy_from_slice(&vsol_reserves.to_le_bytes());

        // 3. Patch tokens_to_sell (vtoken_reserves offset)
        out[o.vtoken_reserves as usize..o.vtoken_reserves as usize + 8]
            .copy_from_slice(&vtoken_reserves.to_le_bytes());

        // 4. Patch blockhash
        out[o.blockhash as usize..o.blockhash as usize + 32]
            .copy_from_slice(recent_blockhash);

        // 5. Patch tip
        out[o.tip_amount as usize..o.tip_amount as usize + 8]
            .copy_from_slice(&tip_lamports.to_le_bytes());

        len
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Find the byte offset of a u64 LE sentinel in `haystack`.
fn find_u64_offset(haystack: &[u8], needle: u64) -> Option<usize> {
    let needle_bytes = needle.to_le_bytes();
    haystack
        .windows(8)
        .position(|w| w == needle_bytes)
}

/// Find the byte offset of an arbitrary byte pattern in `haystack`.
fn find_bytes_offset(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate deterministic test pubkeys from a seed byte.
    fn test_pubkey(seed: u8) -> [u8; 32] {
        let mut pk = [0u8; 32];
        pk[0] = seed;
        pk[31] = seed.wrapping_mul(7);
        pk
    }

    fn build_test_skeleton() -> TxSkeleton {
        // Use a real wallet pubkey (just needs to be a valid 32 bytes)
        let wallet = test_pubkey(0xAA);
        let mint = test_pubkey(0x11);
        let bonding_curve = test_pubkey(0x22);
        let assoc_bonding_curve = test_pubkey(0x33);

        TxSkeleton::build_sell_skeleton(
            &mint,
            &bonding_curve,
            &assoc_bonding_curve,
            &wallet,
            1_000_000_000, // 1B tokens
            0,             // placeholder_vsol (ignored)
            0,             // placeholder_vtokens (ignored)
        )
        .expect("failed to build skeleton")
    }

    #[test]
    fn test_skeleton_patch_roundtrip() {
        let skeleton = build_test_skeleton();

        // Ensure offsets are within bounds
        let o = &skeleton.offsets;
        let len = o.total_len as usize;
        assert!(o.vsol_reserves as usize + 8 <= len);
        assert!(o.vtoken_reserves as usize + 8 <= len);
        assert!(o.blockhash as usize + 32 <= len);
        assert!(o.tip_amount as usize + 8 <= len);

        // Patch with known values
        let vsol: u64 = 30_000_000_000; // 30 SOL in lamports
        let vtokens: u64 = 500_000_000;
        let blockhash = [0x42u8; 32];
        let tip: u64 = 100_000; // 0.0001 SOL

        let mut out = [0u8; MAX_SKELETON_SIZE];
        let out_len = skeleton.patch(vsol, vtokens, &blockhash, tip, &mut out);

        assert_eq!(out_len, len);

        // Verify patched values are at the correct offsets
        let vsol_bytes =
            &out[o.vsol_reserves as usize..o.vsol_reserves as usize + 8];
        assert_eq!(vsol_bytes, &vsol.to_le_bytes());

        let vtokens_bytes =
            &out[o.vtoken_reserves as usize..o.vtoken_reserves as usize + 8];
        assert_eq!(vtokens_bytes, &vtokens.to_le_bytes());

        let bh_bytes =
            &out[o.blockhash as usize..o.blockhash as usize + 32];
        assert_eq!(bh_bytes, &blockhash);

        let tip_bytes =
            &out[o.tip_amount as usize..o.tip_amount as usize + 8];
        assert_eq!(tip_bytes, &tip.to_le_bytes());

        // Verify the rest of the skeleton is unchanged:
        // pick a region before the first patchable field (if any) and compare
        // (simple sanity — first 4 bytes should be message prefix, not zeroed)
        assert_ne!(&out[..4], &[0u8; 4], "output should not be all zeros at start");

        // Patch again with different values — should overwrite cleanly
        let vsol2: u64 = 99_999;
        let vtokens2: u64 = 12_345;
        let blockhash2 = [0xFF; 32];
        let tip2: u64 = 1;

        let mut out2 = [0u8; MAX_SKELETON_SIZE];
        let out_len2 = skeleton.patch(vsol2, vtokens2, &blockhash2, tip2, &mut out2);
        assert_eq!(out_len2, len);

        assert_eq!(
            &out2[o.vsol_reserves as usize..o.vsol_reserves as usize + 8],
            &vsol2.to_le_bytes()
        );
        assert_eq!(
            &out2[o.tip_amount as usize..o.tip_amount as usize + 8],
            &tip2.to_le_bytes()
        );
    }

    #[test]
    fn test_patch_is_fast() {
        let skeleton = build_test_skeleton();

        let vsol: u64 = 30_000_000_000;
        let vtokens: u64 = 500_000_000;
        let blockhash = [0x42u8; 32];
        let tip: u64 = 100_000;

        // Warm up
        let mut out = [0u8; MAX_SKELETON_SIZE];
        for _ in 0..1000 {
            std::hint::black_box(skeleton.patch(vsol, vtokens, &blockhash, tip, &mut out));
        }

        // Measure
        let iterations = 10_000;
        let start = std::time::Instant::now();
        for i in 0..iterations {
            let v = std::hint::black_box(vsol.wrapping_add(i as u64));
            std::hint::black_box(skeleton.patch(v, vtokens, &blockhash, tip, &mut out));
        }
        let elapsed = start.elapsed();
        let per_call_ns = elapsed.as_nanos() / iterations as u128;

        eprintln!(
            "patch() latency: {} ns/call ({} iterations, total {:?})",
            per_call_ns, iterations, elapsed
        );

        // Assert <1μs (1000ns). On modern hardware this should be ~100-300ns.
        assert!(
            per_call_ns < 1_000,
            "patch() took {}ns — exceeds 1μs budget",
            per_call_ns
        );
    }

    #[test]
    fn test_skeleton_size_within_limit() {
        let skeleton = build_test_skeleton();
        let len = skeleton.offsets.total_len as usize;

        eprintln!("skeleton serialized message size: {} bytes", len);

        // Should fit in MAX_SKELETON_SIZE
        assert!(
            len <= MAX_SKELETON_SIZE,
            "skeleton {} bytes > max {}",
            len,
            MAX_SKELETON_SIZE
        );

        // Should be reasonable for a pump.fun sell tx.
        // Without ALTs, the V0 message is ~560 bytes (12 unique account keys).
        assert!(
            len >= 100,
            "skeleton suspiciously small: {} bytes",
            len
        );
        assert!(
            len <= 700,
            "skeleton unexpectedly large: {} bytes",
            len
        );
    }

    #[test]
    fn test_offsets_are_non_overlapping() {
        let skeleton = build_test_skeleton();
        let o = &skeleton.offsets;

        // Collect all (offset, size) pairs
        let fields: Vec<(u16, u16)> = vec![
            (o.vsol_reserves, 8),
            (o.vtoken_reserves, 8),
            (o.blockhash, 32),
            (o.tip_amount, 8),
        ];

        // Check no overlaps
        for (i, &(off_a, sz_a)) in fields.iter().enumerate() {
            for (j, &(off_b, sz_b)) in fields.iter().enumerate() {
                if i == j {
                    continue;
                }
                let a_end = off_a + sz_a;
                let b_end = off_b + sz_b;
                assert!(
                    a_end <= off_b || b_end <= off_a,
                    "fields {} and {} overlap: [{}, {}) vs [{}, {})",
                    i,
                    j,
                    off_a,
                    a_end,
                    off_b,
                    b_end
                );
            }
        }
    }
}
