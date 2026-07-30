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

use std::sync::Mutex;

use base64::Engine as _;
use omniparse::{extract_from_path, Content};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize, Default)]
struct PreprocessInput {
    #[allow(dead_code)]
    artifact_name: Option<String>,
    source_path: Option<String>,
    file_name: Option<String>,
    mime_type: Option<String>,
}

static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// Resolve the per-package log file path from the `SOL_LOG_DIR` env var that
/// the Sol manager sets when invoking Command actions. Returns `None` when
/// the variable is absent (e.g. when the binary is run by hand for debugging).
fn package_log_path() -> Option<std::path::PathBuf> {
    let dir = std::env::var("SOL_LOG_DIR").ok()?;
    let trimmed = dir.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(trimmed).join("sol-omniparse.log"))
}

fn log_line(line: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("{}.{:03}", d.as_secs(), d.subsec_millis()))
        .unwrap_or_else(|_| "0.000".to_string());
    let formatted = format!("[{now}] {line}");

    // Mirror to stderr so `cargo run` debugging is visible.
    eprintln!("{formatted}");

    let Some(path) = package_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut guard) = LOG_FILE.lock() else {
        return;
    };
    if guard.is_none() {
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            *guard = Some(file);
        } else {
            return;
        }
    }
    if let Some(file) = guard.as_mut() {
        use std::io::Write;
        let _ = writeln!(file, "{formatted}");
        let _ = file.flush();
    }
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
fn ensure_ocr_enabled() {
    if std::env::var("OMNIPARSE_OCR").is_err() {
        // Default to the ML backend. Models auto-download on first use.
        // Use `OMNIPARSE_OCR=off` or `OMNIPARSE_OCR=classical` to override.
        std::env::set_var("OMNIPARSE_OCR", "ml");
        log_line("OMNIPARSE_OCR not set; defaulting to 'ml' (ML OCR backend)");
    } else {
        let val = std::env::var("OMNIPARSE_OCR").unwrap_or_default();
        log_line(&format!("OMNIPARSE_OCR already set to '{val}'"));
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
fn ensure_models_path() {
    if std::env::var("OMNIPARSE_OCR_MODELS").is_ok() {
        let val = std::env::var("OMNIPARSE_OCR_MODELS").unwrap_or_default();
        log_line(&format!("OMNIPARSE_OCR_MODELS already set to '{val}'"));
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
        log_line(&format!(
            "staged ocr models not found at {}; relying on upstream default cache",
            models_dir.display()
        ));
        return;
    }

    // Best-effort: serialize to a lossless string. On Windows this yields
    // `D:\Projects\sol\...`; on POSIX an absolute path. omniparse passes
    // this directly to `PathBuf::from`, so any absolute path is fine.
    let path_str = models_dir.to_string_lossy().into_owned();
    std::env::set_var("OMNIPARSE_OCR_MODELS", &path_str);
    log_line(&format!(
        "OMNIPARSE_OCR_MODELS not set; auto-wired to staged models at '{path_str}'"
    ));
}

fn main() {
    use std::io::Read as _;
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        log_line(&format!("failed to read stdin ({e}); using empty input"));
    }
    log_line(&format!(
        "solx-omniparse invoked; stdin bytes={}",
        raw.len()
    ));
    let input: PreprocessInput = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            log_line(&format!("stdin is not valid JSON ({e}); using empty input"));
            PreprocessInput::default()
        }
    };
    log_line(&format!(
        "input: file_name={:?} mime_type={:?} source_path={:?}",
        input.file_name, input.mime_type, input.source_path
    ));

    if !should_apply(input.mime_type.as_deref(), input.file_name.as_deref()) {
        log_line("should_apply=false; returning empty result (fail-open)");
        print_json(json!({}));
        return;
    }

    let Some(source_path) = input.source_path.as_deref() else {
        log_line("source_path is missing; returning empty result (fail-open)");
        print_json(json!({}));
        return;
    };

    // Enable OCR for scanned/image-only PDFs (unless caller overrode the env var).
    ensure_ocr_enabled();
    // Point OMNIPARSE_OCR_MODELS at the package-local staged models, if present.
    ensure_models_path();

    log_line(&format!("omniparse: extracting text from '{source_path}'"));
    let result = match extract_from_path(source_path) {
        Ok(v) => v,
        Err(e) => {
            log_line(&format!(
                "omniparse extract_from_path failed: {e}; returning empty result"
            ));
            print_json(json!({}));
            return;
        }
    };

    log_line(&format!(
        "omniparse: detected mime_type='{}' confidence={:.2}",
        result.mime_type, result.detection_confidence
    ));

    // Log OCR metadata if present (image-only PDFs / image inputs).
    if let Some(ocr_status) = result.metadata.get("ocr_status") {
        log_line(&format!(
            "ocr_status={:?} ocr_applied={:?} ocr_confidence={:?}",
            ocr_status,
            result.metadata.get("ocr_applied"),
            result.metadata.get("ocr_confidence")
        ));
    }

    // Log which PDF parse strategy was used (strict / repair / raw_scan / pdf-extract).
    if let Some(strategy) = result.metadata.get("pdf_parse_strategy") {
        log_line(&format!("pdf_parse_strategy={strategy:?}"));
    }

    let text = match result.content {
        Content::Text(t) => t,
        Content::Binary(_) => {
            log_line("omniparse returned binary content (not text); returning empty result");
            print_json(json!({}));
            return;
        }
        Content::None => {
            log_line("omniparse returned no content; returning empty result");
            print_json(json!({}));
            return;
        }
    };

    log_line(&format!("extracted {} chars of text", text.len()));

    if text.trim().is_empty() {
        log_line("extracted text is empty after trim; returning empty result");
        print_json(json!({}));
        return;
    }

    let text_bytes = text.into_bytes();
    let bytes_base64 = base64::engine::general_purpose::STANDARD.encode(&text_bytes);
    log_line(&format!(
        "success: encoded {} bytes -> {} base64 chars; file_name='{}' mime_type=text/plain",
        text_bytes.len(),
        bytes_base64.len(),
        output_name(input.file_name.as_deref())
    ));

    print_json(json!({
        "bytes_base64": bytes_base64,
        "file_name": output_name(input.file_name.as_deref()),
        "mime_type": "text/plain"
    }));
}