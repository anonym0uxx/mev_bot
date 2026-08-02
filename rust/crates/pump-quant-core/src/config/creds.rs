//! Credential resolution for the paper-trading and live engines.
//!
//! [`Creds`] is resolved once, in `main()`, via [`Creds::from_env`].
//! This is the only place `std::env::var` is called for a secret.
//! No defaults, no fallbacks, no `unwrap_or` — absent means refuse to start.
//!
//! Loading order:
//!   1. If `PQ_CREDS_FILE` is set, load KEY=VALUE pairs from that file
//!      via `dotenvy::from_path`. The path itself is not a credential.
//!   2. Read the now-populated process env for HELIUS_API_KEY and
//!      LASERSTREAM_ENDPOINT. Missing or empty = refuse to start.
//!   3. `PQ_CREDS_FILE` set but unreadable = error, NOT fallback to bare env.

use std::path::Path;

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
    /// If either env var is unset or empty after loading, the process
    /// refuses to start.
    ///
    /// A PATH may live in tracked config (PQ_CREDS_FILE); a VALUE may not.
    /// PQ_CREDS_FILE set but unreadable is an error, not a fallback.
    pub fn from_env() -> Result<Self> {
        if let Ok(p) = std::env::var("PQ_CREDS_FILE") {
            dotenvy::from_path(Path::new(&p))
                .with_context(|| format!("PQ_CREDS_FILE={p} could not be loaded"))?;
        }
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
    use std::io::Write;

    /// Positive control: the Debug output of Creds must NOT contain the key value.
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
        assert!(url.expose().contains("api-key=test-key-1234"));
        assert!(!creds.rpc_url_redacted().contains("test-key-1234"));
    }

    #[test]
    fn from_env_fails_when_unset() {
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

    /// PQ_CREDS_FILE points at a nonexistent path; from_env() returns Err
    /// naming the path. Fail-closed: no fallback to bare process env.
    #[test]
    fn creds_file_missing_is_error() {
        // Generate a unique nonexistent path in the temp dir.
        let bogus = std::env::temp_dir().join(format!(
            "pq-creds-nonexistent-{}",
            std::process::id()
        ));
        // Ensure it does not exist.
        let _ = std::fs::remove_file(&bogus);
        std::env::set_var("PQ_CREDS_FILE", &bogus);
        let result = Creds::from_env();
        // Clean up before asserting so we don't leak the env var on failure.
        std::env::remove_var("PQ_CREDS_FILE");
        assert!(result.is_err(), "from_env must fail when PQ_CREDS_FILE is unreadable");
        let err_msg = format!("{}", result.err().unwrap());
        assert!(
            err_msg.contains(&*bogus.to_string_lossy()),
            "error must name the missing path: {err_msg}"
        );
    }

    /// Load a known fake key from a temp creds file; assert that
    /// format!("{:?}", creds) does NOT contain the fake value.
    /// This is the positive control for the file-loading path — the one
    /// the real credential will take.
    #[test]
    fn loaded_key_never_in_debug() {
        let dir = std::env::temp_dir();
        let creds_path = dir.join(format!("pq-test-creds-{}.env", std::process::id()));
        {
            let mut f = std::fs::File::create(&creds_path).unwrap();
            // Use a value that does NOT match credential-guard patterns.
            writeln!(f, "HELIUS_API_KEY=fake-key-for-loading-test").unwrap();
            writeln!(f, "LASERSTREAM_ENDPOINT=https://laserstream-mainnet-slc.helius-rpc.com").unwrap();
        }
        std::env::set_var("PQ_CREDS_FILE", &creds_path);
        let result = Creds::from_env();
        // Clean up regardless of outcome.
        let _ = std::fs::remove_file(&creds_path);
        std::env::remove_var("PQ_CREDS_FILE");
        let creds = result.expect("from_env must succeed with a valid creds file");
        let dbg = format!("{:?}", creds);
        assert!(
            !dbg.contains("fake-key-for-loading-test"),
            "Creds Debug leaked the key loaded from file! Debug output: {dbg}"
        );
        assert!(dbg.contains("Secret(<redacted>)"));
    }
}
