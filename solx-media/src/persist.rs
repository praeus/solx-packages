//! Save a `MediaDocument` to solx-server via `POST /docs/save`. Mirrors the
//! `entity_save_document` built-in: pass `{path, name, document, ...}` and
//! the server returns `{saved_path, saved_name}`.
//!
//! This is the action's responsibility in v1 (FU-1 will add a
//! return-documents mode in a followup).

use serde_json::{json, Value};

/// Persist a MediaDocument to solx-server. The `document_name` field of the
/// MediaDocument becomes the document's name; the kind becomes the directory
/// under `/media/`. Returns `(path, name)` on 2xx; surfaces the server's
/// error body on non-2xx.
///
/// Naming: `/media/{kind}/{document_name}`. E.g. a `kind: "image-text"` doc
/// with `document_name: "cat-photo"` lands at `/media/image-text/cat-photo`.
pub async fn save_document(
    client: &reqwest::Client,
    server_url: &str,
    token: &str,
    document: &Value,
) -> Result<(String, String), String> {
    let kind = document
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "MediaDocument missing 'kind' field".to_string())?;
    let document_name = document
        .get("document_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "MediaDocument missing 'document_name' field".to_string())?;

    let path = format!("/media/{kind}");
    let body = json!({
        "path": path,
        "name": document_name,
        "document": document,
    });

    let url = format!(
        "{}/docs/save",
        server_url.trim_end_matches('/')
    );

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
