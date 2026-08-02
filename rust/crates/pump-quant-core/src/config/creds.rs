//! Credential resolution for the paper-trading and live engines.
//!
//! [`Creds`] is resolved once, in `main()`, via [`Creds::from_env`].
//! This is the only place `std::env::var` is called for a secret.
//! No defaults, no fallbacks, no `unwrap_or` — absent means refuse to start.

use anyhow::{bail, Context, Result};

use super::secret::Secret;

/// Resolved credentials. The only extraction path for secrets is
/// `helius_api_key.expose()`. URLs containing the key are themselves
/// `Secret` — never `String`.
#[derive(Clone, Debug)]
pub struct Creds {
    /// Helius API key (UUID-format, 36 chars, 8-4-4-4-12).
    pub helius_api_key: Secret,
    /// LaserStream endpoint hostname (not a secret — no key embedded).
    pub laserstream_endpoint: String,
}

impl Creds {
    /// Fail-closed. No defaults, no fallbacks, no `unwrap_or`.
    /// If either env var is unset or empty, the process refuses to start.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            helius_api_key: Secret::new(req("HELIUS_API_KEY")?),
            laserstream_endpoint: req("LASERSTREAM_ENDPOINT")?,
        })
    }

    /// Build the real Helius RPC URL at the call site, never store it as `String`.
    /// The returned `Secret` means it cannot be accidentally logged.
    pub fn rpc_url(&self) -> Secret {
        Secret::new(format!(
            "https://mainnet.helius-rpc.com/?api-key={}",
            self.helius_api_key.expose()
        ))
    }

    /// Build the real Helius WS URL at the call site.
    pub fn ws_url(&self) -> Secret {
        Secret::new(format!(
            "wss://mainnet.helius-rpc.com/?api-key={}",
            self.helius_api_key.expose()
        ))
    }

    /// The ONLY form that may be logged. Contains no key material.
    pub fn rpc_url_redacted(&self) -> String {
        "https://mainnet.helius-rpc.com/?api-key=<redacted>".to_string()
    }

    /// The ONLY form that may be logged. Contains no key material.
    pub fn ws_url_redacted(&self) -> String {
        "wss://mainnet.helius-rpc.com/?api-key=<redacted>".to_string()
    }
}

/// Require an env var. Fail-closed: absent or empty means refuse to start.
fn req(name: &str) -> Result<String> {
    let v = std::env::var(name)
        .with_context(|| format!("{name} not set; refusing to start"))?;
    if v.trim().is_empty() {
        bail!("{name} is empty; refusing to start");
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Positive control: the Debug output of Creds must NOT contain the key value.
    /// This test is the positive control for the redaction design.
    #[test]
    fn debug_does_not_leak_key() {
        let creds = Creds {
            helius_api_key: Secret::new("test-key-not-a-uuid-shape"),
            laserstream_endpoint: "https://laserstream-mainnet-slc.helius-rpc.com".to_string(),
        };
        let dbg = format!("{:?}", creds);
        assert!(
            !dbg.contains("test-key-not-a-uuid-shape"),
            "Creds Debug leaked the key! Debug output: {dbg}"
        );
        assert!(dbg.contains("Secret(<redacted>)"));
    }

    #[test]
    fn rpc_url_is_secret() {
        let creds = Creds {
            helius_api_key: Secret::new("test-key-1234"),
            laserstream_endpoint: "https://laserstream-mainnet-slc.helius-rpc.com".to_string(),
        };
        let url = creds.rpc_url();
        // url is Secret, not String — can only read via expose()
        assert!(url.expose().contains("api-key=test-key-1234"));
        // Redacted form must NOT contain the key
        assert!(!creds.rpc_url_redacted().contains("test-key-1234"));
    }

    #[test]
    fn from_env_fails_when_unset() {
        // HELIUS_API_KEY is not set on this box — from_env must fail.
        // (If it happens to be set in the test environment, skip.)
        if std::env::var("HELIUS_API_KEY").is_ok() {
            eprintln!("HELIUS_API_KEY is set in test env — skipping fail-closed test");
            return;
        }
        let result = Creds::from_env();
        assert!(result.is_err(), "from_env must fail when HELIUS_API_KEY is unset");
        let err_msg = format!("{}", result.err().unwrap());
        assert!(err_msg.contains("HELIUS_API_KEY"), "error must name the missing var: {err_msg}");
        assert!(err_msg.contains("refusing to start"), "error must be fail-closed: {err_msg}");
    }
}
