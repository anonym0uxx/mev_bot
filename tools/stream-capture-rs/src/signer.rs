//! Wallet signer: ed25519 signing for live transaction submission.
//!
//! This is the module that turns bytes into an authorisation to move money. It
//! is written to be boring, to fail closed, and to be impossible to use by
//! accident.
//!
//! ## Dependencies: `ring`, which is already in the tree
//! `rustls` is depended on with `features = ["ring"]`, so the `ring` crate is
//! already vendored. Depending on it directly adds ZERO new vendored code —
//! exactly the precedent this crate's Cargo.toml already sets for `base64` and
//! `getrandom`. No `solana-sdk`, no `ed25519-dalek`, no new supply chain.
//!
//! ## The four controls
//! 1. **Expected pubkey is mandatory.** [`WalletSigner::load_solana_keypair`]
//!    takes the address you believe the file holds and refuses if it does not
//!    match. A signer that will sign with whatever key it happens to find is a
//!    signer that will happily sign with a swapped file.
//! 2. **The file is cross-checked against itself.** A Solana CLI keypair is 64
//!    bytes: a 32-byte seed followed by the 32-byte public key it derives. `ring`
//!    verifies that relationship on construction, so a corrupted or hand-edited
//!    file is rejected rather than producing signatures no validator accepts.
//! 3. **A sign/verify self-test runs at load.** The whole path — seed to
//!    signature to verification — is exercised on a fixed message before the
//!    signer is handed out. A key that cannot sign is discovered at startup, not
//!    at the moment an exit needs to land.
//! 4. **Secret bytes are zeroed and never printable.** The seed buffer is
//!    overwritten after construction, `Debug` prints only the public address,
//!    and there is no `Display`, no `Clone`, and no accessor that returns key
//!    material.
//!
//! ## What this module does not do
//! It does not build transactions. It signs a byte slice the caller supplies.
//! Transaction assembly — instruction layout, account ordering, PDA derivation —
//! lives with the protocol crate that already encodes that knowledge, and this
//! module deliberately holds no opinion about what it is signing beyond the size
//! sanity check.
//!
//! ## §22 / §18.8
//! No floats, no clock, no RNG on the signing path: ed25519 signatures are
//! deterministic, so the same message and key always produce the same 64 bytes,
//! which is what makes a signature reproducible in a replay. Every failure is a
//! typed error with a cause, never a bare `None`.

use std::fmt;
use std::path::Path;

use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};

/// Bytes in a Solana CLI keypair file: 32-byte seed then 32-byte public key.
pub const KEYPAIR_BYTES: usize = 64;

/// Bytes in an ed25519 seed, and in a Solana public key.
pub const SEED_BYTES: usize = 32;

/// Bytes in an ed25519 signature.
pub const SIGNATURE_BYTES: usize = 64;

/// Largest keypair file accepted, in bytes. A 64-integer JSON array is under
/// 400 bytes; the cap stops a wrong path (a log, a core dump) being parsed.
pub const MAX_KEYFILE_BYTES: u64 = 4096;

/// Message used by the load-time self-test.
const SELF_TEST_MESSAGE: &[u8] = b"pump-quant signer self test";

/// Base58 alphabet. `0`, `O`, `I` and `l` are absent by design — they are the
/// characters misread when an address is compared by eye.
const B58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Why a signer could not be built or used.
///
/// No variant carries key material. Several deliberately carry *less* detail
/// than would be convenient, because an error string is a place secrets leak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignerError {
    /// The keypair file could not be read. Carries the path and the OS error,
    /// never any file content.
    Unreadable { path: String, cause: String },
    /// The file exceeded [`MAX_KEYFILE_BYTES`].
    TooLarge { path: String, bytes: u64 },
    /// The file did not parse as a 64-integer JSON array.
    Malformed { path: String, detail: String },
    /// `ring` rejected the seed/public-key pair — the file's two halves do not
    /// correspond, so it is corrupt or hand-edited.
    InconsistentKeypair { path: String },
    /// The file is a valid keypair, but not the wallet the caller expected.
    WrongWallet { expected: String, found: String },
    /// The expected address the caller supplied is not a valid base58 pubkey.
    BadExpectedAddress { given: String },
    /// The load-time sign/verify self-test failed.
    SelfTestFailed,
    /// The caller asked to sign something implausible as a transaction message.
    MessageRejected { bytes: usize, reason: &'static str },
}

impl fmt::Display for SignerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, cause } => {
                write!(f, "keypair file unreadable at {path}: {cause}")
            }
            Self::TooLarge { path, bytes } => write!(
                f,
                "keypair file at {path} is {bytes} bytes, over the {MAX_KEYFILE_BYTES} cap"
            ),
            Self::Malformed { path, detail } => {
                write!(f, "keypair file at {path} is malformed: {detail}")
            }
            Self::InconsistentKeypair { path } => write!(
                f,
                "keypair file at {path} is internally inconsistent: its public half \
                 does not derive from its secret half"
            ),
            Self::WrongWallet { expected, found } => write!(
                f,
                "keypair is for {found} but {expected} was expected; refusing to sign"
            ),
            Self::BadExpectedAddress { given } => {
                write!(f, "expected address {given} is not a valid base58 pubkey")
            }
            Self::SelfTestFailed => {
                write!(
                    f,
                    "signer self-test failed; the key cannot produce a verifiable signature"
                )
            }
            Self::MessageRejected { bytes, reason } => {
                write!(f, "refusing to sign a {bytes}-byte message: {reason}")
            }
        }
    }
}

/// Encode 32 bytes as a base58 Solana address.
///
/// Long division over a fixed buffer, so there is no big-integer dependency.
#[must_use]
pub fn encode_base58(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    // ceil(n * log(256)/log(58)) + 1; 45 covers 32 bytes with room to spare.
    let mut out = [0u8; 64];
    let mut out_len = 0usize;

    for &b in bytes {
        let mut carry = u32::from(b);
        for digit in out.iter_mut().take(out_len) {
            let v = (u32::from(*digit) << 8) + carry;
            *digit = (v % 58) as u8;
            carry = v / 58;
        }
        while carry > 0 {
            out[out_len] = (carry % 58) as u8;
            out_len += 1;
            carry /= 58;
        }
    }

    let leading_zeros = bytes.iter().take_while(|&&b| b == 0).count();
    let mut s = String::with_capacity(leading_zeros + out_len);
    for _ in 0..leading_zeros {
        s.push('1');
    }
    for i in (0..out_len).rev() {
        s.push(B58[out[i] as usize] as char);
    }
    s
}

/// Decode a base58 string into exactly 32 bytes.
///
/// Rejects a value whose leading-zero count does not match its leading `'1'`
/// count — which is what catches a truncated address, since a short value still
/// fits in 32 bytes.
#[must_use]
pub fn decode_base58_32(s: &str) -> Option<[u8; SEED_BYTES]> {
    if s.is_empty() {
        return None;
    }
    let mut out = [0u8; SEED_BYTES];
    for c in s.bytes() {
        let digit = B58.iter().position(|&a| a == c)? as u32;
        let mut carry = digit;
        for byte in out.iter_mut().rev() {
            let v = u32::from(*byte) * 58 + carry;
            *byte = (v & 0xff) as u8;
            carry = v >> 8;
        }
        if carry != 0 {
            return None;
        }
    }
    let leading_ones = s.bytes().take_while(|&c| c == b'1').count();
    let leading_zeros = out.iter().take_while(|&&b| b == 0).count();
    if leading_ones != leading_zeros {
        return None;
    }
    Some(out)
}

/// Parse a Solana CLI keypair file body: a JSON array of exactly 64 integers.
///
/// Hand-rolled rather than pulling in a JSON dependency, and deliberately strict
/// — anything that is not the expected shape is rejected rather than salvaged.
fn parse_cli_keypair(text: &str) -> Result<[u8; KEYPAIR_BYTES], String> {
    let body = text.trim();
    let inner = body
        .strip_prefix('[')
        .and_then(|b| b.strip_suffix(']'))
        .ok_or_else(|| "not a bracketed array".to_string())?;

    let mut out = [0u8; KEYPAIR_BYTES];
    let mut count = 0usize;
    for part in inner.split(',') {
        let t = part.trim();
        if t.is_empty() {
            return Err("empty element".to_string());
        }
        let v: u16 = t
            .parse()
            .map_err(|_| format!("element {count} is not an integer"))?;
        if v > 255 {
            return Err(format!("element {count} is {v}, outside a byte"));
        }
        if count >= KEYPAIR_BYTES {
            return Err(format!("more than {KEYPAIR_BYTES} elements"));
        }
        out[count] = v as u8;
        count += 1;
    }
    if count != KEYPAIR_BYTES {
        return Err(format!("{count} elements, expected {KEYPAIR_BYTES}"));
    }
    Ok(out)
}

/// An ed25519 signing key bound to one wallet address.
///
/// Deliberately not `Clone`: one loaded key, one owner, one place to reason
/// about its lifetime.
pub struct WalletSigner {
    keypair: Ed25519KeyPair,
    address: String,
}

/// Prints the public address and nothing else. The manual impl exists so that
/// `{:?}` on any struct containing a signer is safe by construction rather than
/// by everyone remembering.
impl fmt::Debug for WalletSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalletSigner")
            .field("address", &self.address)
            .field("secret", &"<never printed>")
            .finish()
    }
}

impl WalletSigner {
    /// Load a Solana CLI keypair file and bind it to `expected_address`.
    ///
    /// Fails closed on every discrepancy: unreadable, oversized, malformed,
    /// internally inconsistent, the wrong wallet, or unable to self-test.
    ///
    /// `expected_address` is not optional and has no default. The caller must
    /// state which wallet it believes it is loading, so that swapping the file
    /// changes behaviour from "signs with a different wallet" to "refuses to
    /// start".
    pub fn load_solana_keypair(path: &Path, expected_address: &str) -> Result<Self, SignerError> {
        let path_str = path.display().to_string();

        if decode_base58_32(expected_address).is_none() {
            return Err(SignerError::BadExpectedAddress {
                given: expected_address.to_string(),
            });
        }

        let meta = std::fs::metadata(path).map_err(|e| SignerError::Unreadable {
            path: path_str.clone(),
            cause: e.to_string(),
        })?;
        if meta.len() > MAX_KEYFILE_BYTES {
            return Err(SignerError::TooLarge {
                path: path_str,
                bytes: meta.len(),
            });
        }

        let text = std::fs::read_to_string(path).map_err(|e| SignerError::Unreadable {
            path: path_str.clone(),
            cause: e.to_string(),
        })?;

        let mut bytes = parse_cli_keypair(&text).map_err(|detail| SignerError::Malformed {
            path: path_str.clone(),
            detail,
        })?;

        let (seed, public) = bytes.split_at(SEED_BYTES);

        // ring cross-checks that `public` really derives from `seed`. A file
        // whose halves disagree is corrupt, and would otherwise produce
        // signatures every validator rejects.
        let keypair = Ed25519KeyPair::from_seed_and_public_key(seed, public).map_err(|_| {
            SignerError::InconsistentKeypair {
                path: path_str.clone(),
            }
        })?;

        let address = encode_base58(keypair.public_key().as_ref());

        // Overwrite the secret bytes we parsed. `ring` holds its own copy; this
        // one has no further purpose and should not linger.
        bytes.fill(0);

        if address != expected_address {
            return Err(SignerError::WrongWallet {
                expected: expected_address.to_string(),
                found: address,
            });
        }

        let signer = Self { keypair, address };
        signer.self_test()?;
        Ok(signer)
    }

    /// The public address this signer will sign for. Not a secret.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The 32-byte public key. Not a secret.
    #[must_use]
    pub fn public_key_bytes(&self) -> &[u8] {
        self.keypair.public_key().as_ref()
    }

    /// Sign `message`, returning the raw 64-byte ed25519 signature.
    ///
    /// The size check is a sanity rail, not validation: an empty message or one
    /// larger than a Solana packet is a caller bug, and signing it would produce
    /// a valid signature over nonsense.
    pub fn sign(&self, message: &[u8]) -> Result<[u8; SIGNATURE_BYTES], SignerError> {
        if message.is_empty() {
            return Err(SignerError::MessageRejected {
                bytes: 0,
                reason: "empty message",
            });
        }
        if message.len() > 1232 {
            return Err(SignerError::MessageRejected {
                bytes: message.len(),
                reason: "larger than the 1232-byte Solana packet limit",
            });
        }
        let sig = self.keypair.sign(message);
        let mut out = [0u8; SIGNATURE_BYTES];
        out.copy_from_slice(sig.as_ref());
        Ok(out)
    }

    /// Sign a fixed message and verify it against the public key.
    ///
    /// Run automatically at load. Exposed so a caller can re-run it — for
    /// instance before arming live capital — and confirm the signing path still
    /// works rather than assuming it does.
    pub fn self_test(&self) -> Result<(), SignerError> {
        let sig = self.sign(SELF_TEST_MESSAGE)?;
        verify_signature(self.public_key_bytes(), SELF_TEST_MESSAGE, &sig)
            .then_some(())
            .ok_or(SignerError::SelfTestFailed)
    }
}

/// Verify an ed25519 signature against a raw 32-byte public key.
///
/// Free function so callers can check a signature without holding a signer —
/// used by the self-test and available for post-submission auditing.
#[must_use]
pub fn verify_signature(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(message, signature)
        .is_ok()
}
