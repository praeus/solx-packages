//! ffmpeg-sidecar auto-download. Mirrors `sol-manager/src/manager.rs:55-71`.
//! On first invocation ffmpeg is downloaded into the sidecar crate's default
//! per-user cache; subsequent invocations reuse the cached binary.

use std::sync::OnceLock;

static FFMPEG_SETUP: OnceLock<Result<(), String>> = OnceLock::new();

/// Idempotent: downloads ffmpeg on first call (via the sidecar crate),
/// returns the cached success/failure result on every subsequent call. The
/// `OnceLock<Result>` shape means the binary never re-downloads after a
/// successful install, but also doesn't loop forever if the download failed.
pub fn ensure_ffmpeg_available() -> Result<(), String> {
    FFMPEG_SETUP
        .get_or_init(|| {
            ffmpeg_sidecar::download::auto_download()
                .map_err(|e| format!("ffmpeg auto-download failed: {e}"))
        })
        .clone()
}
