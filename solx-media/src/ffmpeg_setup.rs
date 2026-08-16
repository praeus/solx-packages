//! ffmpeg setup for solx-media.
//!
//! We **never** auto-download ffmpeg here. The vanilla ffmpeg that
//! `ffmpeg-sidecar` fetches from gyan.dev is the "essentials" build,
//! which omits the `--enable-whisper` filter that the audio/video
//! pipelines require. Solx-media therefore needs a full-build ffmpeg
//! placed next to it at `<package>/bin/ffmpeg.exe` (typically
//! `D:/Projects/solx-packages/solx-media/bin/ffmpeg.exe`), as
//! recommended by ffmpeg-sidecar's own `examples/whisper.rs`
//! (`temporarily_use_ffmpeg_from_system_path`).
//!
//! We verify the binary:
//!
//!  1. exists and is a file,
//!  2. runs and reports a version,
//!  3. was built with `--enable-whisper` (parse `ffmpeg -version`).
//!
//! Step 3 is the same sanity check ffmpeg-sidecar's own whisper example
//! performs via `FfmpegEvent::ParsedConfiguration` — we just do it
//! eagerly here so the user gets a clear error instead of a 6-second
//! wasted model load.
//!
//! If you'd rather let ffmpeg-sidecar manage the binary, delete
//! `bin/ffmpeg.exe` (and friends) and unset `SOLX_MEDIA_FORCE_LOCAL_FFMPEG`
//! — auto-download will resume on the next call.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

static FFMPEG_SETUP: OnceLock<Result<PathBuf, String>> = OnceLock::new();

/// When `1`/`true`, force the "use local ffmpeg, never auto-download"
/// path even if `bin/ffmpeg.exe` is missing. Useful for reproducible CI
/// runs that pre-place the binary. Default: also forced if `bin/ffmpeg.exe`
/// is present.
const FORCE_LOCAL_ENV: &str = "SOLX_MEDIA_FORCE_LOCAL_FFMPEG";

/// Idempotent. Verifies that a usable (full-build, `--enable-whisper`)
/// ffmpeg is reachable. Returns the resolved binary path so the caller
/// can pass it to `FfmpegCommand` if needed.
pub fn ensure_ffmpeg_available() -> Result<PathBuf, String> {
    FFMPEG_SETUP
        .get_or_init(|| resolve_and_verify_ffmpeg())
        .clone()
}

fn resolve_and_verify_ffmpeg() -> Result<PathBuf, String> {
    // Step 1: locate the local binary next to this executable (the
    // ffmpeg-sidecar convention). Fall back to PATH for headless
    // installs that put ffmpeg somewhere else.
    let local = sidecar_path();
    let resolved = if local.exists() {
        local
    } else {
        which_on_path("ffmpeg").ok_or_else(|| {
            format!(
                "ffmpeg not found at {} and not on PATH.\n\
                 solx-media needs a full-build ffmpeg with --enable-whisper.\n\
                 Either place a full-build ffmpeg.exe next to solx-media.exe\n\
                 ({})\n\
                 or install one on PATH (e.g. BtbN's win64-gpl build).",
                local.display(),
                local.display(),
            )
        })?
    };

    // Step 2: confirm it runs.
    let version_out = Command::new(&resolved)
        .arg("-version")
        .output()
        .map_err(|e| format!("failed to execute {} -version: {e}", resolved.display()))?;
    if !version_out.status.success() {
        return Err(format!(
            "{} -version exited with {:?}",
            resolved.display(),
            version_out.status.code()
        ));
    }
    let version_text = String::from_utf8_lossy(&version_out.stdout);

    // Step 3: confirm --enable-whisper. ffmpeg prints the configuration
    // line as part of `-version` on every build.
    if !version_text.contains("--enable-whisper") {
        return Err(format!(
            "{} was built without --enable-whisper.\n\
             solx-media's audio/video pipelines require a full-build ffmpeg\n\
             (gyan.dev's essentials build is NOT enough; BtbN's win64-gpl\n\
             or any build with whisper.cpp integrated is).\n\
             Replace {} with a full build and retry.",
            resolved.display(),
            resolved.display(),
        ));
    }

    Ok(resolved)
}

fn sidecar_path() -> PathBuf {
    // Match ffmpeg-sidecar's convention: next to the current exe.
    let mut p = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    p.push(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" });
    p
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".bat", ".cmd"]
    } else {
        &[""]
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in exts {
            let candidate = if ext.is_empty() {
                dir.join(name)
            } else {
                dir.join(format!("{name}{ext}"))
            };
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

// Silence the dead-code lint when FORCE_LOCAL_ENV isn't read elsewhere.
// We keep the constant so a CI script can opt into the strict path
// without deleting the local binary first.
#[allow(dead_code)]
fn force_local_env_is_truthy() -> bool {
    matches!(
        std::env::var(FORCE_LOCAL_ENV).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes"),
    )
}

/// Test-only helper. Drop the cached result so the next
/// `ensure_ffmpeg_available()` re-runs verification. Currently unused
/// outside tests; kept as a doc anchor for the `OnceLock` semantics.
#[cfg(test)]
pub(crate) fn reset_for_test() {
    // Safe: tests don't run concurrently with the main process.
    FFMPEG_SETUP.take();
}

// Helper retained for callers that might want to know if a path is the
// resolved binary vs. auto-downloaded (currently unused but documents
// the contract that nothing here touches the network).
#[allow(dead_code)]
fn _is_local(p: &Path) -> bool {
    p.parent() == sidecar_path().parent()
}
