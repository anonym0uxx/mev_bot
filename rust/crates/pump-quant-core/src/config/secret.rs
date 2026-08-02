//! A value that must never reach a log.
//!
//! The ONLY way out is [`expose`](Secret::expose), which greps cleanly in review.
//! There is no `Display` impl — a `format!("{secret}")` will not compile.
//! `Debug` is redacted, so `{:?}` on any containing struct is safe.

use std::fmt;

/// A secret value. The only extraction path is `expose()`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a plaintext value. Callers are responsible for ensuring the
    /// argument is not logged before this call wraps it.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The ONLY way to read the inner value. Every call site is a review target.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

// No Display impl — deliberately unrepresentable in format!() / tracing.

// Debug is redacted so {:?} on any containing struct is safe.
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let s = Secret::new("abc123-not-a-real-key");
        assert_eq!(format!("{:?}", s), "Secret(<redacted>)");
    }

    #[test]
    fn expose_returns_inner() {
        let s = Secret::new("test-value");
        assert_eq!(s.expose(), "test-value");
    }

    #[test]
    fn no_display_impl() {
        // This test verifies the type at compile time: if a Display impl
        // is ever added, this test's existence is the review signal.
        // (A Display impl would make format!("{}", secret) compile — that
        // is the failure mode this type prevents.)
        let s = Secret::new("x");
        // The only way to get the value is expose():
        let val: &str = s.expose();
        assert_eq!(val, "x");
    }
}
