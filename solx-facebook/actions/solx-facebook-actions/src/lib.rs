//! Facebook Page post → Sol document converters.
//!
//! A single WASM component hosting three actions, dispatched by `fn_name`
//! (the `action_name` argument to the WIT `runner.run` export):
//!
//! | fn_name | What it does |
//! |---|---|
//! | `convert-facebook-post-to-sol-doc` | Pure transform: Graph API post JSON → `entity-save-document` payload |
//! | `import-facebook-post` | Fetch one post (+ comments, + picture), convert, save |
//! | `import-facebook-posts` | Page through `list-facebook-posts`, importing each; resumable via `next_after` |
//!
//! Posts map onto the built-in `BlogPostWithComments` type: the message
//! becomes the Tiptap `content` / `text` / `paragraphs`, the comment thread
//! (top-level comments + their replies) becomes `comments`, and the post's
//! `full_picture` is downloaded into the files store as the post `icon`.
//!
//! All Graph API calls go through the package's own webhook actions
//! (`get-facebook-post`, `list-facebook-posts`, `list-facebook-comments`),
//! which carry the Page access token — this component holds no secrets.
//!
//! Build target: `cargo build --release --target wasm32-wasip2`

use serde_json::{json, Map, Value};

wit_bindgen::generate!({
    world: "custom-action",
    path: "wit",
});

use exports::sol::actions::runner::{ActionResult, Guest};

const TYPE_REF: &str = "/types/docs/BlogPostWithComments";
const GET_POST_ACTION: &str = "/packages/solx-facebook/get-facebook-post";
const LIST_POSTS_ACTION: &str = "/packages/solx-facebook/list-facebook-posts";
const LIST_COMMENTS_ACTION: &str = "/packages/solx-facebook/list-facebook-comments";

/// Fields requested for every post. `story` covers posts with no `message`
/// (e.g. "X updated their cover photo"); `unshimmed_url` is the attachment
/// link without Facebook's l.facebook.com redirect wrapper.
const POST_FIELDS: &str = "id,message,story,created_time,updated_time,permalink_url,full_picture,status_type,from{id,name},attachments{type,title,description,url,unshimmed_url}";
const COMMENT_FIELDS: &str = "id,message,created_time,from{id,name},comment_count";

/// Graph API's maximum page size for both posts and comments.
const GRAPH_PAGE_LIMIT: u64 = 100;
const DEFAULT_MAX_COMMENTS: u64 = 500;
const DEFAULT_IMPORT_COUNT: u64 = 25;
/// Facebook threads are two levels deep: comments, and replies to those.
const MAX_REPLY_DEPTH: u32 = 1;
const TITLE_MAX_CHARS: usize = 80;
const SUMMARY_MAX_CHARS: usize = 300;

struct SolxFacebookActions;

impl Guest for SolxFacebookActions {
    fn run(action_name: Option<String>, params: String) -> Result<ActionResult, String> {
        let fn_name = action_name.as_deref().unwrap_or("");
        let input: Value =
            serde_json::from_str(&params).map_err(|e| format!("invalid JSON params: {e}"))?;
        match fn_name {
            "convert-facebook-post-to-sol-doc" => convert_post_to_sol(&input),
            "import-facebook-post" => import_post(&input),
            "import-facebook-posts" => import_posts(&input),
            other => Err(format!("unknown action name '{other}'")),
        }
    }
}

export!(SolxFacebookActions);

fn ok(output: Value) -> Result<ActionResult, String> {
    Ok(ActionResult {
        success: true,
        message: None,
        output: Some(output.to_string()),
    })
}

fn log(message: &str) {
    sol::actions::logger::log(message);
}

// ── Options shared by the three actions ──────────────────────────────────────

struct ImportOptions {
    /// Explicit document path; `None` derives `/blogs/facebook/<page>`.
    path: Option<String>,
    include_comments: bool,
    max_comments: u64,
    download_picture: bool,
}

impl ImportOptions {
    fn from_input(input: &Value) -> Self {
        Self {
            path: input
                .get("path")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from),
            include_comments: input.get("include_comments").and_then(Value::as_bool).unwrap_or(true),
            max_comments: input
                .get("max_comments")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_MAX_COMMENTS),
            download_picture: input.get("download_picture").and_then(Value::as_bool).unwrap_or(true),
        }
    }
}

// ── convert-facebook-post-to-sol-doc ─────────────────────────────────────────

/// Converts an already-fetched post. Comments are taken from the post's own
/// `comments.data` (i.e. requested via field expansion, e.g.
/// `comments{message,from,created_time,comments{...}}`) — this action makes
/// no network calls, so the picture is referenced in `source` but not stored.
fn convert_post_to_sol(input: &Value) -> Result<ActionResult, String> {
    let post = input
        .get("facebook_post")
        .filter(|v| v.is_object())
        .ok_or_else(|| "facebook_post is required and must be a JSON object".to_string())?;
    let opts = ImportOptions::from_input(input);

    let comments = post
        .get("comments")
        .and_then(|c| c.get("data"))
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(embedded_comment_to_blog_comment).collect())
        .unwrap_or_default();

    let mut payload = build_document_payload(post, comments, None, opts.path.as_deref());
    if let Some(name) = input.get("output_document_name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        payload["name"] = json!(name);
    }
    if let Some(type_ref) = input.get("output_type_ref").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        payload["typeRef"] = json!(type_ref);
    }

    ok(json!({
        "post_id": post.get("id"),
        "title": payload["title"],
        "text": payload["contents"]["text"],
        "suggested_document_path": payload["path"],
        "suggested_document_name": payload["name"],
        "sol_document_payload": payload,
        "metadata": {
            "comment_count": count_comments(&payload["contents"]["comments"]),
            "type_ref": payload["typeRef"],
        }
    }))
}

// ── import-facebook-post ─────────────────────────────────────────────────────

fn import_post(input: &Value) -> Result<ActionResult, String> {
    let post_id = input
        .get("post_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "post_id is required".to_string())?;
    let opts = ImportOptions::from_input(input);

    log(&format!("import-facebook-post: fetching post '{post_id}'"));
    let post = exec_action_json(GET_POST_ACTION, &json!({ "post_id": post_id, "fields": POST_FIELDS }))?;
    ok(import_fetched_post(&post, &opts)?)
}

/// Shared by both importers: fetch comments, store the picture, build and
/// save the document. Returns a short summary of what was saved.
fn import_fetched_post(post: &Value, opts: &ImportOptions) -> Result<Value, String> {
    let post_id = post
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "post has no 'id'".to_string())?;

    let mut budget = CommentBudget { remaining: opts.max_comments, truncated: false };
    let comments = if opts.include_comments && opts.max_comments > 0 {
        fetch_comments(post_id, 0, &mut budget)?
    } else {
        Vec::new()
    };
    if budget.truncated {
        log(&format!(
            "import: post '{post_id}': stopped at max_comments={}; remaining comments not imported",
            opts.max_comments
        ));
    }

    let icon = if opts.download_picture {
        post.get("full_picture")
            .and_then(Value::as_str)
            .and_then(|url| store_picture(post_id, url))
    } else {
        None
    };

    let payload = build_document_payload(post, comments, icon.as_deref(), opts.path.as_deref());
    exec_action_json("/builtin/document/entity-save-document", &payload)?;

    Ok(json!({
        "post_id": post_id,
        "document_path": payload["path"],
        "document_name": payload["name"],
        "title": payload["title"],
        "comments": count_comments(&payload["contents"]["comments"]),
        "comments_truncated": budget.truncated,
        "icon": icon,
    }))
}

// ── import-facebook-posts ────────────────────────────────────────────────────

/// Imports up to `count` posts, newest first. Unlike the Google batch
/// uploader this does not stop at the first failure: one post with an
/// unreadable comment thread shouldn't sink the other 99, so failures are
/// collected in `failed` and the batch carries on. `next_after` resumes the
/// listing where this run stopped (null once the feed is exhausted).
fn import_posts(input: &Value) -> Result<ActionResult, String> {
    let count = input.get("count").and_then(Value::as_u64).unwrap_or(DEFAULT_IMPORT_COUNT);
    if count == 0 {
        return Err("count must be a positive integer".to_string());
    }
    let opts = ImportOptions::from_input(input);

    let mut after = input.get("after").and_then(Value::as_str).map(String::from);
    let mut imported: Vec<Value> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    let mut exhausted = false;
    let mut cancelled = false;

    while ((imported.len() + failed.len()) as u64) < count {
        let processed = (imported.len() + failed.len()) as u64;
        let mut list_params = json!({
            "fields": POST_FIELDS,
            "limit": (count - processed).min(GRAPH_PAGE_LIMIT),
        });
        for key in ["since", "until"] {
            if let Some(v) = input.get(key).filter(|v| !v.is_null()) {
                list_params[key] = v.clone();
            }
        }
        if let Some(cursor) = &after {
            list_params["after"] = json!(cursor);
        }

        let page = exec_action_json(LIST_POSTS_ACTION, &list_params)?;
        let posts = page.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
        log(&format!(
            "import-facebook-posts: fetched a page of {} post(s) ({processed}/{count} processed so far)",
            posts.len()
        ));

        for post in &posts {
            if is_cancelled() {
                cancelled = true;
                break;
            }
            let post_id = post.get("id").and_then(Value::as_str).unwrap_or("?");
            let n = imported.len() + failed.len() + 1;
            match import_fetched_post(post, &opts) {
                Ok(entry) => {
                    log(&format!("import-facebook-posts: [{n}/{count}] '{post_id}': saved"));
                    imported.push(entry);
                }
                Err(e) => {
                    log(&format!("import-facebook-posts: [{n}/{count}] '{post_id}': failed: {e}"));
                    failed.push(json!({ "post_id": post_id, "error": e }));
                }
            }
        }
        if cancelled {
            // Mid-page: the page's `after` cursor would skip the posts we
            // didn't reach, so hand back the cursor we started this page with.
            break;
        }

        after = page
            .get("paging")
            .and_then(|p| p.get("cursors"))
            .and_then(|c| c.get("after"))
            .and_then(Value::as_str)
            .map(String::from);
        let has_next = page.get("paging").and_then(|p| p.get("next")).is_some();
        if posts.is_empty() || !has_next || after.is_none() {
            exhausted = true;
            break;
        }
    }

    let output = json!({
        "requested": count,
        "imported": imported,
        "failed": failed,
        "cancelled": cancelled,
        "next_after": if exhausted { Value::Null } else { json!(after) },
    });
    if failed.is_empty() {
        ok(output)
    } else {
        Ok(ActionResult {
            success: false,
            message: Some(format!(
                "{} of {} post(s) failed to import; see 'failed'",
                failed.len(),
                imported.len() + failed.len()
            )),
            output: Some(output.to_string()),
        })
    }
}

/// Asks solx-core whether `action-stop` was requested for this invocation.
/// Fails closed to `false`: a missing builtin must never abort real work.
fn is_cancelled() -> bool {
    exec_action_json("/builtin/action/cancelled", &json!({}))
        .ok()
        .and_then(|r| r.get("cancelled").and_then(Value::as_bool))
        .unwrap_or(false)
}

// ── Comments ─────────────────────────────────────────────────────────────────

struct CommentBudget {
    remaining: u64,
    /// Set when the budget ran out while more comments were available.
    truncated: bool,
}

/// Pages through `/{object_id}/comments` (a post's top-level comments, or a
/// comment's replies), recursing into replies up to [`MAX_REPLY_DEPTH`].
/// Returns `BlogComment`-shaped values in chronological order.
fn fetch_comments(object_id: &str, depth: u32, budget: &mut CommentBudget) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        if budget.remaining == 0 {
            budget.truncated = true;
            break;
        }
        let mut params = json!({
            "object_id": object_id,
            "fields": COMMENT_FIELDS,
            "order": "chronological",
            "limit": budget.remaining.min(GRAPH_PAGE_LIMIT),
        });
        if let Some(cursor) = &after {
            params["after"] = json!(cursor);
        }
        let page = exec_action_json(LIST_COMMENTS_ACTION, &params)?;
        let comments = page.get("data").and_then(Value::as_array).cloned().unwrap_or_default();

        for comment in &comments {
            if budget.remaining == 0 {
                budget.truncated = true;
                return Ok(out);
            }
            budget.remaining -= 1;
            let mut blog_comment = graph_comment_to_blog_comment(comment);
            let reply_count = comment.get("comment_count").and_then(Value::as_u64).unwrap_or(0);
            if depth < MAX_REPLY_DEPTH && reply_count > 0 {
                if let Some(id) = comment.get("id").and_then(Value::as_str) {
                    blog_comment["replies"] = json!(fetch_comments(id, depth + 1, budget)?);
                }
            }
            out.push(blog_comment);
        }

        after = page
            .get("paging")
            .and_then(|p| p.get("cursors"))
            .and_then(|c| c.get("after"))
            .and_then(Value::as_str)
            .map(String::from);
        let has_next = page.get("paging").and_then(|p| p.get("next")).is_some();
        if comments.is_empty() || !has_next || after.is_none() {
            break;
        }
    }
    Ok(out)
}

/// Graph comment → `BlogComment`. `from` is omitted by Facebook for users
/// who haven't granted the app access (without `pages_read_user_content`),
/// which maps to the type's anonymous `author: null`.
fn graph_comment_to_blog_comment(comment: &Value) -> Value {
    json!({
        "author": comment.get("from").and_then(|f| f.get("name")).and_then(Value::as_str),
        "icon": Value::Null,
        "text": comment.get("message").and_then(Value::as_str).unwrap_or(""),
        "date": comment.get("created_time").and_then(Value::as_str).map(normalize_graph_time),
        "replies": [],
    })
}

/// Like [`graph_comment_to_blog_comment`], but also maps replies nested via
/// field expansion (`comments{...,comments{...}}`) for the pure converter.
fn embedded_comment_to_blog_comment(comment: &Value) -> Value {
    let mut out = graph_comment_to_blog_comment(comment);
    if let Some(replies) = comment.get("comments").and_then(|c| c.get("data")).and_then(Value::as_array) {
        out["replies"] = json!(replies.iter().map(embedded_comment_to_blog_comment).collect::<Vec<_>>());
    }
    out
}

fn count_comments(comments: &Value) -> usize {
    comments
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|c| 1 + c.get("replies").map(count_comments).unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

// ── Picture → files store ────────────────────────────────────────────────────

/// Downloads the post's `full_picture` into the files store and returns the
/// relPath `contents.icon` expects (the preview can't hotlink: solx-server's
/// files route is auth-gated, and the icon convention is a relPath).
///
/// Best-effort: a failed download never fails the import. The usual failure
/// is the outbound allowlist — pictures are served from regional
/// `https://scontent-*.fbcdn.net/` hosts, which need adding to
/// `allowed_base_urls` for the icon to be stored.
fn store_picture(post_id: &str, url: &str) -> Option<String> {
    let result = (|| -> Result<String, String> {
        let resp = exec_action_json("/builtin/web/http-request", &json!({ "url": url, "timeout_secs": 30 }))?;
        let status = resp.get("status").and_then(Value::as_u64).unwrap_or(0);
        if !(200..300).contains(&status) {
            return Err(format!("HTTP status {status}"));
        }
        let content_type = resp
            .get("content_type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let ext = match content_type.as_str() {
            "image/png" => "png",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "jpg",
        };
        let rel_path = format!("files/docs/shared/fb-{}-picture.{ext}", sanitize_name(post_id));
        exec_action_json(
            "/builtin/file/file-put",
            &json!({
                "rel_path": rel_path,
                "content": resp.get("body"),
                "encoding": resp.get("body_encoding"),
            }),
        )?;
        Ok(rel_path)
    })();
    match result {
        Ok(rel_path) => Some(rel_path),
        Err(e) => {
            log(&format!("import: post '{post_id}': picture not stored ({e}); continuing without an icon"));
            None
        }
    }
}

// ── Post → document payload ──────────────────────────────────────────────────

/// Builds an `/builtin/document/entity-save-document` payload for `post`.
fn build_document_payload(post: &Value, comments: Vec<Value>, icon: Option<&str>, path: Option<&str>) -> Value {
    let post_id = post.get("id").and_then(Value::as_str).unwrap_or("unknown");
    let message = post
        .get("message")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| post.get("story").and_then(Value::as_str))
        .unwrap_or("");
    let created_time = post.get("created_time").and_then(Value::as_str).map(normalize_graph_time);
    let permalink = post.get("permalink_url").and_then(Value::as_str);
    let page_name = post.get("from").and_then(|f| f.get("name")).and_then(Value::as_str);
    let page_id = post.get("from").and_then(|f| f.get("id")).and_then(Value::as_str);
    let attachment = post
        .get("attachments")
        .and_then(|a| a.get("data"))
        .and_then(Value::as_array)
        .and_then(|arr| arr.first());
    let attachment_link = attachment_link(attachment);

    let paragraphs = split_paragraphs(message);
    let mut content_nodes: Vec<Value> = paragraphs.iter().map(|p| tiptap_paragraph(p)).collect();
    if let Some((href, label)) = &attachment_link {
        content_nodes.push(json!({
            "type": "paragraph",
            "content": [{
                "type": "text",
                "text": label,
                "marks": [{ "type": "link", "attrs": { "href": href } }],
            }],
        }));
    }

    let title = derive_title(&paragraphs, created_time.as_deref());
    let summary: String = paragraphs.first().map(|p| truncate_chars(p, SUMMARY_MAX_CHARS)).unwrap_or_default();

    let mut contents = Map::new();
    contents.insert("content".into(), json!({ "type": "doc", "content": content_nodes }));
    contents.insert("text".into(), json!(paragraphs.join("\n\n")));
    contents.insert("paragraphs".into(), json!(paragraphs));
    contents.insert("comments".into(), json!(comments));
    if let Some(icon) = icon {
        contents.insert("icon".into(), json!(icon));
    }
    contents.insert(
        "source".into(),
        json!({
            "provider": "facebook",
            "post_id": post_id,
            "page_id": page_id,
            "page_name": page_name,
            "permalink_url": permalink,
            "picture_url": post.get("full_picture"),
            "status_type": post.get("status_type"),
            "created_time": post.get("created_time"),
            "updated_time": post.get("updated_time"),
        }),
    );

    let mut links = Vec::new();
    if let Some(url) = permalink {
        links.push(json!({ "kind": "url", "target": url, "title": "View on Facebook" }));
    }
    if let Some((href, label)) = &attachment_link {
        links.push(json!({ "kind": "url", "target": href, "title": label }));
    }

    let path = path.map(String::from).unwrap_or_else(|| default_path(page_name, page_id));

    json!({
        "path": path,
        "name": sanitize_name(post_id),
        "title": title,
        "summary": summary,
        "typeRef": TYPE_REF,
        "contents": Value::Object(contents),
        "author": page_name,
        "pubDate": created_time,
        "links": links,
    })
}

/// `/blogs/facebook/<page>`, mirroring solx-livejournal's
/// `/blogs/livejournal/<user>`.
fn default_path(page_name: Option<&str>, page_id: Option<&str>) -> String {
    let segment = page_name
        .map(slugify)
        .filter(|s| !s.is_empty())
        .or_else(|| page_id.map(sanitize_name))
        .unwrap_or_else(|| "page".to_string());
    format!("/blogs/facebook/{segment}")
}

/// The post's shared link, if any, as `(href, label)`. Prefers
/// `unshimmed_url` (the real destination) over `url`, which for shared
/// links is an l.facebook.com redirect. Photo/video attachments link back
/// to Facebook itself and are already covered by the permalink, so only
/// `share` attachments count.
fn attachment_link(attachment: Option<&Value>) -> Option<(String, String)> {
    let att = attachment?;
    if att.get("type").and_then(Value::as_str) != Some("share") {
        return None;
    }
    let href = att
        .get("unshimmed_url")
        .and_then(Value::as_str)
        .or_else(|| att.get("url").and_then(Value::as_str))?
        .to_string();
    let label = att
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
        .unwrap_or_else(|| href.clone());
    Some((href, label))
}

/// Splits a post message on blank lines, keeping single newlines inside a
/// paragraph (rendered as Tiptap hard breaks).
fn split_paragraphs(message: &str) -> Vec<String> {
    let normalized = message.replace("\r\n", "\n");
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in normalized.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                out.push(current.join("\n").trim().to_string());
                current.clear();
            }
        } else {
            current.push(line.trim_end());
        }
    }
    if !current.is_empty() {
        out.push(current.join("\n").trim().to_string());
    }
    out
}

fn tiptap_paragraph(text: &str) -> Value {
    let mut nodes = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            nodes.push(json!({ "type": "hardBreak" }));
        }
        if !line.is_empty() {
            nodes.push(json!({ "type": "text", "text": line }));
        }
    }
    json!({ "type": "paragraph", "content": nodes })
}

/// Facebook posts have no title: use the first line of the message, or the
/// date for a picture-only post.
fn derive_title(paragraphs: &[String], created_time: Option<&str>) -> String {
    if let Some(first_line) = paragraphs.first().and_then(|p| p.lines().next()) {
        let line = first_line.trim();
        if !line.is_empty() {
            return truncate_chars(line, TITLE_MAX_CHARS);
        }
    }
    match created_time.and_then(|t| t.get(..10)) {
        Some(date) => format!("Facebook post {date}"),
        None => "Facebook post".to_string(),
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// Graph API timestamps look like `2026-09-28T18:07:44+0000`; ISO 8601 /
/// RFC 3339 consumers want `+00:00`. Anything else passes through.
fn normalize_graph_time(t: &str) -> String {
    let bytes = t.as_bytes();
    let n = bytes.len();
    if n >= 5
        && (bytes[n - 5] == b'+' || bytes[n - 5] == b'-')
        && bytes[n - 4..].iter().all(u8::is_ascii_digit)
    {
        format!("{}:{}", &t[..n - 2], &t[n - 2..])
    } else {
        t.to_string()
    }
}

/// Lowercase ASCII slug for path segments (`My Page!` → `my-page`).
fn slugify(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(60).collect()
}

/// Keeps a Graph id (`<page_id>_<post_id>`) usable as a document name /
/// file name: ASCII alphanumerics, `_` and `-` only.
fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect()
}

// ── Host calls ───────────────────────────────────────────────────────────────

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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_time_gets_a_colon_in_its_offset() {
        assert_eq!(normalize_graph_time("2026-09-28T18:07:44+0000"), "2026-09-28T18:07:44+00:00");
        assert_eq!(normalize_graph_time("2026-09-28T18:07:44-0530"), "2026-09-28T18:07:44-05:30");
        assert_eq!(normalize_graph_time("2026-09-28T18:07:44Z"), "2026-09-28T18:07:44Z");
    }

    #[test]
    fn paragraphs_split_on_blank_lines_and_keep_single_newlines() {
        let paras = split_paragraphs("Line one\nline two\n\n\nSecond para  \r\n");
        assert_eq!(paras, vec!["Line one\nline two".to_string(), "Second para".to_string()]);
        let node = tiptap_paragraph(&paras[0]);
        assert_eq!(node["content"][1]["type"], "hardBreak");
    }

    #[test]
    fn title_falls_back_to_the_date_for_picture_only_posts() {
        assert_eq!(derive_title(&[], Some("2026-09-28T18:07:44+00:00")), "Facebook post 2026-09-28");
        let long = "x".repeat(200);
        assert_eq!(derive_title(&[long], None).chars().count(), TITLE_MAX_CHARS);
    }

    #[test]
    fn payload_maps_post_fields_onto_blog_post_with_comments() {
        let post = json!({
            "id": "111_222",
            "message": "Hello world\n\nMore text",
            "created_time": "2026-09-28T18:07:44+0000",
            "permalink_url": "https://www.facebook.com/111/posts/222",
            "from": { "id": "111", "name": "My Test Page" },
            "attachments": { "data": [{ "type": "share", "title": "Example", "url": "https://l.facebook.com/x", "unshimmed_url": "https://example.com/" }] },
            "comments": { "data": [{
                "message": "Nice", "created_time": "2026-09-28T19:00:00+0000", "from": { "name": "Ann" },
                "comments": { "data": [{ "message": "Thanks", "created_time": "2026-09-28T19:05:00+0000" }] }
            }] }
        });
        let comments: Vec<Value> = post["comments"]["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(embedded_comment_to_blog_comment)
            .collect();
        let payload = build_document_payload(&post, comments, Some("files/docs/shared/fb-111_222-picture.jpg"), None);

        assert_eq!(payload["path"], "/blogs/facebook/my-test-page");
        assert_eq!(payload["name"], "111_222");
        assert_eq!(payload["title"], "Hello world");
        assert_eq!(payload["typeRef"], TYPE_REF);
        assert_eq!(payload["pubDate"], "2026-09-28T18:07:44+00:00");
        assert_eq!(payload["contents"]["text"], "Hello world\n\nMore text");
        assert_eq!(payload["contents"]["icon"], "files/docs/shared/fb-111_222-picture.jpg");
        // Two message paragraphs + the attachment link paragraph.
        assert_eq!(payload["contents"]["content"]["content"].as_array().unwrap().len(), 3);
        assert_eq!(payload["contents"]["comments"][0]["author"], "Ann");
        assert_eq!(payload["contents"]["comments"][0]["replies"][0]["author"], Value::Null);
        assert_eq!(count_comments(&payload["contents"]["comments"]), 2);
        assert_eq!(payload["links"][1]["target"], "https://example.com/");
    }
}
