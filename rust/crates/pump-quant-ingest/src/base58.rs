//! Manual base58 decoder (Bitcoin / Solana alphabet).
//!
//! Responsibility: turn a base58 pubkey/signature string into raw bytes without
//! pulling in the `bs58` crate. The legacy feeds used `bs58::decode().onto()`
//! to fill fixed `[u8; 32]` / `[u8; 64]` buffers; this module reproduces the
//! same result with a deterministic, allocation-simple big-integer
//! multiply-accumulate. Pure integer arithmetic only (§22).

/// Base58 alphabet used by Bitcoin and Solana (no `0`, `O`, `I`, `l`).
const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Map one base58 character to its 0..=57 digit value, or `None` if the byte is
/// not in the alphabet.
fn digit_value(c: u8) -> Option<u32> {
    ALPHABET.iter().position(|&x| x == c).map(|p| p as u32)
}

/// Decode a base58 string into its big-endian byte representation.
///
/// Returns `None` if any character is outside the alphabet. Leading `'1'`
/// characters decode to leading `0x00` bytes, matching the canonical base58
/// convention (so the all-zero 32-byte / 64-byte keys round-trip correctly).
///
/// Complexity is O(n²) in the input length, which is irrelevant for the ≤88
/// character keys this crate decodes.
pub fn decode(s: &str) -> Option<Vec<u8>> {
    // `num` holds the running value in base 256, big-endian (most significant
    // byte first). For each base58 digit d: num = num * 58 + d.
    let mut num: Vec<u8> = Vec::new();

    for &c in s.as_bytes() {
        let mut carry = digit_value(c)?;
        for byte in num.iter_mut().rev() {
            let x = *byte as u32 * 58 + carry;
            *byte = (x & 0xff) as u8;
            carry = x >> 8;
        }
        while carry > 0 {
            num.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }

    // Each leading '1' is a leading zero byte.
    let zeros = s.as_bytes().iter().take_while(|&&c| c == b'1').count();
    let mut out = vec![0u8; zeros];
    out.extend_from_slice(&num);
    Some(out)
}

/// Decode a base58 string that must be exactly 32 bytes (a Solana pubkey/mint).
/// Returns `None` on decode error or wrong length — same contract as the
/// legacy `decode_pubkey`.
pub fn decode_pubkey(s: &str) -> Option<[u8; 32]> {
    let v = decode(s)?;
    if v.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&v);
    Some(arr)
}

/// Decode a base58 string that must be exactly 64 bytes (a Solana signature).
/// Returns `None` on decode error or wrong length — same contract as the
/// legacy `decode_sig`.
pub fn decode_signature(s: &str) -> Option<[u8; 64]> {
    let v = decode(s)?;
    if v.len() != 64 {
        return None;
    }
    let mut arr = [0u8; 64];
    arr.copy_from_slice(&v);
    Some(arr)
}
