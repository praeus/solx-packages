//! Issue an endpoint call through `/builtin/http_request` and turn the
//! response into an [`Outcome`].
//!
//! `http_request` treats a non-2xx as a *successful call that returned a
//! status* — it only errors on transport failure. That split is preserved
//! here as two distinct failure kinds (`transport` vs `http_status`), because
//! they mean very different things to a caller: one says the server is
//! unreachable, the other says the server rejected the request.

use serde_json::{json, Map, Value};

use crate::config;
use crate::endpoint::{self, Endpoint, LEGACY_EMBED_PATH};
use crate::host::{truncate, Host, Outcome};

/// Longest error body echoed back in the failure output.
const MAX_BODY_ECHO: usize = 2048;
/// Longest body used as an error *message* when Ollama sent no `error` field.
const MAX_DETAIL_ECHO: usize = 512;

pub fn call(host: &dyn Host, ep: &Endpoint, params: &Value) -> Outcome {
    let mut body = match endpoint::build_body(ep, params) {
        Ok(b) => b,
        Err(missing) => {
            return Outcome::fail(
                "bad_params",
                format!("{} requires: {}", ep.fn_name, missing.join(", ")),
                json!({ "missing": missing }),
            )
        }
    };

    // `legacy: true` on `embed` retargets the older single-prompt endpoint,
    // which names the field `prompt` and accepts only a string.
    let mut path = ep.path;
    if ep.fn_name == "embed" && params.get("legacy").and_then(Value::as_bool) == Some(true) {
        match rewrite_legacy_embed(&mut body) {
            Ok(()) => path = LEGACY_EMBED_PATH,
            Err(e) => return Outcome::fail("bad_params", e, json!({})),
        }
    }

    let base = config::resolve_base_url(host, params);
    let url = format!("{base}{path}");

    let mut headers = Map::new();
    if body.is_some() {
        headers.insert("content-type".to_string(), json!("application/json"));
    }
    // Caller headers go in before Authorization so our bearer wins a conflict
    // rather than being silently overwritten by a stale one.
    if let Some(h) = params.get("headers").and_then(Value::as_object) {
        for (k, v) in h {
            if v.is_string() {
                headers.insert(k.to_ascii_lowercase(), v.clone());
            }
        }
    }
    match config::resolve_auth(host, params) {
        Ok(Some(token)) => {
            headers.insert("authorization".to_string(), json!(format!("Bearer {token}")));
        }
        Ok(None) => {}
        Err(e) => return Outcome::fail("auth", e, json!({})),
    }

    let timeout = params
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(ep.default_timeout)
        .clamp(1, ep.max_timeout);

    let mut payload = json!({
        "url": url,
        "method": ep.method,
        "headers": Value::Object(headers),
        "timeout_secs": timeout,
    });
    if let Some(b) = &body {
        payload["body"] = json!(b.to_string());
        payload["body_encoding"] = json!("utf8");
    }

    host.log(&format!(
        "solx-ollama {} {url} (timeout {timeout}s)",
        ep.method
    ));

    let call = match host.exec("/builtin/http_request", &payload) {
        Ok(c) => c,
        Err(e) => {
            return Outcome::fail(
                "transport",
                format!("{} {url} failed: {e}", ep.method),
                json!({ "url": url, "method": ep.method }),
            )
        }
    };
    if !call.success {
        return Outcome::fail(
            "transport",
            format!(
                "{} {url} failed: {}",
                ep.method,
                call.message.unwrap_or_else(|| "no message".to_string())
            ),
            json!({ "url": url, "method": ep.method }),
        );
    }

    interpret(ep, path, &url, &call.result)
}

/// `/api/embeddings` takes `prompt` (a single string) where `/api/embed`
/// takes `input` (string or array).
fn rewrite_legacy_embed(body: &mut Option<Value>) -> Result<(), String> {
    let Some(obj) = body.as_mut().and_then(Value::as_object_mut) else {
        return Err("embed: internal error, no request body to rewrite".to_string());
    };
    let input = obj.remove("input").unwrap_or(Value::Null);
    match input {
        Value::String(s) => {
            obj.insert("prompt".to_string(), Value::String(s));
            obj.remove("dimensions");
            obj.remove("truncate");
            Ok(())
        }
        Value::Array(_) => Err(
            "embed: legacy mode targets /api/embeddings, which accepts a single \
             prompt string - pass a string input or drop legacy"
                .to_string(),
        ),
        _ => Err("embed: input must be a string in legacy mode".to_string()),
    }
}

fn interpret(ep: &Endpoint, path: &str, url: &str, resp: &Value) -> Outcome {
    let status = resp.get("status").and_then(Value::as_u64).unwrap_or(0);

    // A base64 body means the bytes were not valid UTF-8, so they are
    // certainly not the JSON we expect. Report it rather than silently
    // returning null.
    if resp.get("body_encoding").and_then(Value::as_str) == Some("base64") {
        return Outcome::fail(
            "non_utf8",
            format!("{url} returned a non-UTF-8 body (status {status})"),
            json!({ "status": status, "url": url }),
        );
    }

    let raw = resp.get("body").and_then(Value::as_str).unwrap_or("");
    let parsed: Value = if raw.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(raw).unwrap_or(Value::Null)
    };

    if !(200..300).contains(&status) {
        let detail = parsed
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| truncate(raw, MAX_DETAIL_ECHO));
        return Outcome::fail(
            "http_status",
            format!("ollama {} {path} returned {status}: {detail}", ep.method),
            json!({
                "status": status,
                "url": url,
                "body": truncate(raw, MAX_BODY_ECHO),
            }),
        );
    }

    // Ollama sometimes answers 200 with an error object in the body.
    if let Some(e) = parsed.get("error").and_then(Value::as_str) {
        return Outcome::fail(
            "ollama_error",
            format!("ollama {} {path}: {e}", ep.method),
            json!({ "status": status, "url": url }),
        );
    }

    // /api/copy and /api/delete answer 200 with an empty body.
    Outcome::ok(if parsed.is_null() {
        json!({ "status": "success" })
    } else {
        parsed
    })
}
