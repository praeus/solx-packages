//! Read/write helper for `{SOLX_PACKAGES_DIR}/solx-media/config.json`. The
//! file holds runtime-persisted settings: `active_whisper_model` and
//! per-model overrides (multimedia_model, summarizer_model) — env vars
//! always win, the file is a fallback / sticky-preference layer.

use serde_json::{Map, Value};
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct MediaConfigOverrides {
    pub active_whisper_model: Option<String>,
    pub multimedia_model: Option<String>,
    pub summarizer_model: Option<String>,
}

/// Read the config file. Missing/unreadable/unparseable → empty defaults.
/// Future fields are preserved (we only touch the three we know about).
pub fn read(path: &Path) -> MediaConfigOverrides {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return MediaConfigOverrides::default();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return MediaConfigOverrides::default();
    };
    let Some(obj) = value.as_object() else {
        return MediaConfigOverrides::default();
    };
    MediaConfigOverrides {
        active_whisper_model: obj
            .get("active_whisper_model")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        multimedia_model: obj
            .get("multimedia_model")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        summarizer_model: obj
            .get("summarizer_model")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    }
}

/// Merge `overrides` into the existing file, then write back. Creates the
/// parent dir if missing. Failures are silent (logged by the caller) — a
/// missing config file should never abort an extraction.
pub fn write(path: &Path, overrides: &MediaConfigOverrides) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create config dir {}: {e}", parent.display()))?;
    }

    // Read existing file (best-effort) and merge.
    let mut root: Map<String, Value> = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    if let Some(v) = &overrides.active_whisper_model {
        root.insert("active_whisper_model".into(), Value::String(v.clone()));
    }
    if let Some(v) = &overrides.multimedia_model {
        root.insert("multimedia_model".into(), Value::String(v.clone()));
    }
    if let Some(v) = &overrides.summarizer_model {
        root.insert("summarizer_model".into(), Value::String(v.clone()));
    }

    let serialized =
        serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(path, serialized)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    Ok(())
}
