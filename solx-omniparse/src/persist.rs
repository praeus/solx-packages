//! FU-2: persist the omniparse extraction result to solx-server via
//! `POST /docs/save`. Sibling of solx-media's `persist.rs`; same shape,
//! tailored for the omniparse Document (`kind: "preprocessed-text"`,
//! `contents: {"text": "<extracted>", "mime_type": "text/plain"}`).
//!
//! Soft-fails on connection errors: the caller still gets the raw
//! `OmniparseResult` on stdout so a workflow can recover. See FU-2 plan in
//! `solx-packages/docs/media-actions.md`.

use base64::Engine as _;
use serde_json::{json, Value};

/// Persist an omniparse extraction result. Builds the Document payload
/// (with `kind: "preprocessed-text"`), POSTs to `/docs/save`, and returns
/// the saved `(path, name)` on 2xx. Surfaces the API error on non-2xx.
///
/// Naming: `/media/preprocessed-text/{document_name}`. `document_name`
/// defaults to the source file's stem.
pub async fn save_document(
    client: &reqwest::Client,
    server_url: &str,
    token: &str,
    document_name: &str,
    mime_type: &str,
    text_bytes: &[u8],
) -> Result<(String, String), String> {
    let bytes_base64 = base64::engine::general_purpose::STANDARD.encode(text_bytes);
    let path = "/media/preprocessed-text".to_string();

    let body = json!({
        "path": path,
        "name": document_name,
        "document": {
            "kind": "preprocessed-text",
            "document_name": document_name,
            "contents": {
                "text": text_bytes_to_text_string(text_bytes),
                "text_bytes_base64": bytes_base64,
                "mime_type": mime_type,
            },
        }
    });

    let url = format!("{}/docs/save", server_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("docs/save request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "<no body>".to_string());
        return Err(format!("docs/save returned HTTP {status}: {body}"));
    }

    let parsed: Value = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse docs/save response: {e}"))?;

    let saved_path = parsed
        .get("saved_path")
        .or_else(|| parsed.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or(&path)
        .to_string();
    let saved_name = parsed
        .get("saved_name")
        .or_else(|| parsed.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or(document_name)
        .to_string();

    Ok((saved_path, saved_name))
}

/// Decode the extracted text as UTF-8 lossy for the human-readable `text`
/// field. Always succeeds; non-UTF-8 bytes become replacement characters.
fn text_bytes_to_text_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
