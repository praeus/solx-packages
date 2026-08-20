//! Resolve where solx-server is and how to authenticate to it, for a
//! Command-action package that needs to call it directly (persisting a
//! document, or anything else on the HTTP API) rather than going through
//! the console loopback above.
//!
//! `server_url` must be set explicitly — there is no default, because a
//! package that needs this should say so via its own action's
//! `action_config.env` (`run_command` forwards it to the child; see
//! `solx-actions/src/exec.rs`), the same way `solx-quickjs`'s `install.solx`
//! already does. `server_token` is different: it must never be baked into
//! `install.solx` (a real secret sitting in a file on disk, with no
//! rotation story), so it is resolved from the environment first and falls
//! back to reading `server_token` straight out of `solx-config.json` — the
//! same file `solx-server` writes it to on first run. That fallback is what
//! lets a package reach the server with zero action-side configuration
//! beyond the URL.

use std::path::PathBuf;

use serde_json::Value;

/// Where solx-server is, and the bearer token to reach it.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub server_url: String,
    pub server_token: String,
}

impl ServerConfig {
    /// `SOLX_SERVER_URL` (required, no default) and `SOLX_SERVER_TOKEN` →
    /// `SOLX_TOKEN` → `solx-config.json`'s `server_token` (required, in that
    /// order).
    pub fn from_env() -> Result<Self, String> {
        let server_url = std::env::var("SOLX_SERVER_URL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| "SOLX_SERVER_URL is required".to_string())?;

        let server_token = std::env::var("SOLX_SERVER_TOKEN")
            .ok()
            .or_else(|| std::env::var("SOLX_TOKEN").ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .or_else(Self::token_from_config_file)
            .ok_or_else(|| {
                "SOLX_SERVER_TOKEN (or SOLX_TOKEN) is required; set it on the \
                 action's action_config.env, or ensure solx-config.json has a \
                 server_token (start solx-server once to generate it)"
                    .to_string()
            })?;

        Ok(Self { server_url, server_token })
    }

    /// Fallback: read `server_token` from `solx-config.json` in the default
    /// appdata dir. `solx-server` generates it on first run, so a freshly
    /// installed action can reach the server without a separate bootstrap
    /// step that bakes the token into `action_config.env`.
    fn token_from_config_file() -> Option<String> {
        let text = std::fs::read_to_string(appdata_dir().join("solx-config.json")).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        value.get("server_token").and_then(Value::as_str).map(str::to_string)
    }
}

/// `SOLX_APPDATA_DIR` env override → `%APPDATA%/praeus/solx` (Windows) →
/// `$HOME/.praeus/solx` → temp dir fallback. Mirrors
/// `solx-config::appdata_dir()` exactly (that crate is not a dependency
/// here — this crate stays dependency-light — but a Command child and the
/// solx-core process that spawned it must resolve to the same directory for
/// the config-file fallback above to find anything).
fn appdata_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SOLX_APPDATA_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if !appdata.trim().is_empty() {
                return PathBuf::from(appdata).join("praeus").join("solx");
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join(".praeus").join("solx");
        }
    }
    std::env::temp_dir().join("praeus").join("solx")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn clear_env() {
        std::env::remove_var("SOLX_SERVER_URL");
        std::env::remove_var("SOLX_SERVER_TOKEN");
        std::env::remove_var("SOLX_TOKEN");
        std::env::remove_var("SOLX_APPDATA_DIR");
    }

    #[test]
    #[serial]
    fn missing_server_url_is_an_error() {
        clear_env();
        std::env::set_var("SOLX_SERVER_TOKEN", "tok");
        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.contains("SOLX_SERVER_URL"), "{err}");
        clear_env();
    }

    #[test]
    #[serial]
    fn env_token_wins_over_config_file() {
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("solx-config.json"), r#"{"server_token":"from-file"}"#).unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", dir.path());
        std::env::set_var("SOLX_SERVER_URL", "http://127.0.0.1:8766");
        std::env::set_var("SOLX_SERVER_TOKEN", "from-env");

        let cfg = ServerConfig::from_env().unwrap();
        assert_eq!(cfg.server_url, "http://127.0.0.1:8766");
        assert_eq!(cfg.server_token, "from-env");
        clear_env();
    }

    #[test]
    #[serial]
    fn solx_token_is_a_fallback_for_solx_server_token() {
        clear_env();
        std::env::set_var("SOLX_SERVER_URL", "http://127.0.0.1:8766");
        std::env::set_var("SOLX_TOKEN", "generic-tok");

        let cfg = ServerConfig::from_env().unwrap();
        assert_eq!(cfg.server_token, "generic-tok");
        clear_env();
    }

    #[test]
    #[serial]
    fn falls_back_to_reading_the_token_from_the_config_file() {
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("solx-config.json"), r#"{"server_token":"from-file"}"#).unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", dir.path());
        std::env::set_var("SOLX_SERVER_URL", "http://127.0.0.1:8766");

        let cfg = ServerConfig::from_env().unwrap();
        assert_eq!(cfg.server_token, "from-file");
        clear_env();
    }

    #[test]
    #[serial]
    fn missing_token_everywhere_is_a_clear_error() {
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", dir.path());
        std::env::set_var("SOLX_SERVER_URL", "http://127.0.0.1:8766");

        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.contains("SOLX_SERVER_TOKEN"), "{err}");
        clear_env();
    }

    #[test]
    #[serial]
    fn a_malformed_config_file_is_treated_as_absent_not_a_panic() {
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("solx-config.json"), "not json").unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", dir.path());
        std::env::set_var("SOLX_SERVER_URL", "http://127.0.0.1:8766");

        assert!(ServerConfig::from_env().is_err());
        clear_env();
    }
}
