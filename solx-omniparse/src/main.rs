//! solx-omniparse — Omniparse-based extraction preprocess for solx.
//!
//! This binary is invoked as a `Command`-type action by solx-core. It reads
//! a JSON payload from stdin, extracts text from the source file using the
//! `omniparse` library, and prints a JSON result to stdout.
//!
//! ## OCR support
//!
//! Omniparse has two OCR backends controlled by the `OMNIPARSE_OCR` env var:
//!
//! - `OMNIPARSE_OCR=ml` — ML backend via ocrs + rten (recommended for photos /
//!   screenshots / scanned PDFs). Models auto-download on first use (~12 MB).
//! - `OMNIPARSE_OCR=classical` — pure-Rust classical pipeline (no downloads,
//!   good only on clean printed scans with matched fonts).
//!
//! This binary enables the ML backend by default (`OMNIPARSE_OCR=ml`) so that
//! scanned PDFs and image-only PDFs are handled automatically. The caller can
//! override by setting `OMNIPARSE_OCR` explicitly before invocation.
//!
//! For PDFs with an intact text layer, omniparse's four-tier PDF parser
//! extracts text directly — OCR only kicks in as a fallback when the text
//! layer is empty (image-only / scanned PDFs). See the omniparse OCR guide:
//! <https://github.com/sirhco/omniparse/blob/main/OCR_GUIDE.md#pdf-ocr>
//!
//! ## Logging
//!
//! Uses `solx-package-log` (stderr + `$SOL_LOG_DIR/solx-omniparse.log` +
//! solx-core's console loopback, whichever of those are configured — see
//! that crate's own docs). Replaces this binary's former hand-rolled
//! `log_line` helper.

use base64::Engine as _;
use omniparse::{extract_from_path, Content};
use serde::Deserialize;
use serde_json::json;

mod persist;

#[derive(Debug, Deserialize, Default)]
struct PreprocessInput {
    #[allow(dead_code)]
    artifact_name: Option<String>,
    source_path: Option<String>,
    file_name: Option<String>,
    mime_type: Option<String>,
}

fn print_json(value: serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
    );
}

fn ext_from_name(name: Option<&str>) -> String {
    name.and_then(|n| std::path::Path::new(n).extension().and_then(|e| e.to_str()))
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Determine whether omniparse should be applied to this input.
///
/// Omniparse supports 25+ formats (PDF, DOCX, XLSX, PPTX, ODT, ODS, ODP,
/// EPUB, HTML, RTF, images, etc.). We gate on the same office/document
/// MIME types and extensions as sol-extractous to keep the two packages
/// interchangeable. Plain text, JSON, CSV, etc. are left to the default
/// extraction pipeline.
fn should_apply(mime_type: Option<&str>, file_name: Option<&str>) -> bool {
    let mime = mime_type.unwrap_or_default().to_ascii_lowercase();
    if mime.contains("pdf")
        || mime.contains("msword")
        || mime.contains("officedocument")
        || mime.contains("rtf")
        || mime.contains("opendocument")
        || mime.contains("epub")
    {
        return true;
    }

    let ext = ext_from_name(file_name);
    matches!(
        ext.as_str(),
        "pdf"
            | "doc"
            | "docx"
            | "ppt"
            | "pptx"
            | "xls"
            | "xlsx"
            | "rtf"
            | "odt"
            | "ods"
            | "odp"
            | "epub"
    )
}

fn output_name(input_name: Option<&str>) -> String {
    if let Some(name) = input_name {
        let stem = std::path::Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("extracted");
        format!("{stem}.txt")
    } else {
        "extracted.txt".to_string()
    }
}

/// Ensure the `OMNIPARSE_OCR` env var is set so that OCR runs on image-only /
/// scanned PDFs. If the caller has already set it (to `ml`, `classical`, or
/// `off`), we respect their choice. Otherwise we default to `ml` (the
/// recommended backend for real-world inputs).
async fn ensure_ocr_enabled() {
    if std::env::var("OMNIPARSE_OCR").is_err() {
        // Default to the ML backend. Models auto-download on first use.
        // Use `OMNIPARSE_OCR=off` or `OMNIPARSE_OCR=classical` to override.
        std::env::set_var("OMNIPARSE_OCR", "ml");
        solx_package_log::info("OMNIPARSE_OCR not set; defaulting to 'ml' (ML OCR backend)").await;
    } else {
        let val = std::env::var("OMNIPARSE_OCR").unwrap_or_default();
        solx_package_log::info(&format!("OMNIPARSE_OCR already set to '{val}'")).await;
    }
}

/// Point `OMNIPARSE_OCR_MODELS` at the package-local `bin/models/` directory
/// when it exists, so the install is self-contained (no lazy-download on
/// first OCR call). If the caller has already set the variable, their value
/// wins — that lets the user redirect to a shared cache or air-gapped copy
/// without rebuilding the binary.
///
/// The detection logic mirrors what `build.rs` stages: it looks for both
/// `text-detection.rten` and `text-recognition.rten` next to the binary
/// (under `<exe_dir>/models/`). If either model is missing the auto-wire is
/// skipped and the upstream default cache (`$XDG_CACHE_HOME/omniparse/ocrs-
/// models/` or platform equivalent) takes over.
async fn ensure_models_path() {
    if std::env::var("OMNIPARSE_OCR_MODELS").is_ok() {
        let val = std::env::var("OMNIPARSE_OCR_MODELS").unwrap_or_default();
        solx_package_log::info(&format!("OMNIPARSE_OCR_MODELS already set to '{val}'")).await;
        return;
    }

    let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    else {
        return;
    };

    let models_dir = exe_dir.join("models");
    let detection = models_dir.join("text-detection.rten");
    let recognition = models_dir.join("text-recognition.rten");
    if !detection.is_file() || !recognition.is_file() {
        // Staged models not present (offline build with
        // SOL_OMNIPARSE_SKIP_MODEL_FETCH=1, or binary copied without its
        // sibling dir). Fall back to upstream's default cache.
        solx_package_log::warn(&format!(
            "staged ocr models not found at {}; relying on upstream default cache",
            models_dir.display()
        ))
        .await;
        return;
    }

    // Best-effort: serialize to a lossless string. On Windows this yields
    // `D:\Projects\sol\...`; on POSIX an absolute path. omniparse passes
    // this directly to `PathBuf::from`, so any absolute path is fine.
    let path_str = models_dir.to_string_lossy().into_owned();
    std::env::set_var("OMNIPARSE_OCR_MODELS", &path_str);
    solx_package_log::info(&format!(
        "OMNIPARSE_OCR_MODELS not set; auto-wired to staged models at '{path_str}'"
    ))
    .await;
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    solx_package_log::init("solx-omniparse");

    use std::io::Read as _;
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        solx_package_log::warn(&format!("failed to read stdin ({e}); using empty input")).await;
    }
    solx_package_log::info(&format!("solx-omniparse invoked; stdin bytes={}", raw.len())).await;

    // FU-2: `--write` flag (read from argv[1]) switches the action to also
    // PUT the result to its own URL on solx-server. install.solx wires this
    // up via per-action `fn_name: ".\\solx-omniparse-process-file.exe --write"`.
    let write_mode = std::env::args().nth(1).as_deref() == Some("--write");
    solx_package_log::info(&format!("write_mode={write_mode}")).await;
    let input: PreprocessInput = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            solx_package_log::warn(&format!("stdin is not valid JSON ({e}); using empty input")).await;
            PreprocessInput::default()
        }
    };
    solx_package_log::info(&format!(
        "input: file_name={:?} mime_type={:?} source_path={:?}",
        input.file_name, input.mime_type, input.source_path
    ))
    .await;

    if !should_apply(input.mime_type.as_deref(), input.file_name.as_deref()) {
        solx_package_log::info("should_apply=false; returning empty result (fail-open)").await;
        print_json(json!({}));
        return;
    }

    let Some(source_path) = input.source_path.as_deref() else {
        solx_package_log::warn("source_path is missing; returning empty result (fail-open)").await;
        print_json(json!({}));
        return;
    };

    // Enable OCR for scanned/image-only PDFs (unless caller overrode the env var).
    ensure_ocr_enabled().await;
    // Point OMNIPARSE_OCR_MODELS at the package-local staged models, if present.
    ensure_models_path().await;

    solx_package_log::info(&format!("omniparse: extracting text from '{source_path}'")).await;
    let result = match extract_from_path(source_path) {
        Ok(v) => v,
        Err(e) => {
            solx_package_log::error(&format!(
                "omniparse extract_from_path failed: {e}; returning empty result"
            ))
            .await;
            print_json(json!({}));
            return;
        }
    };

    solx_package_log::info(&format!(
        "omniparse: detected mime_type='{}' confidence={:.2}",
        result.mime_type, result.detection_confidence
    ))
    .await;

    // Log OCR metadata if present (image-only PDFs / image inputs).
    if let Some(ocr_status) = result.metadata.get("ocr_status") {
        solx_package_log::with_data(
            "info",
            &format!(
                "ocr_status={:?} ocr_applied={:?} ocr_confidence={:?}",
                ocr_status,
                result.metadata.get("ocr_applied"),
                result.metadata.get("ocr_confidence")
            ),
            json!({
                "ocr_status": ocr_status,
                "ocr_applied": result.metadata.get("ocr_applied"),
                "ocr_confidence": result.metadata.get("ocr_confidence"),
            }),
        )
        .await;
    }

    // Log which PDF parse strategy was used (strict / repair / raw_scan / pdf-extract).
    if let Some(strategy) = result.metadata.get("pdf_parse_strategy") {
        solx_package_log::info(&format!("pdf_parse_strategy={strategy:?}")).await;
    }

    let text = match result.content {
        Content::Text(t) => t,
        Content::Binary(_) => {
            solx_package_log::warn("omniparse returned binary content (not text); returning empty result").await;
            print_json(json!({}));
            return;
        }
        Content::None => {
            solx_package_log::warn("omniparse returned no content; returning empty result").await;
            print_json(json!({}));
            return;
        }
    };

    solx_package_log::info(&format!("extracted {} chars of text", text.len())).await;

    if text.trim().is_empty() {
        solx_package_log::warn("extracted text is empty after trim; returning empty result").await;
        print_json(json!({}));
        return;
    }

    let text_bytes = text.into_bytes();
    let bytes_base64 = base64::engine::general_purpose::STANDARD.encode(&text_bytes);
    let file_name = output_name(input.file_name.as_deref());
    solx_package_log::info(&format!(
        "success: encoded {} bytes -> {} base64 chars; file_name='{}' mime_type=text/plain",
        text_bytes.len(),
        bytes_base64.len(),
        file_name
    ))
    .await;

    let result = json!({
        "bytes_base64": bytes_base64,
        "file_name": file_name,
        "mime_type": "text/plain"
    });

    if write_mode {
        // FU-2: persist the extraction result to solx-server, then print the
        // combined `{saved, result}` envelope. Soft-fails on connection
        // errors: the v1 `result` is still on stdout so callers can recover.
        // `tokio::main` provides the runtime reqwest needs for its connection
        // pool (pollster::block_on alone isn't enough because reqwest 0.12
        // requires a Tokio reactor to be present).
        let write_result = write_to_solx_server(&file_name, "text/plain", &text_bytes).await;
        match write_result {
            Ok((path, name)) => {
                solx_package_log::info(&format!("saved preprocessed-text document at {path}/{name}")).await;
                print_json(json!({
                    "saved": [{ "path": path, "name": name }],
                    "result": result,
                }));
            }
            Err(e) => {
                solx_package_log::warn(&format!("doc save failed (soft): {e}; returning raw result")).await;
                print_json(json!({
                    "saved": [],
                    "result": result,
                }));
            }
        }
    } else {
        print_json(result);
    }
}

/// FU-2: POST the omniparse extraction to solx-server. Resolves the
/// connection via `solx_package_log::server::ServerConfig::from_env`
/// (`SOLX_SERVER_URL` required; `SOLX_SERVER_TOKEN`/`SOLX_TOKEN`/a
/// `solx-config.json` fallback for the token); returns Err if either is
/// missing or the request fails (caller soft-handles).
async fn write_to_solx_server(
    file_name: &str,
    mime_type: &str,
    text_bytes: &[u8],
) -> Result<(String, String), String> {
    let solx_package_log::server::ServerConfig { server_url, server_token } =
        solx_package_log::server::ServerConfig::from_env()
            .map_err(|e| format!("{e} for --write mode"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    persist::save_document(&client, &server_url, &server_token, file_name, mime_type, text_bytes).await
}
