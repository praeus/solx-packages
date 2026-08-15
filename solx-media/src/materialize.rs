//! HTML rich-text media materializer. Ported from
//! `sol-manager/src/extraction/media.rs` (the unconditioned helpers + data
//! types). Walks a RichTextDoc, fetches every embedded `<img>` / `<video>`
//! src from a data: URL, http(s) URL, or relative path, replaces each src
//! with an `artifacts/<name>` reference, and returns the embedded artifacts
//! ready to be attached to the parent document.
//!
//! The input/output types are intentionally simple local structs (not the
//! old sol-core::extract::* hierarchy). The downstream caller (vision.rs /
//! the materialize-html action) attaches the resulting artifacts to a
//! `MediaDocument`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct MaterializedMediaAsset {
    pub content_type: String,
    pub data: String, // base64-encoded
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RichTextNodeAttrs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RichTextNode {
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default, skip_serializing_if = "is_empty_attrs")]
    pub attrs: RichTextNodeAttrs,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<RichTextNode>,
}

fn is_empty_attrs(a: &RichTextNodeAttrs) -> bool {
    a.src.is_none() && a.alt.is_none() && a.title.is_none() && a.poster.is_none()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RichTextDoc {
    #[serde(rename = "type", default = "default_doc_type")]
    pub node_type: String,
    #[serde(default)]
    pub content: Vec<RichTextNode>,
}

fn default_doc_type() -> String {
    "doc".to_string()
}

impl RichTextDoc {
    /// Walk every media node (`image` / `video`) and invoke `f` with
    /// `&mut RichTextNodeAttrs` so the caller can rewrite the src in place.
    pub fn visit_media_nodes_mut<F: FnMut(&str, &mut RichTextNodeAttrs)>(
        &mut self,
        mut f: F,
    ) {
        for node in &mut self.content {
            visit_media_node_mut(node, &mut f);
        }
    }
}

fn visit_media_node_mut<F: FnMut(&str, &mut RichTextNodeAttrs)>(
    node: &mut RichTextNode,
    f: &mut F,
) {
    if matches!(node.node_type.as_str(), "image" | "video") {
        f(&node.node_type, &mut node.attrs);
    }
    for child in &mut node.content {
        visit_media_node_mut(child, f);
    }
}

/// Walk every media node and load its src as a MaterializedMediaAsset. Each
/// unique src is loaded at most once; subsequent duplicates in the same doc
/// are skipped. Returns the dedup'd artifacts in encounter order.
pub async fn extract_embedded_artifacts(
    doc: &mut RichTextDoc,
    base_name: &str,
    source_url: Option<&str>,
    source_path: Option<&str>,
) -> Vec<MaterializedMediaAsset> {
    let mut sources = Vec::new();
    doc.visit_media_nodes_mut(|node_type, attrs| {
        if let Some(src) = attrs.src.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            sources.push((node_type.to_string(), src.to_string()));
        }
    });

    let mut artifacts = Vec::new();
    let mut source_to_filename: HashMap<String, String> = HashMap::new();

    for (index, (node_type, src)) in sources.into_iter().enumerate() {
        if source_to_filename.contains_key(&src) {
            continue;
        }
        match load_media_asset(&src, source_url, source_path).await {
            Ok(asset) => {
                let filename =
                    derive_media_artifact_name(base_name, &src, index, &asset.content_type, &node_type);
                source_to_filename.insert(src.clone(), filename.clone());
                artifacts.push(MaterializedMediaAsset {
                    content_type: asset.content_type,
                    data: asset.data,
                });
            }
            Err(err) => {
                solx_package_log::warn(&format!("skipped '{src}': {err}")).await;
            }
        }
    }

    // Rewrite srcs in place to point at the artifact files.
    doc.visit_media_nodes_mut(|_, attrs| {
        if let Some(src) = attrs.src.as_mut() {
            if let Some(filename) = source_to_filename.get(src) {
                *src = format!("artifacts/{filename}");
            }
        }
    });

    artifacts
}

async fn load_media_asset(
    src: &str,
    source_url: Option<&str>,
    source_path: Option<&str>,
) -> Result<MaterializedMediaAsset, String> {
    if let Some(data_part) = src.strip_prefix("data:") {
        let (content_type, data) = parse_data_url(data_part)?;
        return Ok(MaterializedMediaAsset { content_type, data });
    }
    if src.starts_with("http://") || src.starts_with("https://") {
        return fetch_remote_media(src.to_string()).await;
    }
    if let Some(base_url) = source_url {
        if let Ok(base) = url::Url::parse(base_url) {
            if let Ok(joined) = base.join(src) {
                return fetch_remote_media(joined.to_string()).await;
            }
        }
    }
    if let Some(path) = source_path {
        let base = Path::new(path);
        let candidate: PathBuf = if Path::new(src).is_absolute() {
            PathBuf::from(src)
        } else {
            base.parent().unwrap_or_else(|| Path::new("")).join(src)
        };
        if candidate.exists() {
            let bytes = std::fs::read(&candidate)
                .map_err(|err| format!("failed reading media '{}': {err}", candidate.display()))?;
            return Ok(MaterializedMediaAsset {
                content_type: content_type_from_path(&candidate),
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            });
        }
    }
    Err("unsupported media source".to_string())
}

fn parse_data_url(data_part: &str) -> Result<(String, String), String> {
    let (content_type_part, payload) = data_part
        .split_once(';')
        .ok_or_else(|| "invalid data URL".to_string())?;
    let data = payload
        .strip_prefix("base64,")
        .ok_or_else(|| "expected base64 data URL".to_string())?;
    Ok((content_type_part.trim().to_string(), data.to_string()))
}

async fn fetch_remote_media(url: String) -> Result<MaterializedMediaAsset, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|err| format!("failed to build HTTP client: {err}"))?;

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;
    let response = response
        .error_for_status()
        .map_err(|err| format!("request failed: {err}"))?;

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or(value).trim().to_string())
        .unwrap_or_else(|| content_type_from_url(&url));

    let bytes = response
        .bytes()
        .await
        .map_err(|err| format!("failed to read response body: {err}"))?;

    if bytes.len() > 10 * 1024 * 1024 {
        return Err("media asset exceeds 10 MiB limit".to_string());
    }

    Ok(MaterializedMediaAsset {
        content_type,
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

/// Public entry point for the materialize-html action: read JSON on stdin,
/// walk the document's rich_text field (if present), materialize every
/// embedded asset, return the updated document + new artifacts as a
/// MediaDocument-shaped value.
pub async fn run_materialize_html(
    source_path: Option<&str>,
    source_url: Option<&str>,
    input: &Value,
) -> Result<Value, String> {
    // Extract (or build) the rich_text doc from the input payload.
    let rich_text_json = input
        .get("rich_text")
        .cloned()
        .ok_or_else(|| "input missing 'rich_text' field".to_string())?;
    let mut doc: RichTextDoc = serde_json::from_value(rich_text_json)
        .map_err(|e| format!("failed to parse rich_text: {e}"))?;

    let base_name = input
        .get("document_name")
        .and_then(|v| v.as_str())
        .unwrap_or("materialized");
    let artifacts = extract_embedded_artifacts(&mut doc, base_name, source_url, source_path).await;

    let mut output = input.clone();
    output["rich_text"] = serde_json::to_value(&doc).map_err(|e| format!("serialize: {e}"))?;
    let arts_value = serde_json::to_value(
        artifacts
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let name = derive_media_artifact_name(
                    base_name,
                    "", // no src — derive from base_name + index
                    i,
                    &a.content_type,
                    "image",
                );
                serde_json::json!({
                    "name": name,
                    "content_type": a.content_type,
                    "data": a.data,
                })
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|e| format!("serialize: {e}"))?;
    output["artifacts"] = arts_value;
    output["kind"] = Value::String("materialized-html".to_string());
    Ok(output)
}

// ---------------------------------------------------------------------------
// Content-type helpers
// ---------------------------------------------------------------------------

fn extension_from_content_type(content_type: &str) -> &str {
    match content_type {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/ogg" => "ogv",
        "video/quicktime" => "mov",
        _ => "bin",
    }
}

fn derive_media_artifact_name(
    base_name: &str,
    src: &str,
    index: usize,
    content_type: &str,
    node_type: &str,
) -> String {
    let ext = extension_from_content_type(content_type);
    let src_name = src
        .split('?')
        .next()
        .unwrap_or(src)
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let stem = if src_name.is_empty() {
        normalize_identifier(&format!("{base_name}-{node_type}-{}", index + 1), "media")
    } else if let Some((raw_stem, _)) = src_name.rsplit_once('.') {
        normalize_identifier(raw_stem, "media")
    } else {
        normalize_identifier(src_name, "media")
    };
    format!("{stem}.{ext}")
}

fn normalize_identifier(input: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
}

fn content_type_from_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.path_segments().and_then(|s| s.last().map(str::to_string)))
        .map(|name| content_type_from_name(&name))
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

fn content_type_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(content_type_from_name)
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

fn content_type_from_name(name: &str) -> String {
    match name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()) {
        Some(ext) if ext == "png" => "image/png".to_string(),
        Some(ext) if ext == "jpg" || ext == "jpeg" => "image/jpeg".to_string(),
        Some(ext) if ext == "gif" => "image/gif".to_string(),
        Some(ext) if ext == "webp" => "image/webp".to_string(),
        Some(ext) if ext == "svg" => "image/svg+xml".to_string(),
        Some(ext) if ext == "bmp" => "image/bmp".to_string(),
        Some(ext) if ext == "mp4" => "video/mp4".to_string(),
        Some(ext) if ext == "webm" => "video/webm".to_string(),
        Some(ext) if ext == "ogv" || ext == "ogg" => "video/ogg".to_string(),
        Some(ext) if ext == "mov" => "video/quicktime".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_lookup() {
        assert_eq!(extension_from_content_type("image/png"), "png");
        assert_eq!(extension_from_content_type("image/jpeg"), "jpg");
        assert_eq!(extension_from_content_type("image/jpg"), "jpg");
        assert_eq!(extension_from_content_type("video/mp4"), "mp4");
        assert_eq!(extension_from_content_type("unknown/thing"), "bin");
    }

    #[test]
    fn normalize_basic() {
        assert_eq!(normalize_identifier("My Cat Photo!", "media"), "my-cat-photo");
        assert_eq!(normalize_identifier("file_name.jpg", "media"), "file-name-jpg");
        assert_eq!(normalize_identifier("", "media"), "media");
        assert_eq!(normalize_identifier("   ", "media"), "media");
    }

    #[test]
    fn derive_name_with_src() {
        let n = derive_media_artifact_name("page", "https://example.com/cat.jpg", 0, "image/jpeg", "image");
        assert_eq!(n, "cat.jpg");
    }

    #[test]
    fn derive_name_no_src() {
        let n = derive_media_artifact_name("page", "", 2, "image/png", "image");
        assert_eq!(n, "page-image-3.png");
    }

    #[test]
    fn content_type_from_extension() {
        assert_eq!(content_type_from_name("cat.PNG"), "image/png");
        assert_eq!(content_type_from_name("video.mp4"), "video/mp4");
        assert_eq!(content_type_from_name("no_ext"), "application/octet-stream");
    }
}
