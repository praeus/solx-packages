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

    let (transcript, segments) = match try_ffmpeg_whisper_transcribe(&temp_path, &model_path) {
        Ok(t) => t,
        Err(e) => {
            solx_package_log::warn(&format!("whisper failed: {e}; trying audio extraction")).await;
            match ffmpeg_extract_audio_wav(&temp_path) {
                Ok(wav_bytes) => {
                    let wav_path = write_bytes_to_temp(&wav_bytes, "wav")?;
                    let _wav_guard = TempFileGuard(wav_path.clone());
                    match try_ffmpeg_whisper_transcribe(&wav_path, &model_path) {
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

pub(super) fn try_ffmpeg_whisper_transcribe(
    path: &Path,
    model_path: &Path,
) -> Result<(String, Vec<Value>), String> {
    if !model_path.exists() {
        return Err(format!("whisper model not found at {}", model_path.display()));
    }

    let whisper_filter = format!(
        "whisper=model={}:destination=-:queue=4",
        model_path.to_string_lossy().replace('\\', "/")
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
    for event in iter {
        if let FfmpegEvent::Log(_level, msg) = event {
            let m = msg.trim();
            if m.starts_with('[') && m.contains("-->") {
                transcript_lines.push(m.to_string());
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

pub(super) fn ffmpeg_extract_audio_wav(path: &Path) -> Result<Vec<u8>, String> {
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
