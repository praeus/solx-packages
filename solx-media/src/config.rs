//! Environment-driven configuration for the solx-media binary.
//!
//! Reads on every invocation; nothing is persisted by this module itself.
//! `whisper_models::install` writes back to `{SOLX_PACKAGES_DIR}/solx-media/config.json`
//! to remember the active whisper model between runs.

use std::path::PathBuf;

/// Default Ollama base URL (`POST /api/generate`).
const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
/// Default Ollama model for image / frame descriptions (a vision-capable model).
const DEFAULT_MULTIMEDIA_MODEL: &str = "llava";
/// Default Ollama model for synthesis (audio/video summarization).
const DEFAULT_SUMMARIZER_MODEL: &str = "llama3.1";

#[derive(Debug, Clone)]
pub struct MediaConfig {
    pub ollama_url: String,
    pub multimedia_model: String,
    pub summarizer_model: String,
    pub whisper_model_path: Option<PathBuf>,
    pub whisper_models_dir: PathBuf,
    pub server_url: String,
    pub server_token: String,
    pub packages_dir: PathBuf,
}

impl MediaConfig {
    /// Read every env var, applying defaults. Returns the first missing
    /// required value as `Err`. Required: `SOLX_SERVER_URL`, `SOLX_SERVER_TOKEN`
    /// (or `SOLX_TOKEN`). Everything else has a default or is optional.
    pub fn from_env() -> Result<Self, String> {
        let ollama_url = env_or_default("OLLAMA_URL", DEFAULT_OLLAMA_URL);
        let multimedia_model = env_or_default("MULTIMEDIA_MODEL", DEFAULT_MULTIMEDIA_MODEL);
        let summarizer_model = env_or_default("SUMMARIZER_MODEL", DEFAULT_SUMMARIZER_MODEL);

        let whisper_model_path = std::env::var("WHISPER_MODEL_PATH")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);

        let packages_dir = match std::env::var("SOLX_PACKAGES_DIR").ok() {
            Some(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
            _ => solx_appdata_dir().join("packages"),
        };

        let whisper_models_dir = std::env::var("WHISPER_MODELS_DIR")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| packages_dir.join("solx-media").join("models"));

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
            .ok_or_else(|| "SOLX_SERVER_TOKEN (or SOLX_TOKEN) is required".to_string())?;

        Ok(Self {
            ollama_url,
            multimedia_model,
            summarizer_model,
            whisper_model_path,
            whisper_models_dir,
            server_url,
            server_token,
            packages_dir,
        })
    }

    /// Path to `{SOLX_PACKAGES_DIR}/solx-media/config.json`. Always derivable
    /// from `packages_dir`; never errors.
    pub fn media_config_path(&self) -> PathBuf {
        self.packages_dir.join("solx-media").join("config.json")
    }
}

fn env_or_default(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Resolve `{SOLX_APPDATA_DIR}` for the current OS. Mirrors solx-core's
/// default-appdata-dir convention: `%APPDATA%/praeus/solx` on Windows,
/// `$XDG_DATA_HOME/praeus/solx` (or `~/.local/share/praeus/solx`) on POSIX.
fn solx_appdata_dir() -> PathBuf {
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if !appdata.trim().is_empty() {
                return PathBuf::from(appdata).join("praeus").join("solx");
            }
        }
    } else {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.trim().is_empty() {
                return PathBuf::from(xdg).join("praeus").join("solx");
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            if !home.trim().is_empty() {
                return PathBuf::from(home).join(".local").join("share").join("praeus").join("solx");
            }
        }
    }
    PathBuf::from(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests in this module all mutate process-global env vars. Run them
    // serially so concurrent tests don't see each other's writes.
    use serial_test::serial;

    /// Helper: clear every env var `from_env` reads so tests are isolated.
    fn clear_env() {
        for k in [
            "OLLAMA_URL",
            "MULTIMEDIA_MODEL",
            "SUMMARIZER_MODEL",
            "WHISPER_MODEL_PATH",
            "WHISPER_MODELS_DIR",
            "SOLX_SERVER_URL",
            "SOLX_SERVER_TOKEN",
            "SOLX_TOKEN",
            "SOL_LOG_DIR",
            "SOLX_PACKAGES_DIR",
            "APPDATA",
            "XDG_DATA_HOME",
            "HOME",
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    #[serial]
    fn from_env_requires_server_url_and_token() {
        clear_env();
        std::env::set_var("SOLX_SERVER_URL", "http://localhost:8766");
        // No token -> Err
        assert!(MediaConfig::from_env().is_err());

        std::env::set_var("SOLX_SERVER_TOKEN", "secret");
        let cfg = MediaConfig::from_env().expect("ok with URL + token");
        assert_eq!(cfg.server_url, "http://localhost:8766");
        assert_eq!(cfg.server_token, "secret");
        assert_eq!(cfg.ollama_url, DEFAULT_OLLAMA_URL);
        assert_eq!(cfg.multimedia_model, DEFAULT_MULTIMEDIA_MODEL);
        assert_eq!(cfg.summarizer_model, DEFAULT_SUMMARIZER_MODEL);
        assert!(cfg.whisper_model_path.is_none());
    }

    #[test]
    #[serial]
    fn from_env_honors_overrides() {
        clear_env();
        std::env::set_var("SOLX_SERVER_URL", "http://x:1");
        std::env::set_var("SOLX_SERVER_TOKEN", "t");
        std::env::set_var("OLLAMA_URL", "http://y:2");
        std::env::set_var("MULTIMEDIA_MODEL", "bakllava");
        std::env::set_var("SUMMARIZER_MODEL", "mistral");
        std::env::set_var("WHISPER_MODEL_PATH", "C:/models/tiny.bin");
        std::env::set_var("SOLX_PACKAGES_DIR", "C:/pkg");
        let cfg = MediaConfig::from_env().unwrap();
        assert_eq!(cfg.ollama_url, "http://y:2");
        assert_eq!(cfg.multimedia_model, "bakllava");
        assert_eq!(cfg.summarizer_model, "mistral");
        assert_eq!(cfg.whisper_model_path, Some(PathBuf::from("C:/models/tiny.bin")));
        assert_eq!(cfg.packages_dir, PathBuf::from("C:/pkg"));
        assert_eq!(
            cfg.whisper_models_dir,
            PathBuf::from("C:/pkg").join("solx-media").join("models")
        );
    }

    #[serial]
    #[test]
    fn solx_token_alias_works() {
        clear_env();
        std::env::set_var("SOLX_SERVER_URL", "http://x:1");
        std::env::set_var("SOLX_TOKEN", "alt-token");
        let cfg = MediaConfig::from_env().unwrap();
        assert_eq!(cfg.server_token, "alt-token");
    }

    #[serial]
    #[test]
    fn media_config_path_under_packages_dir() {
        clear_env();
        std::env::set_var("SOLX_SERVER_URL", "http://x");
        std::env::set_var("SOLX_SERVER_TOKEN", "t");
        std::env::set_var("SOLX_PACKAGES_DIR", "/var/solx-pkg");
        let cfg = MediaConfig::from_env().unwrap();
        assert_eq!(
            cfg.media_config_path(),
            PathBuf::from("/var/solx-pkg/solx-media/config.json")
        );
    }
}
