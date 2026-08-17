//! solx-media — media extraction action binary for solx-core.
//!
//! Reads JSON on stdin, dispatches on `argv[1]` to image / audio / video /
//! materialize-html / install-whisper-model. Extraction modes POST their
//! result to solx-server (`POST /docs/save`) and print a summary to stdout.
//!
//! Contract:
//! - input:  JSON on stdin (e.g. `{"source_path": "C:/img.png", "file_name": "img.png"}`)
//! - output: JSON on stdout (e.g. `{"saved":[{"path":"/media/image-text/...","name":"..."}], "kind":"image-text", "document":{...}}`)
//! - stderr: human-readable log lines; mirrored to `$SOL_LOG_DIR/solx-media.log`

use std::io::Read as _;
use std::path::PathBuf;

use serde_json::{json, Value};

mod audio;
mod config;
mod ffmpeg_setup;
mod materialize;
mod media_config_file;
mod ollama;
mod persist;
mod prompt;
mod sha256_inline;
mod video;
mod vision;
mod whisper_models;

use crate::config::MediaConfig;

fn print_json(value: Value) {
    println!(
        "{}",
        serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
    );
}

fn usage_and_exit() -> ! {
    eprintln!("solx-media: missing mode argument");
    eprintln!("Usage: solx-media <mode> [flags]");
    eprintln!("Modes:");
    eprintln!("  image              Extract an image to a MediaDocument (kind=image-text)");
    eprintln!("  audio              Extract an audio file (kind=audio-transcript)");
    eprintln!("  video              Extract a video file (kind=video-transcript)");
    eprintln!("  materialize-html   Walk an HTML rich_text payload, embed remote images");
    eprintln!("  install-whisper-model [name] [force=true]");
    eprintln!();
    eprintln!("Reads JSON params from stdin (source_path, file_name, source_url, name, force, ...).");
    eprintln!("Prints JSON result on stdout; persists extracted MediaDocuments via /docs/save.");
    std::process::exit(2);
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    solx_package_log::init("solx-media");

    let mode = std::env::args().nth(1);
    let mode = match mode {
        Some(m) => m,
        None => usage_and_exit(),
    };

    // Read stdin up-front; modes that don't need it (e.g. install-whisper-model
    // can still take optional name/force from stdin) ignore it.
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        solx_package_log::warn(&format!("failed to read stdin ({e}); using empty input")).await;
    }
    let input: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            solx_package_log::warn(&format!("stdin is not valid JSON ({e}); using empty input")).await;
            Value::Object(Default::default())
        }
    };

    let cfg = match MediaConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("solx-media: config error: {e}");
            std::process::exit(2);
        }
    };
    solx_package_log::info(&format!("solx-media invoked; mode={mode}; config=ok")).await;

    // Apply config-file overrides on top of env defaults. Env vars still win
    // because we only apply the file override when the env var didn't set a
    // value AND the field is empty in the env-derived config.
    let overrides = media_config_file::read(&cfg.media_config_path());
    let cfg = apply_overrides(cfg, &overrides);

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("failed to build HTTP client");

    let result: Result<Value, String> = match mode.as_str() {
        "image" => mode_image(&http_client, &cfg, &input).await,
        "audio" => mode_audio(&http_client, &cfg, &input).await,
        "video" => mode_video(&http_client, &cfg, &input).await,
        "materialize-html" => mode_materialize_html(&cfg, &input).await,
        "install-whisper-model" => mode_install_whisper(&cfg, &input).await,
        other => Err(format!("unknown mode '{other}'")),
    };

    match result {
        Ok(value) => {
            print_json(value);
        }
        Err(e) => {
            solx_package_log::error(&format!("mode {mode} failed: {e}")).await;
            std::process::exit(1);
        }
    }
}

fn apply_overrides(mut cfg: MediaConfig, overrides: &media_config_file::MediaConfigOverrides) -> MediaConfig {
    if let Some(m) = &overrides.multimedia_model {
        if std::env::var("MULTIMEDIA_MODEL").ok().map(|v| v.trim().is_empty()).unwrap_or(true) {
            cfg.multimedia_model = m.clone();
        }
    }
    if let Some(m) = &overrides.summarizer_model {
        if std::env::var("SUMMARIZER_MODEL").ok().map(|v| v.trim().is_empty()).unwrap_or(true) {
            cfg.summarizer_model = m.clone();
        }
    }
    cfg
}

async fn mode_image(
    client: &reqwest::Client,
    cfg: &MediaConfig,
    input: &Value,
) -> Result<Value, String> {
    let source_path = require_source_path(input)?;
    let source_path_str = source_path.to_string_lossy().into_owned();
    let file_name = input
        .get("file_name")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let bytes = std::fs::read(&source_path)
        .map_err(|e| format!("failed reading source '{source_path_str}': {e}"))?;
    let doc = vision::run_image(client, &bytes, file_name.as_deref(), cfg).await?;
    save_and_summarize(client, cfg, doc).await
}

async fn mode_audio(
    client: &reqwest::Client,
    cfg: &MediaConfig,
    input: &Value,
) -> Result<Value, String> {
    let source_path = require_source_path(input)?;
    let source_path_str = source_path.to_string_lossy().into_owned();
    let file_name = input
        .get("file_name")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let bytes = std::fs::read(&source_path)
        .map_err(|e| format!("failed reading source '{source_path_str}': {e}"))?;
    let doc = audio::run_audio(client, &bytes, file_name.as_deref(), cfg).await?;
    save_and_summarize(client, cfg, doc).await
}

async fn mode_video(
    client: &reqwest::Client,
    cfg: &MediaConfig,
    input: &Value,
) -> Result<Value, String> {
    let source_path = require_source_path(input)?;
    let source_path_str = source_path.to_string_lossy().into_owned();
    let file_name = input
        .get("file_name")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let bytes = std::fs::read(&source_path)
        .map_err(|e| format!("failed reading source '{source_path_str}': {e}"))?;
    let doc = video::run_video(client, &bytes, file_name.as_deref(), cfg).await?;
    save_and_summarize(client, cfg, doc).await
}

async fn mode_materialize_html(cfg: &MediaConfig, input: &Value) -> Result<Value, String> {
    let source_path = input
        .get("source_path")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let source_url = input
        .get("source_url")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let doc = materialize::run_materialize_html(
        source_path.as_deref(),
        source_url.as_deref(),
        input,
    )
    .await?;
    let http_client = reqwest::Client::new();
    save_and_summarize(&http_client, cfg, doc).await
}

async fn mode_install_whisper(cfg: &MediaConfig, input: &Value) -> Result<Value, String> {
    let name = input
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let force = input
        .get("force")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let installed = whisper_models::install(cfg, name, force).await?;
    Ok(json!({
        "name": installed.name,
        "path": installed.path,
        "size_bytes": installed.size_bytes,
        "sha256": installed.sha256,
    }))
}

async fn save_and_summarize(
    client: &reqwest::Client,
    cfg: &MediaConfig,
    doc: Value,
) -> Result<Value, String> {
    let kind = doc.get("kind").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

    let saved = match persist::save_document(client, &cfg.server_url, &cfg.server_token, &doc).await {
        Ok((path, name)) => {
            solx_package_log::info(&format!("saved {kind} document at {path}/{name}")).await;
            json!([{ "path": path, "name": name }])
        }
        Err(e) => {
            // Soft failure: still return the document so the caller can
            // persist it themselves.
            solx_package_log::warn(&format!(
                "docs/save failed (soft): {e}; returning document to caller"
            ))
            .await;
            json!([])
        }
    };

    Ok(json!({
        "saved": saved,
        "kind": kind,
        "document": doc,
    }))
}

fn require_source_path(input: &Value) -> Result<PathBuf, String> {
    let path = input
        .get("source_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing required field 'source_path'".to_string())?;
    let p = PathBuf::from(path);
    if !p.exists() {
        return Err(format!("source_path '{path}' does not exist"));
    }
    Ok(p)
}
