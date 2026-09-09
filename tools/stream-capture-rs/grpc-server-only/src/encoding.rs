//! Encoding + hashing utilities for the training-capture recorder.
//!
//! All functions are allocation-conscious and side-effect-free. No secrets
//! are ever logged — these helpers operate only on public chain data.

use sha2::{Digest, Sha256};

/// Bitcoin-alphabet base58 encoder (for pubkeys/signatures → string).
pub fn b58_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    // Zero has no significant digits; leading zero bytes are emitted below.
    let mut digits: Vec<u8> = Vec::new();
    for &byte in bytes {
        let mut carry = byte as u32;
        for d in digits.iter_mut() {
            carry += (*d as u32) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    let mut out = String::with_capacity(zeros + digits.len());
    out.extend(std::iter::repeat('1').take(zeros));
    out.extend(digits.iter().rev().map(|&d| ALPHABET[d as usize] as char));
    out
}

#[cfg(test)]
mod tests {
    use super::b58_encode;

    #[test]
    fn b58_empty_is_empty() {
        assert_eq!(b58_encode(&[]), "");
    }

    #[test]
    fn b58_one_zero_is_one_one() {
        assert_eq!(b58_encode(&[0]), "1");
    }

    #[test]
    fn b58_zero_pubkey_is_32_ones() {
        assert_eq!(b58_encode(&[0; 32]), "1".repeat(32));
    }

    #[test]
    fn b58_zero_signature_is_64_ones() {
        assert_eq!(b58_encode(&[0; 64]), "1".repeat(64));
    }

    #[test]
    fn b58_leading_zeros_preserve_nonzero_value() {
        for (bytes, expected) in [
            (&[0, 1][..], "12"),
            (&[0, 0, 1][..], "112"),
            (&[0, 58][..], "121"),
            (&[0, 0, 255][..], "115Q"),
            (&[0, 1, 0][..], "15R"),
        ] {
            assert_eq!(b58_encode(bytes), expected, "input: {bytes:?}");
        }
    }

    #[test]
    fn b58_known_bitcoin_alphabet_fixtures() {
        for (bytes, expected) in [
            (&[1][..], "2"),
            (&[57][..], "z"),
            (&[58][..], "21"),
            (&[255][..], "5Q"),
            (&[1, 0][..], "5R"),
            (&b"a"[..], "2g"),
            (&b"bbb"[..], "a3gV"),
            (&b"ccc"[..], "aPEr"),
            (&b"simply a long string"[..], "2cFupjhnEsSn59qHXstmK2ffpLv2"),
            (&b"Hello World"[..], "JxF12TrwUP45BMd"),
        ] {
            assert_eq!(b58_encode(bytes), expected, "input: {bytes:?}");
        }
    }

    #[test]
    fn b58_all_zero_lengths_have_no_extra_digit() {
        for len in 0..=256 {
            assert_eq!(b58_encode(&vec![0; len]), "1".repeat(len), "length: {len}");
        }
    }
}

/// Standard-alphabet base64 encoder.
pub fn b64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Lowercase hex encoder.
pub fn hex_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 2);
    for &b in data {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// SHA-256 hash of a byte slice → hex string (for record dedupe + manifest).
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

/// Current unix time in milliseconds (best-effort; 0 on failure).
pub fn now_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
