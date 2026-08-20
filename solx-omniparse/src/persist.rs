//! FU-2: persist the omniparse extraction result to solx-server via
//! `PUT /docs/{path}/{name}`. `kind: "preprocessed-text"`,
//! `contents: {"text": "<extracted>", "mime_type": "text/plain"}`.
//!
//! Thin wrapper over `solx_package_log::persist::put_document`, which does
//! the actual HTTP work — shared with `solx-media`'s equivalent wrapper.
//! This module's job is just building the omniparse document body.
//!
//! Soft-fails on connection errors: the caller still gets the raw
//! `OmniparseResult` on stdout so a workflow can recover. See FU-2 plan in
//! `solx-packages/docs/media-actions.md`.

use base64::Engine as _;
use serde_json::json;

/// Persist an omniparse extraction result. Builds the Document payload
/// (with `kind: "preprocessed-text"`), PUTs it to its own URL, and returns
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

    // The permissive base document type rather than `MediaDocument`: the
    // latter's `kind` enum does not include `preprocessed-text`.
    let contents = json!({
        "kind": "preprocessed-text",
        "document_name": document_name,
        "contents": {
            "text": text_bytes_to_text_string(text_bytes),
            "text_bytes_base64": bytes_base64,
            "mime_type": mime_type,
        },
    });

    solx_package_log::persist::put_document(
        client,
        server_url,
        token,
        "/media/preprocessed-text",
        document_name,
        "/types/docs/Document",
        contents,
    )
    .await
}

/// Decode the extracted text as UTF-8 lossy for the human-readable `text`
/// field. Always succeeds; non-UTF-8 bytes become replacement characters.
fn text_bytes_to_text_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
