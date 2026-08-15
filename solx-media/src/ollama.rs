//! Thin Ollama HTTP client. Mirrors `sol-manager/src/providers/ollama.rs` for
//! the two functions solx-media needs (`generate` and `generate_with_image`).
//! All other model-provider abstractions (provider switch, mock responses,
//! retry, etc.) live in the manager and aren't replicated here — solx-media
//! talks to Ollama directly.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<&'a str>>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct GenerateResponse {
    response: String,
}

/// `POST {base_url}/api/generate` with `{model, prompt, stream:false}` and
/// return the `response` field.
pub async fn generate(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    prompt: &str,
) -> Result<String, String> {
    let url = format!("{}/api/generate", base_url.trim_end_matches('/'));
    let body = GenerateRequest {
        model,
        prompt,
        images: None,
        stream: false,
    };
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("ollama request failed: {e}"))?;
    let resp = resp
        .error_for_status()
        .map_err(|e| format!("ollama returned error: {e}"))?;
    let parsed: GenerateResponse = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse ollama response: {e}"))?;
    Ok(parsed.response)
}

/// `POST {base_url}/api/generate` with `{model, prompt, images:[b64], stream:false}`.
pub async fn generate_with_image(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    prompt: &str,
    image_base64: &str,
) -> Result<String, String> {
    let url = format!("{}/api/generate", base_url.trim_end_matches('/'));
    let body = GenerateRequest {
        model,
        prompt,
        images: Some(vec![image_base64]),
        stream: false,
    };
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("ollama request failed: {e}"))?;
    let resp = resp
        .error_for_status()
        .map_err(|e| format!("ollama returned error: {e}"))?;
    let parsed: GenerateResponse = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse ollama response: {e}"))?;
    Ok(parsed.response)
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
