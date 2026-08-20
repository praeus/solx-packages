//! Save a `MediaDocument` to solx-server via `PUT /docs/{path}/{name}`. The
//! reference is the URL; the body is a bare `DocumentInput`, and the
//! response is the saved `Document`.
//!
//! Thin wrapper over `solx_package_log::persist::put_document`, which does
//! the actual HTTP work (URL building, percent-encoding, response
//! parsing) — shared with `solx-omniparse`'s equivalent wrapper. This
//! module's job is just extracting `kind`/`document_name` from the
//! MediaDocument and deriving the naming convention below.

use serde_json::Value;

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
    solx_package_log::persist::put_document(
        client,
        server_url,
        token,
        &path,
        document_name,
        "/builtin/types/MediaDocument",
        document.clone(),
    )
    .await
}
