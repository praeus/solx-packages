//! Sol ↔ Google Workspace JSON converters.
//!
//! Ported from the old `sol-google-actions` crate (Rust + Python) onto
//! solx-core's `custom-action` WIT world. The single compiled WASM binary
//! hosts 8 converters, dispatched by `fn_name` (the `action_name` argument
//! to the WIT `runner.run` export).
//!
//! ## Conversions hosted in this binary
//!
//! | fn_name | Direction |
//! |---|---|
//! | `convert-sol-doc-to-google-doc` | Sol → Google Docs (Tiptap → batchUpdate) |
//! | `convert-google-doc-to-sol-doc` | Google Docs → Sol |
//! | `convert-sol-doc-to-gmail-message` | Sol → Gmail (RFC 2822 base64url) |
//! | `convert-gmail-message-to-sol-doc` | Gmail → Sol (MIME parts → plain text) |
//! | `convert-sol-doc-to-google-task` | Sol → Google Tasks |
//! | `convert-google-task-to-sol-doc` | Google Tasks → Sol |
//! | `convert-sol-doc-to-calendar-event` | Sol → Google Calendar |
//! | `convert-calendar-event-to-sol-doc` | Google Calendar → Sol |
//! | `upload-documents-to-google-docs` | Batch: list Sol docs under a path, convert, and create+populate a Google Doc for each |
//!
//! Build target: `cargo build --release --target wasm32-wasip2`
//!
//! ## WIT contract
//!
//! Implements the `custom-action` world from `solx-core/solx-wasm/wit/`:
//! imports `action-exec` (recursive action calls), `artifact-read`
//! (file-store reads), `logger`; exports `runner.run(action-name, params)`
//! returning `action-result`.

use std::collections::HashSet;

use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};
use serde_json::{json, Value};

wit_bindgen::generate!({
    world: "custom-action",
    path: "wit",
});

use exports::sol::actions::runner::{ActionResult, Guest};

const DEFAULT_TYPE_NAME: &str = "RichTextDocument";
const DEFAULT_TITLE: &str = "Imported Google Doc";
const RICH_TEXT_FIELD_CANDIDATES: &[&str] = &["rich_text", "content"];
const INTERNAL_LINK_PREFIXES: &[&str] = &["documents/", "actions/", "artifacts/"];

struct SolxGoogleActions;

impl Guest for SolxGoogleActions {
    fn run(action_name: Option<String>, params: String) -> Result<ActionResult, String> {
        let fn_name = action_name.as_deref().unwrap_or("");
        match fn_name {
            "convert-sol-doc-to-google-doc" => convert_sol_to_google_doc(&params),
            "convert-google-doc-to-sol-doc" => convert_google_doc_to_sol(&params),
            "convert-sol-doc-to-gmail-message" => convert_sol_to_gmail(&params),
            "convert-gmail-message-to-sol-doc" => convert_gmail_to_sol(&params),
            "convert-sol-doc-to-google-task" => convert_sol_to_google_task(&params),
            "convert-google-task-to-sol-doc" => convert_google_task_to_sol(&params),
            "convert-sol-doc-to-calendar-event" => convert_sol_to_calendar_event(&params),
            "convert-calendar-event-to-sol-doc" => convert_calendar_event_to_sol(&params),
            "upload-documents-to-google-docs" => upload_documents_to_google_docs(&params),
            other => Err(format!("unknown action name '{other}'")),
        }
    }
}

export!(SolxGoogleActions);

// ── Action-result helpers ──────────────────────────────────────────────────

fn ok(output: Value) -> Result<ActionResult, String> {
    Ok(ActionResult {
        success: true,
        message: None,
        output: Some(output.to_string()),
    })
}

// ── convert-sol-doc-to-google-doc ─────────────────────────────────────────────

fn convert_sol_to_google_doc(params: &str) -> Result<ActionResult, String> {
    let input: Value = serde_json::from_str(params)
        .map_err(|e| format!("invalid JSON params: {e}"))?;

    let source_doc = resolve_sol_document(&input)?;
    let default_title = input.get("default_title").and_then(Value::as_str);
    let text_hint = input.get("text_field_hint").and_then(Value::as_str);
    let rich_text_hint = input.get("rich_text_field_hint").and_then(Value::as_str);

    let payload = build_google_doc_payload(&source_doc, default_title, text_hint, rich_text_hint, true);

    let request_count = payload
        .batch_update_body
        .get("requests")
        .and_then(|r| r.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let output = json!({
        "title": payload.title,
        "text": payload.text,
        "create_body": payload.create_body,
        "batch_update_body": payload.batch_update_body,
        "metadata": {
            "source_name": source_doc.get("name"),
            "text_field_hint": text_hint,
            "rich_text_field": payload.rich_text_field,
            "has_rich_text": payload.has_rich_text,
            "request_count": request_count,
        }
    });

    ok(output)
}

/// The reusable core of `convert-sol-doc-to-google-doc`: given an already-
/// resolved Sol document, produce everything needed to create the Drive
/// file and populate it (`create_body` for `create-google-file`,
/// `batch_update_body` for `post-google-doc`). Shared with
/// `upload_documents_to_google_docs`, which calls this once per document
/// instead of round-tripping through the standalone converter action.
struct GoogleDocPayload {
    title: String,
    text: String,
    create_body: Value,
    batch_update_body: Value,
    rich_text_field: Option<String>,
    has_rich_text: bool,
}

fn build_google_doc_payload(
    source_doc: &Value,
    default_title: Option<&str>,
    text_hint: Option<&str>,
    rich_text_hint: Option<&str>,
    include_icon: bool,
) -> GoogleDocPayload {
    let title = source_doc
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| default_title.filter(|s| !s.is_empty()).map(String::from))
        .unwrap_or_else(|| DEFAULT_TITLE.to_string());

    let contents = source_doc.get("contents").cloned().unwrap_or(Value::Null);

    let icon_relpath = contents.get("icon").and_then(resolve_icon_relpath);
    let comments: Vec<Value> = contents
        .get("comments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let (rich_text_doc, rich_text_field) = find_rich_text(&contents, rich_text_hint);
    let has_rich_text = rich_text_doc.is_some();

    // One continuous pass over a single converter/cursor: icon, then
    // body, then comments — so indices stay consistent across all three
    // instead of each assuming it starts at index 1.
    let mut converter = TiptapToGoogleDocsConverter::new();

    if include_icon {
        if let Some(relpath) = &icon_relpath {
            match upload_icon_and_get_public_url(relpath) {
                Ok(url) => converter.insert_inline_image_from_url(&url),
                Err(e) => {
                    sol::actions::logger::log(&format!(
                        "convert-sol-doc-to-google-doc: icon upload failed for '{relpath}': {e}; skipping icon"
                    ));
                }
            }
        }
    }

    let text = if let Some(rt_doc) = &rich_text_doc {
        let text = converter.flatten_to_text(rt_doc);
        converter.process_tiptap_document(rt_doc);
        text
    } else {
        let text = extract_text_from_sol_contents(&contents, text_hint).unwrap_or_default();
        converter.insert_plain_text_block(&text);
        text
    };

    if !comments.is_empty() {
        converter.render_comments_section(&comments);
    }

    let batch_update_body = json!({ "requests": converter.finish() });

    // Shaped to match GoogleDriveCreateFileParams (required: name, mimeType)
    // so this can be piped directly into create-google-file — `text` is
    // carried separately for the subsequent post-google-doc
    // batch_update_body step, not inside create_body, since Drive's create
    // endpoint doesn't recognize a "text" field.
    let create_body = json!({ "name": &title, "mimeType": "application/vnd.google-apps.document" });

    GoogleDocPayload {
        title,
        text,
        create_body,
        batch_update_body,
        rich_text_field,
        has_rich_text,
    }
}

// ── upload-documents-to-google-docs ───────────────────────────────────────────

/// Google Drive `appProperties` key tagging a created Google Doc with the
/// Sol document path it was generated from -- see `find_existing_google_doc`.
const SOURCE_PATH_PROPERTY: &str = "solx_source_path";

/// Escapes a value for embedding in a Drive `q` query string literal
/// (`'...'`-quoted). Per Drive's query syntax, both `\` and `'` need a
/// backslash escape inside the quotes.
fn escape_drive_query_value(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Looks up a previously-created Google Doc tagged with `doc_path` via
/// Drive's `appProperties` (set at creation time, see `create_tagged_google_doc`).
/// Self-healing by construction: if that file was deleted or untrashed
/// outside solx, the search just finds nothing -- there's no stale
/// reference to clean up, the caller just creates a fresh one.
fn find_existing_google_doc(doc_path: &str) -> Result<Option<String>, String> {
    let escaped = escape_drive_query_value(doc_path);
    let q = format!(
        "mimeType='application/vnd.google-apps.document' and trashed=false and \
         appProperties has {{ key='{SOURCE_PATH_PROPERTY}' and value='{escaped}' }}"
    );
    let result = exec_action_json(
        "/packages/solx-google/find-google-drive-folder",
        &json!({ "q": q, "fields": "files(id)", "pageSize": 1 }),
    )?;
    Ok(result
        .get("files")
        .and_then(Value::as_array)
        .and_then(|files| files.first())
        .and_then(|f| f.get("id"))
        .and_then(Value::as_str)
        .map(String::from))
}

/// Creates a new Google Doc tagged with `doc_path`, so a later run can find
/// and update it instead of creating a duplicate.
fn create_tagged_google_doc(create_body: &Value, doc_path: &str, parent_folder_id: Option<&str>) -> Result<String, String> {
    let mut body = create_body.clone();
    if let Value::Object(map) = &mut body {
        map.insert("appProperties".to_string(), json!({ SOURCE_PATH_PROPERTY: doc_path }));
        if let Some(parent) = parent_folder_id {
            map.insert("parents".to_string(), json!([parent]));
        }
    }
    let created = exec_action_json("/packages/solx-google/create-google-file", &body)?;
    created
        .get("id")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| "create-google-file response missing 'id'".to_string())
}

/// Builds the `deleteContentRange` request that clears an existing
/// document's body before reposting fresh content into it -- reposting
/// insert-only requests into a non-empty doc would misalign or prepend
/// instead of replacing, since Docs indices assume a doc starting empty.
/// `google_doc` is a `get-google-doc` response; `body.content`'s last
/// element's `endIndex` is the document's total end index. Docs disallows
/// deleting the final implicit newline, hence `- 1`. Returns `None` for a
/// doc that's already empty (nothing to clear -- e.g. a tagged doc whose
/// prior post-google-doc never got past creation).
fn compute_clear_range(google_doc: &Value) -> Option<Value> {
    let total_end_index = google_doc
        .get("body")?
        .get("content")?
        .as_array()?
        .last()?
        .get("endIndex")?
        .as_i64()?;
    if total_end_index <= 2 {
        return None;
    }
    Some(json!({
        "deleteContentRange": { "range": { "startIndex": 1, "endIndex": total_end_index - 1 } }
    }))
}

/// Finishes a `batch_update_body` (`{"requests": [...]}`) for `post-google-doc`:
/// prepends the clear-range request when reposting into an existing doc, and
/// merges in `documentId` (see the comment at its call sites for why that's
/// not nested under a `body` key).
fn finalize_post_body(mut body: Value, doc_id: &str, clear_request: Option<&Value>) -> Value {
    if let Value::Object(map) = &mut body {
        if let (Some(Value::Array(requests)), Some(clear)) = (map.get_mut("requests"), clear_request) {
            requests.insert(0, clear.clone());
        }
        map.insert("documentId".to_string(), json!(doc_id));
    }
    body
}

fn upload_documents_to_google_docs(params: &str) -> Result<ActionResult, String> {
    let input: Value = serde_json::from_str(params)
        .map_err(|e| format!("invalid JSON params: {e}"))?;

    let path_prefix = input
        .get("path_prefix")
        .and_then(Value::as_str)
        .ok_or_else(|| "path_prefix is required".to_string())?;
    let count = input
        .get("count")
        .and_then(Value::as_u64)
        .ok_or_else(|| "count is required and must be a positive integer".to_string())?;
    let order = input.get("order").and_then(Value::as_str).unwrap_or("desc");
    let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let parent_folder_id = input.get("parent_folder_id").and_then(Value::as_str);

    let list_result = exec_action_json(
        "/builtin/document/entity_list_documents",
        &json!({
            "pathPrefix": path_prefix,
            "limit": count,
            "offset": offset,
            "sortBy": "updated_at",
            "sortOrder": order,
        }),
    )?;

    let items = list_result
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let found = items.len();

    sol::actions::logger::log(&format!(
        "upload-documents-to-google-docs: found {found} document(s) under '{path_prefix}' (requested {count}, offset {offset})"
    ));

    let mut uploaded: Vec<Value> = Vec::new();

    for (i, doc) in items.iter().enumerate() {
        let doc_path = doc.get("path").and_then(Value::as_str).unwrap_or("");
        let doc_name = doc.get("name").and_then(Value::as_str).unwrap_or("");
        // `doc_path` alone is just the *folder* -- every sibling document
        // under the same path_prefix shares it (Sol documents split
        // path/name the same way actions do). The tag/search key and the
        // result's sol_document_path both need the full, per-document
        // identifier, or every doc in one folder collapses onto the same
        // Google Doc (confirmed live: doc 2 and 3 overwrote doc 1's content
        // instead of getting their own).
        let full_doc_path = format!("{doc_path}/{doc_name}");
        let progress = format!("[{}/{found}] '{full_doc_path}'", i + 1);

        sol::actions::logger::log(&format!("upload-documents-to-google-docs: {progress}: starting"));

        let upload_result = (|| -> Result<Value, String> {
            let payload = build_google_doc_payload(doc, None, None, None, true);

            // Reuse a previously-created doc tagged with this Sol document's
            // path (see find_existing_google_doc) instead of always
            // creating a new one. A tag match that turns out unreadable
            // (deleted/trashed outside solx since the search, etc.) just
            // falls back to creating fresh rather than failing the document.
            sol::actions::logger::log(&format!("upload-documents-to-google-docs: {progress}: searching for an existing tagged doc"));
            let existing_doc_id = find_existing_google_doc(&full_doc_path)?;
            let (doc_id, action_kind, clear_request) = if let Some(id) = existing_doc_id {
                sol::actions::logger::log(&format!(
                    "upload-documents-to-google-docs: {progress}: found existing doc '{id}', fetching its current content"
                ));
                match exec_action_json("/packages/solx-google/get-google-doc", &json!({ "documentId": id })) {
                    Ok(existing_doc) => (id, "updated", compute_clear_range(&existing_doc)),
                    Err(e) => {
                        sol::actions::logger::log(&format!(
                            "upload-documents-to-google-docs: {progress}: tagged doc '{id}' \
                             could not be read ({e}); creating a new one"
                        ));
                        (
                            create_tagged_google_doc(&payload.create_body, &full_doc_path, parent_folder_id)?,
                            "created",
                            None,
                        )
                    }
                }
            } else {
                sol::actions::logger::log(&format!(
                    "upload-documents-to-google-docs: {progress}: no existing tagged doc found, creating a new one"
                ));
                (
                    create_tagged_google_doc(&payload.create_body, &full_doc_path, parent_folder_id)?,
                    "created",
                    None,
                )
            };

            // batch_update_body is already `{"requests": [...]}` -- Google's
            // batchUpdate endpoint expects that shape as the raw request
            // body, so documentId (and, when reposting, the clear-range
            // request) are merged in directly rather than nested under a
            // "body" key (which the endpoint would reject as an
            // unrecognized field).
            let post_body = finalize_post_body(payload.batch_update_body, &doc_id, clear_request.as_ref());
            sol::actions::logger::log(&format!(
                "upload-documents-to-google-docs: {progress}: posting content to doc '{doc_id}'"
            ));
            if let Err(e) = exec_action_json("/packages/solx-google/post-google-doc", &post_body) {
                // The icon is the only thing in this batch backed by a
                // hotlinked (rather than caller-supplied) URL, and Drive
                // hotlinks are known to be unreliable for Google's own
                // server-to-server image fetch (see upload_icon_and_get_public_url).
                // Since batchUpdate is all-or-nothing, one flaky icon would
                // otherwise sink the whole document's text/formatting.
                // Best-effort: on an image-fetch complaint specifically,
                // rebuild without the icon and retry once before giving up.
                if e.contains("insertInlineImage") {
                    sol::actions::logger::log(&format!(
                        "upload-documents-to-google-docs: {progress}: icon could not be embedded \
                         ({e}); retrying without the icon"
                    ));
                    let retry_payload = build_google_doc_payload(doc, None, None, None, false);
                    let retry_body = finalize_post_body(retry_payload.batch_update_body, &doc_id, clear_request.as_ref());
                    exec_action_json("/packages/solx-google/post-google-doc", &retry_body)?;
                } else {
                    return Err(e);
                }
            }
            sol::actions::logger::log(&format!("upload-documents-to-google-docs: {progress}: done"));

            Ok(json!({
                "sol_document_path": full_doc_path,
                "sol_document_name": doc_name,
                "title": payload.title,
                "google_doc_id": doc_id,
                "google_doc_url": format!("https://docs.google.com/document/d/{doc_id}/edit"),
                "action": action_kind,
            }))
        })();

        match upload_result {
            Ok(entry) => uploaded.push(entry),
            Err(e) => {
                return Ok(ActionResult {
                    success: false,
                    message: Some(format!("failed at document '{full_doc_path}': {e}")),
                    output: Some(
                        json!({
                            "requested": count,
                            "found": found,
                            "uploaded": uploaded,
                        })
                        .to_string(),
                    ),
                });
            }
        }
    }

    ok(json!({
        "requested": count,
        "found": found,
        "uploaded": uploaded,
    }))
}

// ── convert-google-doc-to-sol-doc ─────────────────────────────────────────────

fn convert_google_doc_to_sol(params: &str) -> Result<ActionResult, String> {
    let input: Value = serde_json::from_str(params)
        .map_err(|e| format!("invalid JSON params: {e}"))?;

    let google_doc = input
        .get("google_doc")
        .ok_or_else(|| "google_doc is required and must be a JSON object".to_string())?;

    let source_document_id = google_doc.get("documentId").and_then(Value::as_str);
    let source_title = google_doc.get("title").and_then(Value::as_str);
    let text = extract_text_from_google_doc(google_doc);

    let suggested_name = input
        .get("output_document_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| {
            normalize_document_name(source_title.unwrap_or(DEFAULT_TITLE))
        });

    let output_type = input
        .get("output_type_name")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_TYPE_NAME);

    let sol_payload = json!({
        "name": suggested_name,
        "type_name": output_type,
        "title": source_title,
        "contents": {
            "text": text,
            "source": {
                "provider": "google-docs",
                "document_id": source_document_id,
            }
        }
    });

    let output = json!({
        "source_document_id": source_document_id,
        "source_title": source_title,
        "text": text,
        "suggested_document_name": suggested_name,
        "sol_document_payload": sol_payload,
        "metadata": { "output_type_name": output_type },
    });

    ok(output)
}

// ── Document resolution ──────────────────────────────────────────────────────

fn resolve_sol_document(input: &Value) -> Result<Value, String> {
    if let Some(doc) = input.get("sol_document") {
        if doc.is_object() {
            return Ok(doc.clone());
        }
        return Err("sol_document must be an object when provided".to_string());
    }

    // Defensive: some plans bind a full document into sol_document_name.
    if let Some(doc) = input.get("sol_document_name") {
        if doc.is_object() {
            return Ok(doc.clone());
        }
    }

    let name = input
        .get("sol_document_name")
        .and_then(Value::as_str)
        .ok_or_else(|| "provide sol_document or sol_document_name".to_string())?;

    // Recursive call into /builtin/document/entity_get_document. The solx-core
    // custom-action WIT world exposes `action-exec.exec` as a synchronous
    // import (no `.await`); see the WIT for details.
    let payload = json!({ "name": name });
    exec_action_json("/builtin/document/entity_get_document", &payload)
}

fn exec_action_json(action_name: &str, payload: &Value) -> Result<Value, String> {
    let response = sol::actions::action_exec::exec(action_name, &payload.to_string())
        .map_err(|e| format!("action '{action_name}' failed: {e}"))?;

    if !response.success {
        return Err(format!(
            "action '{action_name}' reported failure: {}",
            response.message.unwrap_or_else(|| "no message".to_string())
        ));
    }

    let raw = response
        .output
        .ok_or_else(|| format!("action '{action_name}' returned no output"))?;

    serde_json::from_str(&raw).map_err(|e| format!("failed to parse '{action_name}' output: {e}"))
}

// ── Rich text detection ───────────────────────────────────────────────────────

fn find_rich_text(contents: &Value, hint: Option<&str>) -> (Option<Value>, Option<String>) {
    if !contents.is_object() {
        return (None, None);
    }

    if let Some(field) = hint {
        if let Some(candidate) = contents.get(field) {
            if is_tiptap_doc(candidate) {
                return (Some(candidate.clone()), Some(field.to_string()));
            }
        }
    }

    for field in RICH_TEXT_FIELD_CANDIDATES {
        if let Some(candidate) = contents.get(field) {
            if is_tiptap_doc(candidate) {
                return (Some(candidate.clone()), Some(field.to_string()));
            }
        }
    }

    (None, None)
}

fn is_tiptap_doc(value: &Value) -> bool {
    value
        .get("type")
        .and_then(Value::as_str)
        .map(|t| t == "doc")
        .unwrap_or(false)
}

// ── Post-level icon (BlogPostWithComments.icon) ──────────────────────────────

/// Resolves a `contents.icon` value to a files-store relPath, mirroring
/// the `iconRelPath()` helper in `BLOG_POST_WITH_COMMENTS_TEMPLATE`
/// (solx-types/src/seed.rs): accepts either a bare relPath string, or an
/// object carrying `relPath` directly or a `name` to join under
/// `files/docs/shared/`.
fn resolve_icon_relpath(icon: &Value) -> Option<String> {
    match icon {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(_) => {
            if let Some(rp) = icon.get("relPath").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                return Some(rp.to_string());
            }
            icon.get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|name| format!("files/docs/shared/{name}"))
        }
        _ => None,
    }
}

fn guess_mime_from_extension(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// Reads the icon's bytes from the file store via `artifact-read`,
/// uploads them to Google Drive (`upload-google-drive-image`), makes the
/// resulting file link-shareable (`share-google-drive-file-public`), and
/// returns a URL Google's own servers can fetch anonymously — needed
/// because `insertInlineImage` cannot reach solx's auth-gated `/files/*`
/// route directly. Best-effort: callers should log-and-skip on `Err`
/// rather than fail the whole document conversion over a broken icon.
fn upload_icon_and_get_public_url(relpath: &str) -> Result<String, String> {
    let bytes = sol::actions::artifact_read::read(relpath)
        .map_err(|e| format!("artifact-read '{relpath}' failed: {e}"))?;
    let media_b64 = STANDARD.encode(&bytes);
    let mime = guess_mime_from_extension(relpath);
    let filename = relpath.rsplit('/').next().unwrap_or("icon");

    let uploaded = exec_action_json(
        "/packages/solx-google/upload-google-drive-image",
        &json!({
            "metadata": { "name": filename },
            "media_base64": media_b64,
            "media_content_type": mime,
        }),
    )?;
    let file_id = uploaded
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "upload-google-drive-image response missing 'id'".to_string())?;

    exec_action_json(
        "/packages/solx-google/share-google-drive-file-public",
        &json!({ "fileId": file_id, "type": "anyone", "role": "reader" }),
    )?;

    // NOT `drive.google.com/uc?export=view&id=...` -- that legacy link
    // increasingly serves an HTML interstitial instead of raw bytes on a
    // server-to-server fetch, which is what Google's own Docs backend does
    // when resolving insertInlineImage, causing "There was a problem
    // retrieving the image". The googleusercontent.com CDN form (what
    // Docs/Slides themselves generate when you insert a Drive image via
    // the UI) reliably returns the raw image.
    Ok(format!("https://lh3.googleusercontent.com/d/{file_id}"))
}

// ── Text extraction from Sol contents ────────────────────────────────────────

fn extract_text_from_sol_contents(contents: &Value, text_hint: Option<&str>) -> Option<String> {
    if !contents.is_object() {
        let mut chunks = Vec::new();
        collect_text_nodes(contents, &mut chunks);
        let merged: String = chunks.into_iter().collect();
        return normalize_text(&merged);
    }

    if let Some(field) = text_hint {
        if let Some(v) = contents.get(field).and_then(Value::as_str) {
            let trimmed = v.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    if let Some(v) = contents.get("text").and_then(Value::as_str) {
        let trimmed = v.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }

    let (rich_text_doc, _) = find_rich_text(contents, None);
    if let Some(rt_doc) = &rich_text_doc {
        return Some(TiptapToGoogleDocsConverter::new().flatten_to_text(rt_doc));
    }

    let mut chunks = Vec::new();
    collect_text_nodes(contents, &mut chunks);
    let merged: String = chunks.into_iter().collect();
    normalize_text(&merged)
}

fn collect_text_nodes(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                out.push(text.to_string());
            }
            for v in map.values() {
                collect_text_nodes(v, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_text_nodes(v, out);
            }
        }
        _ => {}
    }
}

fn normalize_text(merged: &str) -> Option<String> {
    let normalized: String = merged
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = normalized.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

// ── Text extraction from Google Docs ─────────────────────────────────────────

fn extract_text_from_google_doc(google_doc: &Value) -> String {
    let mut out = String::new();
    let content = google_doc
        .get("body")
        .and_then(|b| b.get("content"))
        .and_then(|c| c.as_array());

    if let Some(blocks) = content {
        for block in blocks {
            if let Some(paragraph) = block.get("paragraph") {
                if let Some(elements) = paragraph.get("elements").and_then(|e| e.as_array()) {
                    for el in elements {
                        if let Some(run_text) = el
                            .get("textRun")
                            .and_then(|tr| tr.get("content"))
                            .and_then(Value::as_str)
                        {
                            out.push_str(run_text);
                        }
                    }
                }
            }
        }
    }

    out.trim().to_string()
}

fn normalize_document_name(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if matches!(ch, ' ' | '-' | '_' | '.') {
            out.push('-');
        }
    }
    let compact: Vec<&str> = out.split('-').filter(|s| !s.is_empty()).collect();
    if compact.is_empty() {
        "google-doc-import".to_string()
    } else {
        compact.join("-")
    }
}

// ── Tiptap → Google Docs request builder ─────────────────────────────────────

struct TiptapToGoogleDocsConverter {
    current_index: usize,
    requests: Vec<Value>,
    style_requests: Vec<Value>,
}

impl TiptapToGoogleDocsConverter {
    fn new() -> Self {
        Self {
            current_index: 1,
            requests: Vec::new(),
            style_requests: Vec::new(),
        }
    }

    fn flatten_to_text(&self, tiptap_json: &Value) -> String {
        if !is_tiptap_doc(tiptap_json) {
            return String::new();
        }

        let mut out = String::new();
        if let Some(children) = tiptap_json.get("content").and_then(|c| c.as_array()) {
            for child in children {
                Self::collect_text_static(child, &mut out);
            }
        }
        normalize_text(&out).unwrap_or_default()
    }

    fn collect_text_static(node: &Value, out: &mut String) {
        if !node.is_object() {
            return;
        }
        let node_type = node.get("type").and_then(Value::as_str).unwrap_or("");

        match node_type {
            "text" => {
                if let Some(text) = node.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
            "hardBreak" => out.push('\n'),
            "image" => {
                if let Some(alt) = node
                    .get("attrs")
                    .and_then(|a| a.get("alt"))
                    .and_then(Value::as_str)
                {
                    out.push_str(alt);
                }
            }
            "video" => {
                if let Some(src) = node
                    .get("attrs")
                    .and_then(|a| a.get("src"))
                    .and_then(Value::as_str)
                {
                    out.push_str(src);
                }
            }
            _ => {}
        }

        if let Some(children) = node.get("content").and_then(|c| c.as_array()) {
            for child in children {
                Self::collect_text_static(child, out);
            }
        }

        if matches!(node_type, "paragraph" | "heading" | "listItem") {
            out.push('\n');
        }
    }

    /// Walks a tiptap document's top-level `content[]` and emits requests
    /// for it at the current cursor position. Split out from the old
    /// consuming `build_requests` so a post-level icon (and, after this,
    /// a rendered comment thread) can share one converter/cursor with the
    /// document body instead of each needing their own `index: 1` start.
    fn process_tiptap_document(&mut self, tiptap_json: &Value) {
        if !is_tiptap_doc(tiptap_json) {
            return;
        }
        if let Some(children) = tiptap_json.get("content").and_then(|c| c.as_array()) {
            for child in children {
                self.process_node(child, None);
            }
        }
    }

    /// Inserts a single block of plain text at the cursor, followed by a
    /// paragraph break. Used when a Sol document has no tiptap rich-text
    /// field to walk (the plain-text fallback), in place of the old
    /// hardcoded `index: 1` insert — this one is cursor-aware so it
    /// composes correctly after a possible icon insertion.
    fn insert_plain_text_block(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.process_text(&json!({ "type": "text", "text": text }));
        let break_start = self.current_index;
        self.emit_insert_text("\n", break_start);
        self.current_index += 1;
    }

    /// Renders a "Comments (N)" heading followed by the comment tree,
    /// mirroring the EJS `renderComment` recursion in
    /// `BLOG_POST_WITH_COMMENTS_TEMPLATE` (solx-types/src/seed.rs).
    fn render_comments_section(&mut self, comments: &[Value]) {
        let block_start = self.current_index;
        self.process_text(&json!({ "type": "text", "text": format!("Comments ({})", comments.len()) }));
        let break_start = self.current_index;
        self.emit_insert_text("\n", break_start);
        self.current_index += 1;
        self.style_requests.push(json!({
            "updateParagraphStyle": {
                "range": { "startIndex": block_start, "endIndex": self.current_index },
                "paragraphStyle": { "namedStyleType": "HEADING_2" },
                "fields": "namedStyleType"
            }
        }));

        for comment in comments {
            self.render_comment(comment, 0);
        }
    }

    /// Renders one comment (icon, bold author, optional date, text) and
    /// recurses into `replies`, indenting each nesting level via
    /// `indentStart` — the Docs-native analogue of the preview's
    /// cumulative `padding-left` per `.blog-preview__comment` level.
    fn render_comment(&mut self, comment: &Value, depth: usize) {
        let block_start = self.current_index;

        if let Some(icon_url) = comment.get("icon").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            // Comment icons are already-public hotlinked URLs per the
            // BlogComment schema — no Drive upload needed, unlike the
            // post-level icon.
            self.insert_inline_image_from_url(icon_url);
        }

        let author = comment.get("author").and_then(Value::as_str).unwrap_or("Anonymous");
        self.process_text(&json!({ "type": "text", "text": author, "marks": [{ "type": "bold" }] }));
        if let Some(date) = comment.get("date").and_then(Value::as_str) {
            self.process_text(&json!({ "type": "text", "text": format!("  {date}") }));
        }
        self.process_hard_break();

        let text = comment.get("text").and_then(Value::as_str).unwrap_or("");
        self.process_text(&json!({ "type": "text", "text": text }));
        let break_start = self.current_index;
        self.emit_insert_text("\n", break_start);
        self.current_index += 1;

        if depth > 0 {
            self.style_requests.push(json!({
                "updateParagraphStyle": {
                    "range": { "startIndex": block_start, "endIndex": self.current_index },
                    "paragraphStyle": { "indentStart": { "magnitude": (18 * depth) as f64, "unit": "PT" } },
                    "fields": "indentStart"
                }
            }));
        }

        if let Some(replies) = comment.get("replies").and_then(Value::as_array) {
            for reply in replies {
                self.render_comment(reply, depth + 1);
            }
        }
    }

    /// Consumes the converter, concatenating `insertText`/
    /// `insertInlineImage` requests (in document order) with the style
    /// requests (`updateParagraphStyle`, `updateTextStyle`,
    /// `createParagraphBullets`) computed against the final post-insertion
    /// indices. `requests` must come first: Docs' `batchUpdate` applies
    /// requests sequentially and each insert shifts every later index, so
    /// inserting all text first (in increasing index order) lets the style
    /// ranges use final offsets without re-deriving them post-shift.
    fn finish(self) -> Vec<Value> {
        if self.requests.is_empty() && self.style_requests.is_empty() {
            return vec![json!({
                "insertText": {
                    "location": { "index": 1 },
                    "text": ""
                }
            })];
        }
        let mut all = self.requests;
        all.extend(self.style_requests);
        all
    }

    fn process_node(&mut self, node: &Value, list_kind: Option<&str>) {
        if !node.is_object() {
            return;
        }
        let node_type = node.get("type").and_then(Value::as_str).unwrap_or("");

        match node_type {
            "paragraph" | "heading" => self.process_block(node, list_kind),
            "bulletList" | "orderedList" => self.process_list(node, node_type),
            "text" => self.process_text(node),
            "hardBreak" => self.process_hard_break(),
            "image" => self.process_image(node),
            "video" => self.process_video_as_caption(node),
            _ => {} // skip unknown nodes
        }
    }

    fn process_block(&mut self, node: &Value, list_kind: Option<&str>) {
        let node_type = node.get("type").and_then(Value::as_str).unwrap_or("");
        let level = if node_type == "heading" {
            node.get("attrs")
                .and_then(|a| a.get("level"))
                .and_then(Value::as_u64)
        } else {
            None
        };

        let block_start = self.current_index;

        if let Some(children) = node.get("content").and_then(|c| c.as_array()) {
            for child in children {
                self.process_node(child, list_kind);
            }
        }

        // Paragraph break
        let break_start = self.current_index;
        self.emit_insert_text("\n", break_start);
        self.current_index += 1;
        let block_end = self.current_index;

        if let Some(lvl) = level {
            self.style_requests.push(json!({
                "updateParagraphStyle": {
                    "range": { "startIndex": block_start, "endIndex": block_end },
                    "paragraphStyle": { "namedStyleType": format!("HEADING_{}", lvl) },
                    "fields": "namedStyleType"
                }
            }));
        }

        if let Some(kind) = list_kind {
            let bullet_preset = if kind == "bulletList" {
                "BULLET_DISC_CIRCLE_SQUARE"
            } else {
                "NUMBERED_DECIMAL_ALPHA_ROMAN"
            };
            self.style_requests.push(json!({
                "createParagraphBullets": {
                    "range": { "startIndex": block_start, "endIndex": block_end },
                    "bulletPreset": bullet_preset
                }
            }));
        }
    }

    fn process_list(&mut self, list_node: &Value, kind: &str) {
        if let Some(children) = list_node.get("content").and_then(|c| c.as_array()) {
            for child in children {
                if child.get("type").and_then(Value::as_str) == Some("listItem") {
                    if let Some(grandchildren) = child.get("content").and_then(|c| c.as_array()) {
                        for gc in grandchildren {
                            self.process_node(gc, Some(kind));
                        }
                    }
                }
            }
        }
    }

    fn process_text(&mut self, node: &Value) {
        let text = node.get("text").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            return;
        }

        let start = self.current_index;
        // Google Docs API indices are UTF-16 code units, not UTF-8 bytes —
        // `str::len()` would silently corrupt every style/bullet range
        // downstream of the first non-ASCII character (emoji included).
        let end = start + text.encode_utf16().count();
        self.emit_insert_text(text, start);
        self.current_index = end;

        let marks = node.get("marks").and_then(|m| m.as_array());
        if marks.is_none() {
            return;
        }

        let mut text_style = serde_json::Map::new();
        let mut fields: Vec<String> = Vec::new();

        for mark in marks.unwrap() {
            if !mark.is_object() {
                continue;
            }
            let mark_type = mark.get("type").and_then(Value::as_str).unwrap_or("");
            let mark_attrs = mark.get("attrs");

            match mark_type {
                "link" => {
                    let url = mark_attrs
                        .and_then(|a| a.get("href"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if url.is_empty() || is_internal_link(url) {
                        continue;
                    }
                    text_style.insert("link".into(), json!({ "url": url }));
                    text_style.insert("underline".into(), Value::Bool(true));
                    text_style.insert(
                        "foregroundColor".into(),
                        json!({ "color": { "rgbColor": { "blue": 1.0 } } }),
                    );
                    fields.extend_from_slice(&[
                        "link".into(),
                        "underline".into(),
                        "foregroundColor".into(),
                    ]);
                }
                "bold" => {
                    text_style.insert("bold".into(), Value::Bool(true));
                    fields.push("bold".into());
                }
                "italic" => {
                    text_style.insert("italic".into(), Value::Bool(true));
                    fields.push("italic".into());
                }
                "underline" => {
                    text_style.insert("underline".into(), Value::Bool(true));
                    fields.push("underline".into());
                }
                "strike" => {
                    text_style.insert("strikethrough".into(), Value::Bool(true));
                    fields.push("strikethrough".into());
                }
                "code" => {
                    text_style.insert(
                        "weightedFontFamily".into(),
                        json!({ "fontFamily": "Consolas" }),
                    );
                    text_style
                        .insert("fontSize".into(), json!({ "magnitude": 10, "unit": "PT" }));
                    fields.extend_from_slice(&["weightedFontFamily".into(), "fontSize".into()]);
                }
                _ => {}
            }
        }

        if fields.is_empty() {
            return;
        }

        // Deduplicate fields while preserving first-seen order.
        let mut seen: HashSet<String> = HashSet::new();
        let deduped: Vec<String> = fields
            .into_iter()
            .filter(|f| seen.insert(f.clone()))
            .collect();
        let fields_str = deduped.join(",");

        self.style_requests.push(json!({
            "updateTextStyle": {
                "range": { "startIndex": start, "endIndex": end },
                "textStyle": text_style,
                "fields": fields_str
            }
        }));
    }

    fn process_hard_break(&mut self) {
        let start = self.current_index;
        self.emit_insert_text("\n", start);
        self.current_index += 1;
    }

    /// Inserts an inline image at the cursor, followed by a paragraph
    /// break, advancing `current_index` past both. `url` must be
    /// fetchable by Google's own servers (no auth headers, no data:
    /// URIs) — used for tiptap `image` nodes, the post-level icon (after
    /// it's been uploaded to Drive and shared, see
    /// `upload_icon_and_get_public_url`), and comment author icons
    /// (already-public hotlinked URLs, no upload needed).
    fn insert_inline_image_from_url(&mut self, url: &str) {
        self.requests.push(json!({
            "insertInlineImage": {
                "location": { "index": self.current_index },
                "uri": url
            }
        }));
        self.current_index += 1;
        let break_start = self.current_index;
        self.emit_insert_text("\n", break_start);
        self.current_index += 1;
    }

    fn process_image(&mut self, node: &Value) {
        let attrs = node.get("attrs");
        let src = attrs.and_then(|a| a.get("src")).and_then(Value::as_str);

        match src {
            Some(url) if !url.is_empty() && !is_internal_link(url) => {
                self.insert_inline_image_from_url(url);
            }
            _ => {
                let alt = attrs
                    .and_then(|a| a.get("alt"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !alt.is_empty() {
                    self.process_text(&json!({
                        "type": "text",
                        "text": format!("[image: {}]", alt)
                    }));
                }
            }
        }
    }

    fn process_video_as_caption(&mut self, node: &Value) {
        let attrs = node.get("attrs");
        let src = attrs
            .and_then(|a| a.get("src"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let title = attrs
            .and_then(|a| a.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("video");
        let caption = if src.is_empty() {
            format!("[{}]", title)
        } else {
            format!("[{}]({})", title, src)
        };
        self.process_text(&json!({ "type": "text", "text": caption }));
    }

    fn emit_insert_text(&mut self, text: &str, index: usize) {
        self.requests.push(json!({
            "insertText": {
                "location": { "index": index },
                "text": text
            }
        }));
    }
}

fn is_internal_link(url: &str) -> bool {
    INTERNAL_LINK_PREFIXES
        .iter()
        .any(|prefix| url.starts_with(prefix))
}

// ── Base64url helpers (Gmail) ─────────────────────────────────────────────────

fn b64url_decode_to_string(data: &str) -> String {
    let trimmed = data.trim_end_matches('=');
    let bytes = URL_SAFE_NO_PAD
        .decode(trimmed)
        .or_else(|_| URL_SAFE.decode(data))
        .unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn encode_mime_header(s: &str) -> String {
    if s.is_ascii() {
        s.to_string()
    } else {
        format!("=?UTF-8?B?{}?=", STANDARD.encode(s.as_bytes()))
    }
}

fn strip_html_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    normalize_text(&out).unwrap_or_default()
}

// ── Gmail ──────────────────────────────────────────────────────────────────────

fn gmail_header<'a>(headers: &'a Value, name: &str) -> Option<&'a str> {
    headers
        .as_array()?
        .iter()
        .find(|h| {
            h.get("name")
                .and_then(Value::as_str)
                .map(|n| n.eq_ignore_ascii_case(name))
                .unwrap_or(false)
        })
        .and_then(|h| h.get("value"))
        .and_then(Value::as_str)
}

fn find_gmail_text_part<'a>(payload: &'a Value, preferred_mime: &str) -> Option<&'a Value> {
    let mime = payload.get("mimeType").and_then(Value::as_str).unwrap_or("");
    if mime == preferred_mime
        && payload
            .get("body")
            .and_then(|b| b.get("data"))
            .and_then(Value::as_str)
            .is_some()
    {
        return Some(payload);
    }
    if let Some(parts) = payload.get("parts").and_then(|p| p.as_array()) {
        for part in parts {
            if let Some(found) = find_gmail_text_part(part, preferred_mime) {
                return Some(found);
            }
        }
    }
    None
}

fn extract_gmail_body_text(payload: &Value) -> String {
    if let Some(part) = find_gmail_text_part(payload, "text/plain") {
        if let Some(data) = part.get("body").and_then(|b| b.get("data")).and_then(Value::as_str) {
            return b64url_decode_to_string(data);
        }
    }
    if let Some(part) = find_gmail_text_part(payload, "text/html") {
        if let Some(data) = part.get("body").and_then(|b| b.get("data")).and_then(Value::as_str) {
            return strip_html_tags(&b64url_decode_to_string(data));
        }
    }
    String::new()
}

// ── convert-sol-doc-to-gmail-message ────────────────────────────────────────────

fn convert_sol_to_gmail(params: &str) -> Result<ActionResult, String> {
    let input: Value = serde_json::from_str(params)
        .map_err(|e| format!("invalid JSON params: {e}"))?;

    let to = input
        .get("to")
        .and_then(Value::as_str)
        .ok_or_else(|| "to is required (recipient email address)".to_string())?;
    let cc = input.get("cc").and_then(Value::as_str);
    let bcc = input.get("bcc").and_then(Value::as_str);

    let source_doc = resolve_sol_document(&input)?;
    let contents = source_doc.get("contents").cloned().unwrap_or(Value::Null);
    let text_hint = input.get("text_field_hint").and_then(Value::as_str);
    let rich_text_hint = input.get("rich_text_field_hint").and_then(Value::as_str);

    let subject = input
        .get("subject")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            source_doc
                .get("title")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .unwrap_or_else(|| DEFAULT_TITLE.to_string());

    let (rich_text_doc, _) = find_rich_text(&contents, rich_text_hint);
    let body = if let Some(rt_doc) = &rich_text_doc {
        TiptapToGoogleDocsConverter::new().flatten_to_text(rt_doc)
    } else {
        extract_text_from_sol_contents(&contents, text_hint).unwrap_or_default()
    };

    let mut message = String::new();
    message.push_str(&format!("To: {}\r\n", to));
    if let Some(cc) = cc {
        message.push_str(&format!("Cc: {}\r\n", cc));
    }
    if let Some(bcc) = bcc {
        message.push_str(&format!("Bcc: {}\r\n", bcc));
    }
    message.push_str(&format!("Subject: {}\r\n", encode_mime_header(&subject)));
    message.push_str("MIME-Version: 1.0\r\n");
    message.push_str("Content-Type: text/plain; charset=\"UTF-8\"\r\n\r\n");
    message.push_str(&body);

    let raw = URL_SAFE_NO_PAD.encode(message.as_bytes());

    let output = json!({
        "to": to,
        "cc": cc,
        "bcc": bcc,
        "subject": subject,
        "text": body,
        "raw": raw,
        "send_body": { "raw": raw },
        "metadata": {
            "source_name": source_doc.get("name"),
            "has_rich_text": rich_text_doc.is_some(),
        }
    });

    ok(output)
}

// ── convert-gmail-message-to-sol-doc ────────────────────────────────────────────

fn convert_gmail_to_sol(params: &str) -> Result<ActionResult, String> {
    let input: Value = serde_json::from_str(params)
        .map_err(|e| format!("invalid JSON params: {e}"))?;

    let gmail_message = input
        .get("gmail_message")
        .ok_or_else(|| "gmail_message is required and must be a JSON object".to_string())?;

    let payload = gmail_message.get("payload").cloned().unwrap_or(Value::Null);
    let headers = payload.get("headers").cloned().unwrap_or(Value::Array(vec![]));
    let subject = gmail_header(&headers, "Subject").unwrap_or("").to_string();
    let from = gmail_header(&headers, "From").map(String::from);
    let to = gmail_header(&headers, "To").map(String::from);
    let date = gmail_header(&headers, "Date").map(String::from);
    let message_id = gmail_message.get("id").and_then(Value::as_str);
    let thread_id = gmail_message.get("threadId").and_then(Value::as_str);

    let text = extract_gmail_body_text(&payload);

    let suggested_name = input
        .get("output_document_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| {
            normalize_document_name(if subject.is_empty() { "gmail-message" } else { &subject })
        });

    let output_type = input
        .get("output_type_name")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_TYPE_NAME);

    let sol_payload = json!({
        "name": suggested_name,
        "type_name": output_type,
        "title": subject,
        "contents": {
            "text": text,
            "source": {
                "provider": "gmail",
                "message_id": message_id,
                "thread_id": thread_id,
                "from": from,
                "to": to,
                "date": date,
            }
        }
    });

    let output = json!({
        "message_id": message_id,
        "thread_id": thread_id,
        "subject": subject,
        "from": from,
        "to": to,
        "date": date,
        "text": text,
        "suggested_document_name": suggested_name,
        "sol_document_payload": sol_payload,
        "metadata": { "output_type_name": output_type },
    });

    ok(output)
}

// ── Google Tasks ──────────────────────────────────────────────────────────────

fn convert_sol_to_google_task(params: &str) -> Result<ActionResult, String> {
    let input: Value = serde_json::from_str(params)
        .map_err(|e| format!("invalid JSON params: {e}"))?;

    let source_doc = resolve_sol_document(&input)?;

    let title = source_doc
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| input.get("default_title").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| "Imported Sol Task".to_string());

    let contents = source_doc.get("contents").cloned().unwrap_or(Value::Null);
    let text_hint = input.get("text_field_hint").and_then(Value::as_str);
    let notes = extract_text_from_sol_contents(&contents, text_hint).unwrap_or_default();

    let due = input.get("due").and_then(Value::as_str).map(String::from);
    let status = input
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("needsAction")
        .to_string();

    let mut create_body = json!({ "title": title, "notes": notes, "status": status });
    if let Some(due) = &due {
        create_body["due"] = json!(due);
    }

    let output = json!({
        "title": title,
        "notes": notes,
        "due": due,
        "status": status,
        "create_body": create_body,
        "metadata": { "source_name": source_doc.get("name") }
    });

    ok(output)
}

fn convert_google_task_to_sol(params: &str) -> Result<ActionResult, String> {
    let input: Value = serde_json::from_str(params)
        .map_err(|e| format!("invalid JSON params: {e}"))?;

    let task = input
        .get("google_task")
        .ok_or_else(|| "google_task is required and must be a JSON object".to_string())?;

    let title = task
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Untitled task")
        .to_string();
    let notes = task
        .get("notes")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let due = task.get("due").and_then(Value::as_str);
    let status = task.get("status").and_then(Value::as_str);
    let task_id = task.get("id").and_then(Value::as_str);

    let suggested_name = input
        .get("output_document_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| normalize_document_name(&title));

    let output_type = input
        .get("output_type_name")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_TYPE_NAME);

    let sol_payload = json!({
        "name": suggested_name,
        "type_name": output_type,
        "title": title,
        "contents": {
            "text": notes,
            "source": {
                "provider": "google-tasks",
                "task_id": task_id,
                "due": due,
                "status": status,
            }
        }
    });

    let output = json!({
        "task_id": task_id,
        "title": title,
        "notes": notes,
        "due": due,
        "status": status,
        "suggested_document_name": suggested_name,
        "sol_document_payload": sol_payload,
        "metadata": { "output_type_name": output_type },
    });

    ok(output)
}

// ── Google Calendar ────────────────────────────────────────────────────────────

fn convert_sol_to_calendar_event(params: &str) -> Result<ActionResult, String> {
    let input: Value = serde_json::from_str(params)
        .map_err(|e| format!("invalid JSON params: {e}"))?;

    let source_doc = resolve_sol_document(&input)?;

    let summary = input
        .get("summary")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            source_doc
                .get("title")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .unwrap_or_else(|| DEFAULT_TITLE.to_string());

    let contents = source_doc.get("contents").cloned().unwrap_or(Value::Null);
    let text_hint = input.get("text_field_hint").and_then(Value::as_str);
    let description = extract_text_from_sol_contents(&contents, text_hint).unwrap_or_default();

    let start = input
        .get("start")
        .and_then(Value::as_str)
        .ok_or_else(|| "start is required (RFC3339 dateTime or YYYY-MM-DD date)".to_string())?;
    let end = input
        .get("end")
        .and_then(Value::as_str)
        .ok_or_else(|| "end is required (RFC3339 dateTime or YYYY-MM-DD date)".to_string())?;
    let all_day = input.get("all_day").and_then(Value::as_bool).unwrap_or(false);
    let time_zone = input.get("time_zone").and_then(Value::as_str);
    let location = input.get("location").and_then(Value::as_str);

    let time_key = if all_day { "date" } else { "dateTime" };
    let mut start_obj = json!({ time_key: start });
    let mut end_obj = json!({ time_key: end });
    if let (Some(tz), false) = (time_zone, all_day) {
        start_obj["timeZone"] = json!(tz);
        end_obj["timeZone"] = json!(tz);
    }

    let attendees: Vec<Value> = input
        .get("attendees")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| a.as_str().map(|email| json!({ "email": email })))
        .collect();

    let mut create_body = json!({
        "summary": summary,
        "description": description,
        "start": start_obj,
        "end": end_obj,
    });
    if let Some(loc) = location {
        create_body["location"] = json!(loc);
    }
    if !attendees.is_empty() {
        create_body["attendees"] = json!(attendees);
    }

    let output = json!({
        "summary": summary,
        "description": description,
        "start": start_obj,
        "end": end_obj,
        "create_body": create_body,
        "metadata": {
            "source_name": source_doc.get("name"),
            "attendee_count": attendees.len(),
        }
    });

    ok(output)
}

fn convert_calendar_event_to_sol(params: &str) -> Result<ActionResult, String> {
    let input: Value = serde_json::from_str(params)
        .map_err(|e| format!("invalid JSON params: {e}"))?;

    let event = input
        .get("calendar_event")
        .ok_or_else(|| "calendar_event is required and must be a JSON object".to_string())?;

    let summary = event
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("Untitled event")
        .to_string();
    let description = event
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let start = event.get("start").cloned().unwrap_or(Value::Null);
    let end = event.get("end").cloned().unwrap_or(Value::Null);
    let location = event.get("location").and_then(Value::as_str);
    let event_id = event.get("id").and_then(Value::as_str);
    let html_link = event.get("htmlLink").and_then(Value::as_str);

    let suggested_name = input
        .get("output_document_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| normalize_document_name(&summary));

    let output_type = input
        .get("output_type_name")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_TYPE_NAME);

    let sol_payload = json!({
        "name": suggested_name,
        "type_name": output_type,
        "title": summary,
        "contents": {
            "text": description,
            "source": {
                "provider": "google-calendar",
                "event_id": event_id,
                "start": start,
                "end": end,
                "location": location,
                "html_link": html_link,
            }
        }
    });

    let output = json!({
        "event_id": event_id,
        "summary": summary,
        "description": description,
        "start": start,
        "end": end,
        "location": location,
        "suggested_document_name": suggested_name,
        "sol_document_payload": sol_payload,
        "metadata": { "output_type_name": output_type },
    });

    ok(output)
}
