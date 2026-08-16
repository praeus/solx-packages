//! Thin Ollama HTTP client. Mirrors `sol-manager/src/providers/ollama.rs` for
//! the two functions solx-media needs (`generate` and `generate_with_image`).
//! All other model-provider abstractions (provider switch, mock responses,
//! retry, etc.) live in the manager and aren't replicated here — solx-media
//! talks to Ollama directly.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<&'a str>>,
    stream: bool,
}

/// `POST {base_url}/api/generate` with `{model, prompt, stream:true}` and
/// return the accumulated `response` text. Streaming is on so the caller can
/// observe progress and so a `solx_package_log::cancelled()` check can abort
/// mid-generation rather than only after the full response arrives.
pub async fn generate(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    prompt: &str,
) -> Result<String, String> {
    generate_streaming(client, base_url, model, prompt, None).await
}

/// `POST {base_url}/api/generate` with `{model, prompt, images:[b64], stream:true}`.
pub async fn generate_with_image(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    prompt: &str,
    image_base64: &str,
) -> Result<String, String> {
    generate_streaming(client, base_url, model, prompt, Some(vec![image_base64])).await
}

/// Shared streaming body for [`generate`] and [`generate_with_image`].
///
/// Ollama's `/api/generate` with `stream: true` emits one NDJSON object per
/// line, each carrying an incremental `response` fragment and a `done` flag
/// (the final line has `done: true` and an empty `response`). We read the
/// body incrementally with [`reqwest::Response::chunk`], split on newlines,
/// and concatenate the `response` fragments. Between chunks we poll
/// [`solx_package_log::cancelled`] so a detached `action_stop` aborts the
/// generation instead of waiting for the whole response.
async fn generate_streaming(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    prompt: &str,
    images: Option<Vec<&str>>,
) -> Result<String, String> {
    let url = format!("{}/api/generate", base_url.trim_end_matches('/'));
    let body = GenerateRequest {
        model,
        prompt,
        images,
        stream: true,
    };
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("ollama request failed: {e}"))?;
    let mut resp = resp
        .error_for_status()
        .map_err(|e| format!("ollama returned error: {e}"))?;

    let mut full = String::new();
    let mut buf = String::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("ollama stream failed: {e}"))?
    {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parsed: Value = serde_json::from_str(line).unwrap_or(Value::Null);
            if let Some(err) = parsed.get("error").and_then(Value::as_str) {
                return Err(format!("ollama stream error: {err}"));
            }
            if let Some(r) = parsed.get("response").and_then(Value::as_str) {
                full.push_str(r);
            }
        }
        if solx_package_log::cancelled().await {
            solx_package_log::warn("ollama generation cancelled").await;
            return Err("cancelled".to_string());
        }
    }
    // A final fragment may arrive without a trailing newline.
    if !buf.trim().is_empty() {
        if let Ok(parsed) = serde_json::from_str::<Value>(buf.trim()) {
            if let Some(r) = parsed.get("response").and_then(Value::as_str) {
                full.push_str(r);
            }
        }
    }
    Ok(full)
}

/// Parse a model response as JSON. Ollama sometimes wraps the JSON in
/// ```json ... ``` fences or returns the raw text — best-effort extraction.
pub fn parse_llm_json(text: &str) -> Option<Value> {
    let trimmed = text.trim();

    // Strip ```json fences if present.
    let inner = if let Some(rest) = trimmed.strip_prefix("```json") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else if let Some(rest) = trimmed.strip_prefix("```") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        trimmed
    };

    if let Ok(v) = serde_json::from_str::<Value>(inner) {
        return Some(v);
    }
    // Fallback: try to find the first '{' and matching '}' substring.
    if let Some(start) = inner.find('{') {
        if let Some(end) = inner.rfind('}') {
            if end > start {
                if let Ok(v) = serde_json::from_str::<Value>(&inner[start..=end]) {
                    return Some(v);
                }
            }
        }
    }
    None
}
