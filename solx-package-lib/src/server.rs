//! Resolve where solx-server is and how to authenticate to it, for a
//! Command-action package that needs to call it directly (persisting a
//! document, or anything else on the HTTP API) rather than going through
//! the console loopback above.
//!
//! solx sets `SOLX_SERVER_URL` and `SOLX_SERVER_TOKEN` on every Command
//! action it spawns (see `solx-actions/src/exec.rs`), and those are the
//! only supported source. The token is never read from `solx-config.json`:
//! solx keeps it in the OS credential manager, and a token baked into an
//! action's own config can't override the one solx issues per invocation.

use std::path::PathBuf;

/// Where solx-server is, and the bearer token to reach it.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub server_url: String,
    pub server_token: String,
}

impl ServerConfig {
    /// `SOLX_SERVER_URL` and `SOLX_SERVER_TOKEN`, both required.
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            server_url: required_env("SOLX_SERVER_URL")?,
            server_token: required_env("SOLX_SERVER_TOKEN")?,
        })
    }
}

fn required_env(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            format!(
                "{name} is required; solx sets it when it runs this action (run it                  through solx, or set it yourself when running by hand)"
            )
        })
}

/// `SOLX_APPDATA_DIR` env override → `%APPDATA%/praeus/solx` (Windows) →
/// `$HOME/.praeus/solx` → temp dir fallback. Mirrors
/// `solx-config::appdata_dir()` exactly (that crate is not a dependency
/// here — this crate stays dependency-light).
///
/// Used directly by `solx-git`, which reads its own
/// `solx-git.json` (an absolute-path allowlist) from the same directory
/// alongside `solx-config.json`.
pub fn appdata_dir() -> PathBuf {
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
    fn reads_url_and_token_from_the_environment() {
        clear_env();
        std::env::set_var("SOLX_SERVER_URL", "http://127.0.0.1:8766");
        std::env::set_var("SOLX_SERVER_TOKEN", " tok ");

        let cfg = ServerConfig::from_env().unwrap();
        assert_eq!(cfg.server_url, "http://127.0.0.1:8766");
        assert_eq!(cfg.server_token, "tok");
        clear_env();
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

    /// Neither the old `SOLX_TOKEN` alias nor a `server_token` in
    /// `solx-config.json` stands in for `SOLX_SERVER_TOKEN` any more.
    #[test]
    #[serial]
    fn only_solx_server_token_is_accepted() {
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("solx-config.json"), r#"{"server_token":"from-file"}"#).unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", dir.path());
        std::env::set_var("SOLX_SERVER_URL", "http://127.0.0.1:8766");
        std::env::set_var("SOLX_TOKEN", "generic-tok");

        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.contains("SOLX_SERVER_TOKEN"), "{err}");
        clear_env();
    }
}
