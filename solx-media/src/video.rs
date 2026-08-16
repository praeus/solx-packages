//! Video extraction. Reuses the audio pipeline for the transcript, then
//! additionally extracts frames at 1 frame per 10s and asks Ollama to caption
//! each frame. Returns a `MediaDocument` of kind `video-transcript`.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use ffmpeg_sidecar::command::FfmpegCommand;
use ffmpeg_sidecar::event::FfmpegEvent;
use serde_json::{json, Value};

use crate::audio;
use crate::config::MediaConfig;
use crate::ffmpeg_setup;
use crate::ollama;
use crate::prompt;
use crate::whisper_models;

pub async fn run_video(
    client: &reqwest::Client,
    bytes: &[u8],
    file_name: Option<&str>,
    cfg: &MediaConfig,
) -> Result<Value, String> {
    ffmpeg_setup::ensure_ffmpeg_available()?;

    let ext = file_name
        .and_then(|n| Path::new(n).extension())
        .and_then(|e| e.to_str())
        .unwrap_or("video");
    let temp_path = audio::write_bytes_to_temp(bytes, ext)?;
    let _temp_guard = audio::TempFileGuard(temp_path.clone());

    // 1) Transcript (mirrors audio path; falls back to wav extract).
    let model_path = whisper_models::active_model_path(cfg).ok_or_else(|| {
        "no whisper model available: run solx exec /packages/solx-media/solx-media-install-whisper-model first, \
         or set WHISPER_MODEL_PATH"
            .to_string()
    })?;

    let (transcript, segments) = match audio::try_ffmpeg_whisper_transcribe(&temp_path, &model_path).await {
        Ok(t) => t,
        Err(e) => {
            solx_package_log::warn(&format!("direct whisper failed: {e}; trying audio extraction")).await;
            match audio::ffmpeg_extract_audio_wav(&temp_path).await {
                Ok(wav_bytes) => {
                    let wav_path = audio::write_bytes_to_temp(&wav_bytes, "wav")?;
                    let _wav_guard = audio::TempFileGuard(wav_path.clone());
                    match audio::try_ffmpeg_whisper_transcribe(&wav_path, &model_path).await {
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

    // 2) Scene captions (per-frame vision calls).
    let mut scene_captions: Vec<Value> = Vec::new();
    match ffmpeg_extract_frames(&temp_path, 20).await {
        Ok(frames) => {
            for (ts_ms, jpeg_bytes) in &frames {
                if solx_package_log::cancelled().await {
                    solx_package_log::warn("video frame captioning cancelled").await;
                    break;
                }
                let ts_secs = *ts_ms as f64 / 1000.0;
                let frame_prompt = format!(
                    "Describe what you see in this video frame (timestamp: {:.1}s). \
                     Focus on the main subject, action, setting, and any visible text. \
                     Be concise (1-3 sentences).",
                    ts_secs
                );
                let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg_bytes);
                match ollama::generate_with_image(
                    client,
                    &cfg.ollama_url,
                    &cfg.multimedia_model,
                    &frame_prompt,
                    &b64,
                )
                .await
                {
                    Ok(desc) => {
                        scene_captions.push(json!({
                            "start_ms": ts_ms,
                            "end_ms": ts_ms + 10_000,
                            "speaker": Value::Null,
                            "text": desc,
                        }));
                    }
                    Err(e) => {
                        solx_package_log::warn(&format!("frame description failed at {ts_secs}s: {e}")).await;
                    }
                }
            }
        }
        Err(e) => {
            solx_package_log::warn(&format!("frame extraction failed: {e}")).await;
        }
    }

    // 3) Synthesis prompt (video variant).
    let media_info = audio::ffmpeg_media_info_string(&temp_path, file_name);
    let transcript_for_prompt = if transcript.is_empty() {
        "(no transcription available)".to_string()
    } else {
        transcript.clone()
    };
    let desc_for_prompt = "(no acoustic description available)".to_string();
    let scenes_for_prompt = if scene_captions.is_empty() {
        "(no frame descriptions available)".to_string()
    } else {
        scene_captions
            .iter()
            .map(|s| {
                let ts = s.get("start_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                let text = s.get("text").and_then(|v| v.as_str()).unwrap_or("");
                format!("[{:.1}s] {}", ts as f64 / 1000.0, text)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let template = prompt::load("extraction-video-synthesize.prompt.txt")?;
    let rendered = prompt::render(
        template,
        &[
            ("video_metadata", &media_info),
            ("transcript", &transcript_for_prompt),
            ("acoustic_description", &desc_for_prompt),
            ("scene_descriptions", &scenes_for_prompt),
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
        .map(|s| normalize_document_name(s, "video-transcript"))
        .unwrap_or_else(|| {
            normalize_document_name(file_name.unwrap_or("video"), "video-transcript")
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
        "video_metadata": media_info,
        "model": cfg.summarizer_model,
    });
    if !transcript.is_empty() {
        contents["transcript"] = Value::String(transcript.clone());
    }
    if !segments.is_empty() {
        contents["segments"] = Value::Array(segments.clone());
    }
    if !scene_captions.is_empty() {
        contents["scene_captions"] = Value::Array(scene_captions);
    }
    if let Some(desc) = &description {
        contents["description"] = Value::String(desc.clone());
    }

    let mut notes: Vec<String> = Vec::new();
    if transcript.is_empty() {
        notes.push("Transcription unavailable (whisper failed)".to_string());
    } else if transcript.trim().is_empty() {
        notes.push("Transcription is empty".to_string());
    }
    if let Some(mn) = metadata_notes {
        notes.push(mn);
    }

    Ok(json!({
        "kind": "video-transcript",
        "document_name": document_name,
        "title": title,
        "summary": summary,
        "author": Value::Null,
        "contents": contents,
        "notes": notes,
    }))
}

async fn ffmpeg_extract_frames(path: &Path, max_frames: usize) -> Result<Vec<(u64, Vec<u8>)>, String> {
    const INTERVAL_SECS: f64 = 10.0;
    let fps_filter = format!("fps=1/{},scale=640:-1", INTERVAL_SECS as u32);

    let mut raw_bytes: Vec<u8> = Vec::new();
    let mut cmd = FfmpegCommand::new();
    cmd.input(path.to_string_lossy().as_ref())
        .args(["-vf", &fps_filter])
        .format("image2pipe")
        .args(["-vcodec", "mjpeg"])
        .output("pipe:1");
    let mut child = cmd.spawn().map_err(|e| format!("ffmpeg extract frames spawn: {e}"))?;
    let iter = child.iter().map_err(|e| format!("ffmpeg extract frames iter: {e}"))?;
    for event in iter {
        if let FfmpegEvent::OutputChunk(chunk) = event {
            raw_bytes.extend_from_slice(&chunk);
        }
        if solx_package_log::cancelled().await {
            return Err("cancelled".to_string());
        }
    }

    let mut frames: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut pos = 0usize;
    let mut frame_index = 0u64;
    while pos + 2 < raw_bytes.len() && frames.len() < max_frames {
        if raw_bytes[pos] != 0xFF || raw_bytes[pos + 1] != 0xD8 {
            pos += 1;
            continue;
        }
        let search_start = pos + 2;
        let eoi_pos = raw_bytes[search_start..]
            .windows(2)
            .position(|w| w == [0xFF, 0xD9])
            .map(|p| search_start + p + 2);
        if let Some(end) = eoi_pos {
            let jpeg = raw_bytes[pos..end].to_vec();
            let ts_ms = (frame_index as f64 * INTERVAL_SECS * 1000.0) as u64;
            frames.push((ts_ms, jpeg));
            frame_index += 1;
            pos = end;
        } else {
            break;
        }
    }
    Ok(frames)
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

// Silence dead-code warnings for re-exports from audio.
#[allow(dead_code)]
fn _re_exports() {
    let _: PathBuf = PathBuf::new();
}
