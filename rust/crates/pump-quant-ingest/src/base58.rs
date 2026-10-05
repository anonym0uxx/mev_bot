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

/// Encode bytes as base58 (Bitcoin / Solana alphabet): the exact inverse of [`decode`], including
/// leading zero bytes as leading `'1'`s. Used to render a mint the way the corpus prints it.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    // Base-58 digits, least significant first.
    let mut digits: Vec<u8> = Vec::new();
    for &b in &bytes[zeros..] {
        let mut carry = u32::from(b);
        for d in &mut digits {
            let x = u32::from(*d) * 256 + carry;
            *d = (x % 58) as u8;
            carry = x / 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut out = String::with_capacity(zeros + digits.len());
    out.extend(std::iter::repeat_n('1', zeros));
    for &d in digits.iter().rev() {
        out.push(ALPHABET[d as usize] as char);
    }
    out
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

#[cfg(test)]
mod encode_tests {
    #[test]
    fn encode_is_the_inverse_of_decode_including_zero_prefixes() {
        for s in [
            "11111111111111111111111111111111",
            "8kHikWnr5jsKXEJoruR9omqwXskmKTmW6RuSabzLpump",
            "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
        ] {
            let b = super::decode(s).expect("decodes");
            assert_eq!(super::encode(&b), s);
        }
        assert_eq!(super::encode(&[]), "");
        assert_eq!(super::encode(&[0, 0, 1]), "112");
    }
}
