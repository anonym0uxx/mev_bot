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
    /// Helius WebSocket base URL (e.g. `wss://marielle-qe2lvr-fast-mainnet.helius-rpc.com`).
    /// This is an ENDPOINT-REFERENCE, not a credential. The API key is appended
    /// at the call site so this value never contains key material.
    pub helius_ws_base: String,
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
            // Normalise MSYS/git-bash paths (e.g. `/c/Users/...`) to native
            // Windows paths (`C:\Users\...`) so the file is reachable from a
            // Windows binary launched inside a git-bash shell. On non-Windows
            // hosts this is a no-op.
            let p = normalise_path(&p);
            dotenvy::from_path(Path::new(&p))
                .with_context(|| format!("PQ_CREDS_FILE={p} could not be loaded"))?;
        }
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Pure credential lookup from an explicit map. Touches no global
    /// state. This is what tests exercise to avoid racing on process env.
    pub fn from_lookup<F: Fn(&str) -> Option<String>>(get: F) -> Result<Self> {
        let req = |k: &str| -> Result<String> {
            get(k)
                .filter(|v| !v.trim().is_empty())
                .with_context(|| format!("{k} not set; refusing to start"))
        };
        Ok(Self {
            helius_api_key: Secret::new(req("HELIUS_API_KEY")?),
            laserstream_endpoint: req("LASERSTREAM_ENDPOINT")?,
            helius_ws_base: req("HELIUS_WS_URL")?,
        })
    }

    /// Build the real Helius RPC URL at the call site, never store it as `String`.
    /// The returned `Secret` means it cannot be accidentally logged.
    pub fn rpc_url(&self) -> Secret {
        Secret::new(format!(
            "{}/?api-key={}",
            self.helius_ws_base.replacen("wss://", "https://", 1),
            self.helius_api_key.expose()
        ))
    }

    /// Build the real Helius WS URL at the call site.
    pub fn ws_url(&self) -> Secret {
        Secret::new(format!(
            "{}/?api-key={}",
            self.helius_ws_base,
            self.helius_api_key.expose()
        ))
    }

    /// The ONLY form that may be logged. Contains no key material.
    pub fn rpc_url_redacted(&self) -> String {
        format!(
            "{}/?api-key=<redacted>",
            self.helius_ws_base.replacen("wss://", "https://", 1)
        )
    }

    /// The ONLY form that may be logged. Contains no key material.
    pub fn ws_url_redacted(&self) -> String {
        format!("{}/?api-key=<redacted>", self.helius_ws_base)
    }
}

/// Normalise an MSYS/git-bash path (`/c/Users/...`) to a native Windows path
/// (`C:\Users\...`) so that a Windows binary launched inside a git-bash shell
/// can find a file referenced by `PQ_CREDS_FILE`. On non-Windows hosts this
/// is a no-op (returns the input unchanged). This is a defensive convenience
/// — the primary fix is setting `PQ_CREDS_FILE` to a native path at User
/// scope.
fn normalise_path(p: &str) -> String {
    #[cfg(windows)]
    {
        // MSYS mounts `/c` at `C:\`, `/d` at `D:\`, etc. The pattern is
        // `/<single-letter>/rest`.
        let bytes = p.as_bytes();
        if bytes.len() >= 2 && bytes[0] == b'/' {
            let drive = bytes[1] as char;
            if drive.is_ascii_alphabetic() && (bytes.len() == 2 || bytes[2] == b'/') {
                let rest = &p[2..];
                let native = format!("{}:{}", drive.to_ascii_uppercase(), rest.replace('/', "\\"));
                return native;
            }
        }
        p.to_string()
    }
    #[cfg(not(windows))]
    p.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::Mutex;

    /// Lock to serialize the single test that touches real process env.
    /// All other tests use `from_lookup` with an explicit map.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Helper: build an explicit lookup map for `from_lookup`.
    fn make_lookup(
        helius: Option<&str>,
        laserstream: Option<&str>,
        ws_base: Option<&str>,
    ) -> impl Fn(&str) -> Option<String> {
        let mut m: HashMap<String, String> = HashMap::new();
        if let Some(v) = helius {
            m.insert("HELIUS_API_KEY".to_string(), v.to_string());
        }
        if let Some(v) = laserstream {
            m.insert("LASERSTREAM_ENDPOINT".to_string(), v.to_string());
        }
        if let Some(v) = ws_base {
            m.insert("HELIUS_WS_URL".to_string(), v.to_string());
        }
        move |k: &str| m.get(k).cloned()
    }

    // ── Positive controls via from_lookup (no process env, no race) ──

    /// Positive control: from_lookup fails when HELIUS_API_KEY is absent.
    #[test]
    fn from_lookup_fails_when_helius_unset() {
        let lookup = make_lookup(None, Some("https://laserstream-mainnet-slc.helius-rpc.com"), Some("wss://marielle-qe2lvr-fast-mainnet.helius-rpc.com"));
        let result = Creds::from_lookup(lookup);
        assert!(result.is_err());
        let err_msg = format!("{}", result.err().unwrap());
        assert!(
            err_msg.contains("HELIUS_API_KEY"),
            "error must name the missing var: {err_msg}"
        );
        assert!(
            err_msg.contains("refusing to start"),
            "error must be fail-closed: {err_msg}"
        );
    }

    /// Positive control: from_lookup fails when LASERSTREAM_ENDPOINT is absent.
    #[test]
    fn from_lookup_fails_when_laserstream_unset() {
        let lookup = make_lookup(Some("fake-key-1234"), None, Some("wss://marielle-qe2lvr-fast-mainnet.helius-rpc.com"));
        let result = Creds::from_lookup(lookup);
        assert!(result.is_err());
        let err_msg = format!("{}", result.err().unwrap());
        assert!(
            err_msg.contains("LASERSTREAM_ENDPOINT"),
            "error must name the missing var: {err_msg}"
        );
    }

    /// Positive control: from_lookup fails when HELIUS_WS_URL is absent.
    #[test]
    fn from_lookup_fails_when_ws_url_unset() {
        let lookup = make_lookup(Some("fake-key-1234"), Some("https://laserstream-mainnet-slc.helius-rpc.com"), None);
        let result = Creds::from_lookup(lookup);
        assert!(result.is_err());
        let err_msg = format!("{}", result.err().unwrap());
        assert!(
            err_msg.contains("HELIUS_WS_URL"),
            "error must name the missing var: {err_msg}"
        );
    }

    /// Positive control: from_lookup fails when HELIUS_API_KEY is empty.
    #[test]
    fn from_lookup_fails_when_helius_empty() {
        let lookup = make_lookup(Some("   "), Some("https://laserstream-mainnet-slc.helius-rpc.com"), Some("wss://marielle-qe2lvr-fast-mainnet.helius-rpc.com"));
        let result = Creds::from_lookup(lookup);
        assert!(result.is_err(), "from_lookup must fail when HELIUS_API_KEY is whitespace-only");
        let err_msg = format!("{}", result.err().unwrap());
        assert!(err_msg.contains("HELIUS_API_KEY"));
    }

    /// Positive control: from_lookup succeeds when both vars are present.
    #[test]
    fn from_lookup_succeeds_when_both_set() {
        let lookup = make_lookup(Some("fake-key-1234"), Some("https://laserstream-mainnet-slc.helius-rpc.com"), Some("wss://marielle-qe2lvr-fast-mainnet.helius-rpc.com"));
        let creds = Creds::from_lookup(lookup).expect("both vars set must succeed");
        assert_eq!(creds.helius_api_key.expose(), "fake-key-1234");
        assert_eq!(creds.laserstream_endpoint, "https://laserstream-mainnet-slc.helius-rpc.com");
        assert_eq!(creds.helius_ws_base, "wss://marielle-qe2lvr-fast-mainnet.helius-rpc.com");
    }

    /// Positive control: the Debug output of Creds must NOT contain the key value.
    /// Uses from_lookup — no process env mutation.
    #[test]
    fn debug_does_not_leak_key() {
        let lookup = make_lookup(
            Some("test-key-not-a-uuid-shape"),
            Some("https://laserstream-mainnet-slc.helius-rpc.com"),
            Some("wss://marielle-qe2lvr-fast-mainnet.helius-rpc.com"),
        );
        let creds = Creds::from_lookup(lookup).unwrap();
        let dbg = format!("{:?}", creds);
        assert!(
            !dbg.contains("test-key-not-a-uuid-shape"),
            "Creds Debug leaked the key! Debug output: {dbg}"
        );
        assert!(dbg.contains("Secret(<redacted>)"));
    }

    /// rpc_url and ws_url must embed the key but redacted forms must not.
    /// Uses from_lookup — no process env mutation.
    #[test]
    fn rpc_url_is_secret() {
        let lookup = make_lookup(
            Some("test-key-1234"),
            Some("https://laserstream-mainnet-slc.helius-rpc.com"),
            Some("wss://marielle-qe2lvr-fast-mainnet.helius-rpc.com"),
        );
        let creds = Creds::from_lookup(lookup).unwrap();
        let url = creds.rpc_url();
        assert!(url.expose().contains("api-key=test-key-1234"));
        assert!(!creds.rpc_url_redacted().contains("test-key-1234"));
        let ws = creds.ws_url();
        assert!(ws.expose().contains("api-key=test-key-1234"));
        assert!(!creds.ws_url_redacted().contains("test-key-1234"));
    }

    // ── from_env tests (serialized behind ENV_LOCK) ──

    /// The ONE test that exercises from_env through real process env.
    /// Serialized behind ENV_LOCK to prevent races with other from_env tests.
    #[test]
    fn from_env_fails_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Save and remove any existing PQ_CREDS_FILE so from_env goes straight
        // to bare env. Then ensure HELIUS_API_KEY is unset.
        let saved_pq = std::env::var_os("PQ_CREDS_FILE");
        std::env::remove_var("PQ_CREDS_FILE");
        let saved_helius = std::env::var_os("HELIUS_API_KEY");
        std::env::remove_var("HELIUS_API_KEY");
        let saved_ws = std::env::var_os("HELIUS_WS_URL");
        std::env::remove_var("HELIUS_WS_URL");

        let result = Creds::from_env();
        assert!(
            result.is_err(),
            "from_env must fail when HELIUS_API_KEY is unset"
        );
        if let Err(e) = &result {
            let err_msg = format!("{e}");
            assert!(
                err_msg.contains("HELIUS_API_KEY"),
                "error must name the missing var: {err_msg}"
            );
            assert!(
                err_msg.contains("refusing to start"),
                "error must be fail-closed: {err_msg}"
            );
        }

        // Restore env.
        if let Some(v) = saved_helius {
            std::env::set_var("HELIUS_API_KEY", v);
        }
        if let Some(v) = saved_ws {
            std::env::set_var("HELIUS_WS_URL", v);
        }
        if let Some(v) = saved_pq {
            std::env::set_var("PQ_CREDS_FILE", v);
        }
    }

    /// PQ_CREDS_FILE points at a nonexistent path; from_env() returns Err
    /// naming the path. Fail-closed: no fallback to bare process env.
    /// Serialized behind ENV_LOCK.
    #[test]
    fn creds_file_missing_is_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        let bogus = std::env::temp_dir().join(format!(
            "pq-creds-nonexistent-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&bogus);

        let saved_pq = std::env::var_os("PQ_CREDS_FILE");
        std::env::set_var("PQ_CREDS_FILE", &bogus);

        let result = Creds::from_env();

        // Restore env before asserting.
        match &saved_pq {
            Some(v) => std::env::set_var("PQ_CREDS_FILE", v),
            None => std::env::remove_var("PQ_CREDS_FILE"),
        }

        assert!(
            result.is_err(),
            "from_env must fail when PQ_CREDS_FILE is unreadable"
        );
        let err_msg = format!("{}", result.err().unwrap());
        assert!(
            err_msg.contains(&*bogus.to_string_lossy()),
            "error must name the missing path: {err_msg}"
        );
    }

    /// Load a known fake key from a temp creds file; assert that
    /// format!("{:?}", creds) does NOT contain the fake value.
    /// This is the positive control for the file-loading path — the one
    /// the real credential will take. Serialized behind ENV_LOCK.
    #[test]
    fn loaded_key_never_in_debug() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir();
        let creds_path = dir.join(format!("pq-test-creds-{}.env", std::process::id()));
        {
            let mut f = std::fs::File::create(&creds_path).unwrap();
            writeln!(f, "HELIUS_API_KEY=fake-key-for-loading-test").unwrap();
            writeln!(f, "LASERSTREAM_ENDPOINT=https://laserstream-mainnet-slc.helius-rpc.com").unwrap();
            writeln!(f, "HELIUS_WS_URL=wss://marielle-qe2lvr-fast-mainnet.helius-rpc.com").unwrap();
        }

        let saved_pq = std::env::var_os("PQ_CREDS_FILE");
        std::env::set_var("PQ_CREDS_FILE", &creds_path);

        let result = Creds::from_env();

        // Clean up and restore env regardless of outcome.
        let _ = std::fs::remove_file(&creds_path);
        match &saved_pq {
            Some(v) => std::env::set_var("PQ_CREDS_FILE", v),
            None => std::env::remove_var("PQ_CREDS_FILE"),
        }

        let creds = result.expect("from_env must succeed with a valid creds file");
        let dbg = format!("{:?}", creds);
        assert!(
            !dbg.contains("fake-key-for-loading-test"),
            "Creds Debug leaked the key loaded from file! Debug output: {dbg}"
        );
        assert!(dbg.contains("Secret(<redacted>)"));
    }

    #[test]
    fn normalise_path_msys_to_native() {
        // MSYS `/c/Users/...` → `C:\Users\...` on Windows; unchanged elsewhere.
        let inp = "/c/Users/Alon/.hermes/creds/pump-quant.env";
        let out = normalise_path(inp);
        #[cfg(windows)]
        assert_eq!(out, "C:\\Users\\Alon\\.hermes\\creds\\pump-quant.env");
        #[cfg(not(windows))]
        assert_eq!(out, inp);
    }

    #[test]
    fn normalise_path_already_native() {
        // A native Windows path is left unchanged.
        let inp = "C:\\Users\\Alon\\creds.env";
        let out = normalise_path(inp);
        assert_eq!(out, inp);
    }
}
