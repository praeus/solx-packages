//! Audio extraction. Whisper-transcribe via ffmpeg, then optionally
//! synthesize a summary via Ollama. Returns a `MediaDocument` of kind
//! `audio-transcript`.

use std::path::{Path, PathBuf};

use ffmpeg_sidecar::command::FfmpegCommand;
use ffmpeg_sidecar::event::FfmpegEvent;
use serde_json::{json, Value};

use crate::config::MediaConfig;
use crate::ffmpeg_setup;
use crate::ollama;
use crate::prompt;
use crate::whisper_models;

pub async fn run_audio(
    client: &reqwest::Client,
    bytes: &[u8],
    file_name: Option<&str>,
    cfg: &MediaConfig,
) -> Result<Value, String> {
    ffmpeg_setup::ensure_ffmpeg_available()?;

    let ext = file_name
        .and_then(|n| Path::new(n).extension())
        .and_then(|e| e.to_str())
        .unwrap_or("audio");
    let temp_path = write_bytes_to_temp(bytes, ext)?;
    let _temp_guard = TempFileGuard(temp_path.clone());

    let model_path = whisper_models::active_model_path(cfg).ok_or_else(|| {
        "no whisper model available: run solx exec /packages/solx-media/solx-media-install-whisper-model first, \
         or set WHISPER_MODEL_PATH"
            .to_string()
    })?;

    let (transcript, segments) = match try_ffmpeg_whisper_transcribe(&temp_path, &model_path).await {
        Ok(t) => t,
        Err(e) => {
            solx_package_log::warn(&format!("whisper failed: {e}; trying audio extraction")).await;
            match ffmpeg_extract_audio_wav(&temp_path).await {
                Ok(wav_bytes) => {
                    let wav_path = write_bytes_to_temp(&wav_bytes, "wav")?;
                    let _wav_guard = TempFileGuard(wav_path.clone());
                    match try_ffmpeg_whisper_transcribe(&wav_path, &model_path).await {
                        Ok(t2) => t2,
                        Err(e2) => {
                            solx_package_log::warn(&format!("wav whisper also failed: {e2}")).await;
                            (String::new(), Vec::new())
                        }
                    }
                }
                Err(e2) => {
                    solx_package_log::warn(&format!("audio extraction failed: {e2}")).await;
                    (String::new(), Vec::new())
                }
            }
        }
    };

    let media_info = ffmpeg_media_info_string(&temp_path, file_name);
    let segments_summary = if segments.is_empty() {
        "(no timestamped segments available)".to_string()
    } else {
        segments
            .iter()
            .take(20)
            .map(|s| {
                let start_ms = s.get("start_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                let end_ms = s.get("end_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                let text = s.get("text").and_then(|v| v.as_str()).unwrap_or("");
                format!(
                    "[{:.1}s-{:.1}s] {}",
                    start_ms as f64 / 1000.0,
                    end_ms as f64 / 1000.0,
                    text
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let transcript_for_prompt = if transcript.is_empty() {
        "(no transcription available)".to_string()
    } else {
        transcript.clone()
    };

    let template = prompt::load("extraction-audio-synthesize.prompt.txt")?;
    let rendered = prompt::render(
        template,
        &[
            ("audio_metadata", &media_info),
            ("transcript", &transcript_for_prompt),
            ("acoustic_description", "(no acoustic description available)"),
            ("segments_summary", &segments_summary),
        ],
    );

    let synthesis = match ollama::generate(client, &cfg.ollama_url, &cfg.summarizer_model, &rendered).await {
        Ok(resp) => ollama::parse_llm_json(&resp),
        Err(e) => {
            solx_package_log::warn(&format!("summarizer failed: {e}")).await;
            None
        }
    };

    let document_name = synthesis
        .as_ref()
        .and_then(|s| s.get("document_name"))
        .and_then(|v| v.as_str())
        .map(|s| normalize_document_name(s, "audio-transcript"))
        .unwrap_or_else(|| {
            normalize_document_name(file_name.unwrap_or("audio"), "audio-transcript")
        });
    let title = synthesis
        .as_ref()
        .and_then(|s| s.get("title"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let summary = synthesis
        .as_ref()
        .and_then(|s| s.get("summary"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let description = synthesis
        .as_ref()
        .and_then(|s| s.get("description"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let metadata_notes = synthesis
        .as_ref()
        .and_then(|s| s.get("metadata_notes"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty());

    let mut contents = json!({
        "audio_metadata": media_info,
        "model": cfg.summarizer_model,
    });
    if !transcript.is_empty() {
        contents["transcript"] = Value::String(transcript.clone());
    }
    if !segments.is_empty() {
        contents["segments"] = serde_json::to_value(&segments)
            .map_err(|e| format!("serialize segments: {e}"))?;
    }
    if let Some(desc) = &description {
        contents["description"] = Value::String(desc.clone());
    }

    let mut notes: Vec<String> = Vec::new();
    if transcript.is_empty() {
        notes.push("Transcription unavailable (whisper failed)".to_string());
    }
    if let Some(mn) = metadata_notes {
        notes.push(mn);
    }

    Ok(json!({
        "kind": "audio-transcript",
        "document_name": document_name,
        "title": title,
        "summary": summary,
        "author": Value::Null,
        "contents": contents,
        "notes": notes,
    }))
}

// ---------------------------------------------------------------------------
// ffmpeg/ffmpeg-sidecar plumbing
// ---------------------------------------------------------------------------

pub(super) struct TempFileGuard(pub(super) PathBuf);
impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(super) fn write_bytes_to_temp(bytes: &[u8], ext: &str) -> Result<PathBuf, String> {
    let mut path = std::env::temp_dir();
    path.push(format!("solx-media-{}.{}", uuid::Uuid::new_v4(), ext));
    std::fs::write(&path, bytes).map_err(|e| format!("failed writing temp media file: {e}"))?;
    Ok(path)
}

pub(super) async fn try_ffmpeg_whisper_transcribe(
    path: &Path,
    model_path: &Path,
) -> Result<(String, Vec<Value>), String> {
    if !model_path.exists() {
        return Err(format!("whisper model not found at {}", model_path.display()));
    }

    // ffmpeg's whisper-filter parser treats the first colon in the
    // `model=...` value as the field separator, so a Windows drive-letter
    // path like `C:/Users/foo/ggml-tiny.en.bin` parses as
    //   model = "C", destination = "/Users/..." (bogus), queue = 4
    // and the filter fails with "No option name near /Users/...". Two
    // fixes work — pick whichever produces a shorter, unambiguous path:
    //
    //   * relative to the ffmpeg-sidecar cwd (the package `bin/`), or
    //   * a `\\?\` UNC absolute path (whitespace-safe and colon-safe on
    //     Windows; ffmpeg's path parser accepts the prefix transparently).
    //
    // We try the relative first, falling back to the UNC form, and only
    // then to the forward-slashed form (which works on Linux/macOS but
    // not Windows).
    let pkg_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let model_arg = model_path_for_filter(model_path, pkg_bin.as_deref());

    let whisper_filter = format!(
        "whisper=model={}:destination=-:queue=4",
        model_arg
    );

    let mut transcript_lines: Vec<String> = Vec::new();
    let mut cmd = FfmpegCommand::new();
    cmd.input(path.to_string_lossy().as_ref())
        .arg("-af")
        .arg(&whisper_filter)
        .format("null")
        .output("-");
    let mut child = cmd.spawn().map_err(|e| format!("ffmpeg spawn failed: {e}"))?;
    let iter = child.iter().map_err(|e| format!("ffmpeg iter failed: {e}"))?;
    let mut stdout_buf: Vec<u8> = Vec::new();
    for event in iter {
        match event {
            // The whisper filter writes its output to stdout (per the
            // `destination=-` arg). Older versions of this code only
            // listened on stderr, which silently produced empty
            // transcripts because ffmpeg-sidecar's `FfmpegEvent::Log`
            // surfaces only stderr.
            FfmpegEvent::OutputChunk(chunk) => {
                stdout_buf.extend_from_slice(&chunk);
            }
            FfmpegEvent::Log(_level, msg) => {
                let m = msg.trim();
                if m.starts_with('[') && m.contains("-->") {
                    transcript_lines.push(m.to_string());
                }
            }
            _ => {}
        }
        // Cooperative cancellation: a detached `action_stop` sets the flag
        // the loopback `/cancelled` route reports. Dropping `child` here
        // kills the ffmpeg sidecar process.
        if solx_package_log::cancelled().await {
            return Err("cancelled".to_string());
        }
    }
    // Decode the captured stdout once the ffmpeg child exits. Treat each
    // line that starts with `[` and contains `-->` as a timestamped
    // segment; anything else is plain transcript text. The whisper filter
    // emits both forms depending on the build/version.
    if let Ok(stdout_text) = std::str::from_utf8(&stdout_buf) {
        for line in stdout_text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.contains("-->") {
                transcript_lines.push(line.to_string());
            } else {
                // Plain text — collect under a synthetic segment so it
                // still contributes to the transcript. We don't have
                // timestamps here; downstream code joins all segments
                // for the final transcript string.
                transcript_lines.push(format!("[00:00:00.000 --> 00:00:00.000] {line}"));
            }
        }
    }

    let segments: Vec<Value> = transcript_lines
        .iter()
        .filter_map(|line| {
            let inner = line.trim().trim_start_matches('[');
            let parts: Vec<&str> = inner.splitn(2, " --> ").collect();
            if parts.len() != 2 {
                return None;
            }
            let start_ms = parse_whisper_ts(parts[0].trim())?;
            let rest = parts[1];
            let (end_str, text) = if let Some(bracket_end) = rest.find(']') {
                (&rest[..bracket_end], rest[bracket_end + 1..].trim())
            } else {
                return None;
            };
            let end_ms = parse_whisper_ts(end_str.trim())?;
            Some(json!({
                "start_ms": start_ms,
                "end_ms": end_ms,
                "speaker": Value::Null,
                "text": text,
            }))
        })
        .collect();

    let transcript = segments
        .iter()
        .filter_map(|s| s.get("text").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    Ok((transcript, segments))
}

/// Choose a model-path representation that's safe to embed inside an
/// ffmpeg `whisper=model=...` filter on the current platform.
///
/// On Windows the filter parser treats the first colon as a field
/// separator, so any absolute path with a drive letter breaks it.
/// Linux/macOS have no such issue with colon, but on POSIX the
/// `\\?\` prefix is also accepted (and harmless).
///
/// Strategy (in order):
///
///  1. Relative to `<package>/bin/` if the model is reachable from
///     there. The package is invoked with cwd=<bin>, so a relative
///     path is colon-free.
///  2. Stage the model to `<bin>/whisper-model.bin` if it's not already
///     there. ffmpeg's spawn cwd is `<bin>`, so this becomes a clean
///     relative reference.
///  3. Fall back to the forward-slashed absolute path (correct on
///     POSIX; broken on Windows but at least the error is informative).
fn model_path_for_filter(model_path: &Path, pkg_bin: Option<&Path>) -> String {
    let Some(bin) = pkg_bin else {
        return model_path.to_string_lossy().replace('\\', "/");
    };

    // 1. Already under bin/?
    if let Ok(rel) = model_path.strip_prefix(bin) {
        return normalize_rel(rel);
    }

    // 2. Stage a colon-free copy in bin/. Idempotent — only copies if
    //    the target is missing or stale.
    if let Some(staged) = stage_model_to_bin(model_path, bin) {
        if let Ok(rel) = staged.strip_prefix(bin) {
            return normalize_rel(rel);
        }
        return staged.to_string_lossy().replace('\\', "/");
    }

    // 3. Last resort.
    model_path.to_string_lossy().replace('\\', "/")
}

fn normalize_rel(rel: &Path) -> String {
    let mut s = rel.to_string_lossy().into_owned();
    s = s.replace('\\', "/");
    if let Some(stripped) = s.strip_prefix("./") {
        return stripped.to_string();
    }
    s
}

/// Copy `model_path` to `<bin>/whisper-model.bin` (or `.ggml-<basename>`)
/// if the target is missing or out of date. Returns the staged path on
/// success. Returns `None` on any I/O failure — callers fall through to
/// the absolute-path last resort.
fn stage_model_to_bin(model_path: &Path, bin: &Path) -> Option<std::path::PathBuf> {
    let target_name = if cfg!(windows) {
        "whisper-model.bin"
    } else {
        "whisper-model.ggml"
    };
    let staged = bin.join(target_name);
    let needs_copy = match (staged.metadata(), model_path.metadata()) {
        (Ok(s), Ok(m)) => s.len() != m.len() || s.modified().ok() < m.modified().ok(),
        (Err(_), _) => true,
        _ => false,
    };
    if !needs_copy {
        return Some(staged);
    }
    if std::fs::copy(model_path, &staged).is_ok() {
        Some(staged)
    } else {
        None
    }
}

fn parse_whisper_ts(ts: &str) -> Option<u64> {
    let parts: Vec<&str> = ts.splitn(3, ':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h: u64 = parts[0].parse().ok()?;
    let m: u64 = parts[1].parse().ok()?;
    let sec_parts: Vec<&str> = parts[2].splitn(2, '.').collect();
    let s: u64 = sec_parts[0].parse().ok()?;
    let ms: u64 = if sec_parts.len() == 2 {
        let raw = sec_parts[1];
        let padded = format!("{:0<3}", &raw[..raw.len().min(3)]);
        padded.parse().unwrap_or(0)
    } else {
        0
    };
    Some(h * 3_600_000 + m * 60_000 + s * 1_000 + ms)
}

pub(super) async fn ffmpeg_extract_audio_wav(path: &Path) -> Result<Vec<u8>, String> {
    let mut wav_bytes: Vec<u8> = Vec::new();
    let mut cmd = FfmpegCommand::new();
    cmd.input(path.to_string_lossy().as_ref())
        .args(["-vn", "-acodec", "pcm_s16le", "-ar", "16000", "-ac", "1"])
        .format("wav")
        .output("pipe:1");
    let mut child = cmd.spawn().map_err(|e| format!("ffmpeg extract audio spawn: {e}"))?;
    let iter = child.iter().map_err(|e| format!("ffmpeg extract audio iter: {e}"))?;
    for event in iter {
        if let FfmpegEvent::OutputChunk(chunk) = event {
            wav_bytes.extend_from_slice(&chunk);
        }
        if solx_package_log::cancelled().await {
            return Err("cancelled".to_string());
        }
    }
    if wav_bytes.is_empty() {
        Err("ffmpeg produced empty audio output (file may have no audio stream)".to_string())
    } else {
        Ok(wav_bytes)
    }
}

pub(super) fn ffmpeg_media_info_string(path: &Path, file_name: Option<&str>) -> String {
    let mut info_lines: Vec<String> = Vec::new();
    if let Ok(mut child) = FfmpegCommand::new()
        .input(path.to_string_lossy().as_ref())
        .format("null")
        .output("-")
        .spawn()
    {
        if let Ok(iter) = child.iter() {
            for event in iter {
                if let FfmpegEvent::Log(_level, msg) = &event {
                    let m = msg.trim();
                    if m.starts_with("Duration:")
                        || m.contains("Stream #")
                        || m.starts_with("  Duration")
                    {
                        info_lines.push(m.to_string());
                    }
                }
            }
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = file_name {
        parts.push(format!("filename: {name}"));
    }
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    parts.push(format!("file_size: {file_size} bytes"));
    for line in &info_lines {
        parts.push(line.clone());
    }
    parts.join(", ")
}

fn normalize_document_name(input: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
}
