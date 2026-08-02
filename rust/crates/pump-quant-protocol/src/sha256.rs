//! From-scratch, integer-only SHA-256 (FIPS 180-4) — protocol-crate copy.
//!
//! ## Responsibility
//! Provide the deterministic 32-byte digest primitive under program-derived-
//! address search ([`crate::pda`]) and Anchor discriminator derivation
//! (`sha256("global:<name>")[..8]`, `sha256("account:<Name>")[..8]`).
//!
//! ## Why a copy and not a dependency
//! `pump-quant-governance` carries the identical implementation for its own
//! hashing guards, with the stated rationale that the crate stays
//! self-contained so every result is reproducible with no external code. The
//! same rationale applies here, and the dependency direction forbids the
//! alternative: governance sits above protocol, and this crate has ZERO
//! dependencies by design (hot-path lint scope). The two copies are pinned to
//! each other by the shared FIPS 180-4 / NIST test vectors in
//! `tests/sha256.rs` — a divergence fails both suites identically.
//!
//! ## §22 / §705 compliance
//! No floating point. All state is `u32`/`u64`. SHA-256's arithmetic is defined
//! modulo 2^32, so additions use [`u32::wrapping_add`] *wrapping-by-contract* —
//! the wrap is the specification, not an accident.
//!
//! ## Verification
//! Independently checkable against the published FIPS 180-4 / NIST test vectors
//! (empty string, `"abc"`, the 448-bit message, and the one-million-`'a'`
//! message) in `tests/sha256.rs`, plus every PDA fixture in `tests/pda.rs` —
//! a defective digest cannot reproduce the venue's known program addresses.

/// The eight SHA-256 initial hash values (FIPS 180-4 §5.3.3): the first 32 bits
/// of the fractional parts of the square roots of the first eight primes.
const H0: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// The 64 SHA-256 round constants (FIPS 180-4 §4.2.2): the first 32 bits of the
/// fractional parts of the cube roots of the first 64 primes.
const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// Streaming SHA-256 hasher with a fixed 64-byte block buffer.
///
/// ## Memory bound (§57)
/// Fixed size: eight `u32` of chained state, a 64-byte block, a byte counter.
/// No heap allocation and no unbounded growth regardless of input length.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    /// Number of bytes buffered in `block` (always `0..64`).
    buffered: usize,
    /// Total message length in bytes, for the FIPS length padding.
    total_len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// Construct a fresh hasher primed with the FIPS initial hash values.
    pub fn new() -> Self {
        Self {
            state: H0,
            block: [0u8; 64],
            buffered: 0,
            total_len: 0,
        }
    }

    /// Absorb `data` into the running digest.
    ///
    /// The total length counter uses [`u64::wrapping_add`]; per FIPS 180-4 the
    /// message length is taken modulo 2^64, so this is wrapping-by-contract.
    pub fn update(&mut self, data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        for &byte in data {
            self.block[self.buffered] = byte;
            self.buffered += 1;
            if self.buffered == 64 {
                self.process_block();
                self.buffered = 0;
            }
        }
    }

    /// Finish and return the 32-byte digest. Consumes the hasher.
    pub fn finalize(mut self) -> [u8; 32] {
        // FIPS 180-4 §5.1.1 padding: append 0x80, then zeros, then the 64-bit
        // big-endian bit length, so the padded message is a multiple of 512
        // bits (64 bytes).
        let bit_len = self.total_len.wrapping_mul(8);

        // The 0x80 terminator.
        self.absorb_byte(0x80);
        // Pad with zeros until exactly 8 bytes remain in the final block.
        while self.buffered != 56 {
            self.absorb_byte(0x00);
        }
        // The 64-bit big-endian bit length.
        for &b in &bit_len.to_be_bytes() {
            self.absorb_byte(b);
        }
        debug_assert_eq!(self.buffered, 0, "final block must be flushed");

        let mut out = [0u8; 32];
        for (word, chunk) in self.state.iter().zip(out.chunks_exact_mut(4)) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// Push a single byte during finalization, flushing a full block.
    fn absorb_byte(&mut self, byte: u8) {
        self.block[self.buffered] = byte;
        self.buffered += 1;
        if self.buffered == 64 {
            self.process_block();
            self.buffered = 0;
        }
    }

    /// The SHA-256 compression function over the current 64-byte block
    /// (FIPS 180-4 §6.2.2). Every `+` here is modulo 2^32 by specification and
    /// therefore uses `wrapping_add` deliberately.
    fn process_block(&mut self) {
        let mut w = [0u32; 64];
        for (i, chunk) in self.block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        let deltas = [a, b, c, d, e, f, g, h];
        for (slot, delta) in self.state.iter_mut().zip(deltas.iter()) {
            *slot = slot.wrapping_add(*delta);
        }
    }
}

/// One-shot convenience: the SHA-256 digest of `data`.
///
/// ## Constitution
/// The deterministic primitive under §56.3 hashing / §44 evaluator pinning.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// Lower-case hex encoding of a 32-byte digest, for logs and fixtures.
///
/// Never used in outcome-controlling logic; a display helper only.
pub fn to_hex(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for &byte in digest {
        s.push(HEX[(byte >> 4) as usize] as char);
        s.push(HEX[(byte & 0x0f) as usize] as char);
    }
    s
}
