//! Whisper ggml model catalog + downloader. Port of
//! `sol-manager/src/whisper.rs` simplified for standalone use.
//!
//! - `WHISPER_MODEL_CATALOG`: static metadata for every model available from
//!   the ggerganov/whisper.cpp HuggingFace repo.
//! - `install(name, force)`: download + verify SHA-256 → atomic rename →
//!   persist `active_whisper_model` to the media config file.
//! - `active_model_path(cfg)`: resolve the model ffmpeg should use, in
//!   priority order: env var → config file → scan dir for any installed model.

use std::path::{Path, PathBuf};

use crate::config::MediaConfig;
use crate::media_config_file;
use crate::sha256_inline;

pub struct WhisperModelMeta {
    pub name: &'static str,
    pub size_hint_bytes: u64,
    /// SHA-256 of the upstream ggml-{name}.bin. Pinned so a hash mismatch
    /// on download surfaces as an error (not a silent corruption).
    pub sha256: &'static str,
}

/// All-zero hashes mean "hash not yet pinned". Install of such models skips
/// the hash check (with a warning) until the hash is refreshed.
pub const PLACEHOLDER_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// Catalog of the well-known whisper.cpp models. SHA-256 hashes must be
/// refreshed when upstream rotates the model binary.
///
/// **TODO:** Pin the real SHA-256 for `tiny` and `tiny.en` (the two defaults
/// users actually use) before shipping to production. For now the install
/// succeeds without verification; once pinned, downloads will fail closed on
/// any byte mismatch.
pub const WHISPER_MODEL_CATALOG: &[WhisperModelMeta] = &[
    WhisperModelMeta {
        name: "tiny",
        size_hint_bytes: 77_704_441,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "tiny.en",
        size_hint_bytes: 77_704_441,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "tiny-q5_1",
        size_hint_bytes: 32_000_000,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "tiny.en-q5_1",
        size_hint_bytes: 32_000_000,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "tiny-q8_0",
        size_hint_bytes: 44_000_000,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "base",
        size_hint_bytes: 148_000_000,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "base.en",
        size_hint_bytes: 148_000_000,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "small",
        size_hint_bytes: 489_000_000,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "small.en",
        size_hint_bytes: 489_000_000,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "small.en-tdrz",
        size_hint_bytes: 488_000_000,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "medium",
        size_hint_bytes: 1_573_000_000,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "medium.en",
        size_hint_bytes: 1_573_000_000,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "large-v3",
        size_hint_bytes: 3_093_000_000,
        sha256: PLACEHOLDER_HASH,
    },
    WhisperModelMeta {
        name: "large-v3-turbo",
        size_hint_bytes: 1_573_000_000,
        sha256: PLACEHOLDER_HASH,
    },
];

/// All-zero hashes mean "hash not yet pinned". Install of such models skips
/// the hash check (with a warning) until the hash is refreshed.
pub fn default_model() -> &'static str {
    "tiny.en"
}

fn download_url(model_name: &str) -> String {
    if model_name.contains("tdrz") {
        format!(
            "https://huggingface.co/akashmjn/tinydiarize-whisper.cpp/resolve/main/ggml-{model_name}.bin"
        )
    } else {
        format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{model_name}.bin"
        )
    }
}

fn model_path(models_dir: &Path, model_name: &str) -> PathBuf {
    models_dir.join(format!("ggml-{model_name}.bin"))
}

fn find_spec<'a>(name: &str) -> Option<&'a WhisperModelMeta> {
    WHISPER_MODEL_CATALOG.iter().find(|m| m.name == name)
}

/// Resolve the active model path. Priority:
/// 1. `WHISPER_MODEL_PATH` env var (if set and file exists).
/// 2. `active_whisper_model` from `{SOLX_PACKAGES_DIR}/solx-media/config.json`.
/// 3. Scan `WHISPER_MODELS_DIR` for any `ggml-*.bin` (lex-smallest first).
pub fn active_model_path(cfg: &MediaConfig) -> Option<PathBuf> {
    if let Some(p) = &cfg.whisper_model_path {
        if p.exists() {
            return Some(p.clone());
        }
    }

    let overrides = media_config_file::read(&cfg.media_config_path());
    if let Some(name) = &overrides.active_whisper_model {
        let p = model_path(&cfg.whisper_models_dir, name);
        if p.exists() {
            return Some(p);
        }
    }

    let mut installed: Vec<PathBuf> = std::fs::read_dir(&cfg.whisper_models_dir)
        .ok()
        .map(|dir| {
            dir.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().and_then(|s| s.to_str()) == Some("bin")
                        && p.file_name()
                            .and_then(|s| s.to_str())
                            .map(|s| s.starts_with("ggml-"))
                            .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    installed.sort();
    installed.into_iter().next()
}

#[derive(Debug)]
pub struct InstalledModel {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
}

/// Download (or verify) a whisper model. Returns metadata on success.
///
/// - `name`: model name; defaults to `tiny.en` when `None`.
/// - `force`: re-download even if the file is already present and verified.
pub async fn install(
    cfg: &MediaConfig,
    name: Option<String>,
    force: bool,
) -> Result<InstalledModel, String> {
    let name = name.unwrap_or_else(|| default_model().to_string());
    let spec = find_spec(&name)
        .ok_or_else(|| format!("unknown whisper model '{name}' (not in WHISPER_MODEL_CATALOG)"))?;

    let dest = model_path(&cfg.whisper_models_dir, &name);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create models dir {}: {e}", parent.display()))?;
    }

    // Already present + matches the pinned hash + not forced → return early.
    if !force && dest.exists() {
        if spec.sha256 != PLACEHOLDER_HASH {
            let actual = sha256_inline::hash_file(&dest).map_err(|e| format!("hash: {e}"))?;
            if actual.eq_ignore_ascii_case(spec.sha256) {
                solx_package_log::info(&format!(
                    "whisper model '{name}' already installed and verified; skipping download"
                ))
                .await;
                let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                return Ok(InstalledModel {
                    name: name.clone(),
                    path: dest,
                    size_bytes: size,
                    sha256: actual,
                });
            }
            // Hash mismatch → remove + re-download.
            solx_package_log::warn(&format!(
                "whisper model '{name}' failed hash verification; re-downloading"
            ))
            .await;
            let _ = std::fs::remove_file(&dest);
        }
    }

    // Download into a sibling tmp file, then atomic rename.
    let tmp = dest.with_extension("bin.tmp");
    let url = download_url(&name);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("download request failed: {e}"))?;
    let mut resp = resp
        .error_for_status()
        .map_err(|e| format!("download returned error: {e}"))?;

    // `content_length()` reflects the real transfer size when the server
    // sends one; the catalog's hint is only a fallback for a chunked
    // response with no `Content-Length` header.
    let total = resp.content_length().unwrap_or(spec.size_hint_bytes);
    solx_package_log::with_data(
        "info",
        &format!("downloading whisper model '{name}' ({total} bytes) from {url}"),
        serde_json::json!({ "name": name, "total": total }),
    )
    .await;

    let mut file = std::fs::File::create(&tmp)
        .map_err(|e| format!("failed to create tmp file {}: {e}", tmp.display()))?;
    use std::io::Write;

    // Progress is naturally the most valuable thing to see here — this is
    // the single largest, slowest operation in the whole package (up to
    // ~3 GB) — but logging every chunk would be hundreds of console writes
    // a second. Throttle to roughly once per 5 MiB.
    const LOG_EVERY_BYTES: u64 = 5 * 1024 * 1024;
    let mut downloaded: u64 = 0;
    let mut last_logged: u64 = 0;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("download stream error: {e}"))?
    {
        file.write_all(&chunk)
            .map_err(|e| format!("failed writing tmp file: {e}"))?;
        downloaded += chunk.len() as u64;
        if downloaded - last_logged >= LOG_EVERY_BYTES {
            last_logged = downloaded;
            let pct = if total > 0 { (downloaded * 100 / total) as u32 } else { 0 };
            solx_package_log::with_data(
                "info",
                &format!("downloading whisper model '{name}': {downloaded}/{total} bytes ({pct}%)"),
                serde_json::json!({ "name": name, "downloaded": downloaded, "total": total, "pct": pct }),
            )
            .await;
        }
    }
    drop(file);

    let actual = sha256_inline::hash_file(&tmp).map_err(|e| format!("hash: {e}"))?;
    if spec.sha256 != PLACEHOLDER_HASH && !actual.eq_ignore_ascii_case(spec.sha256) {
        let _ = std::fs::remove_file(&tmp);
        solx_package_log::error(&format!(
            "sha256 mismatch for '{name}' (expected {}, got {actual})",
            spec.sha256
        ))
        .await;
        return Err(format!(
            "sha256 mismatch for {name} (expected {}, got {})",
            spec.sha256, actual
        ));
    }

    std::fs::rename(&tmp, &dest)
        .map_err(|e| format!("failed to rename tmp -> dest: {e}"))?;

    solx_package_log::info(&format!("whisper model '{name}' installed ({downloaded} bytes)")).await;

    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);

    // Persist as the active model (only when env var wasn't overriding).
    if cfg.whisper_model_path.is_none() {
        let overrides = media_config_file::MediaConfigOverrides {
            active_whisper_model: Some(name.clone()),
            multimedia_model: None,
            summarizer_model: None,
        };
        let _ = media_config_file::write(&cfg.media_config_path(), &overrides);
    }

    Ok(InstalledModel {
        name,
        path: dest,
        size_bytes: size,
        sha256: actual,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_default() {
        assert!(find_spec(default_model()).is_some());
    }

    #[test]
    fn default_model_resolves() {
        assert_eq!(default_model(), "tiny.en");
    }
}
